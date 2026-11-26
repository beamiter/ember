//! Frozen native OSC5522 policies before shared owner adoption.
use super::*;
impl TerminalState {
    fn frozen_osc5522(&mut self, metadata: &str, payload: Option<&str>) {
        crate::debug_log!("[OSC5522] metadata={} payload={:?}", metadata, payload);

        let mut message_type = None;
        let mut password = None;
        let mut human_name = None;

        for part in metadata.split(':') {
            if let Some(value) = part.strip_prefix("type=") {
                message_type = Some(value);
            } else if let Some(value) = part.strip_prefix("pw=") {
                password = Self::decode_base64(value);
            } else if let Some(value) = part.strip_prefix("name=") {
                human_name = Self::decode_base64(value);
            }
        }

        if message_type != Some("read") {
            return;
        }

        // Per the OSC 5522 protocol, the third field is base64-encoded and is
        // either "." (list types) or a space-separated MIME request. This
        // implementation intentionally supports one MIME per paste grant for
        // now; accepting arbitrary direct reads would require a permission UI.
        let Some(request) = payload.and_then(Self::decode_base64) else {
            self.append_osc_5522_status("type=read:status=EPERM", None);
            return;
        };

        if request == "." {
            // Clipboard type enumeration is itself a host read. ember sends
            // the sanitized MIME list proactively only after an actual user
            // paste, together with a short-lived capability. A PTY-originated
            // discovery request must not spawn host clipboard helpers.
            self.append_osc_5522_status("type=read:status=EPERM", None);
            return;
        }

        let mut requested_mimes = request.split_ascii_whitespace();
        let Some(mime_type) = requested_mimes.next() else {
            self.append_osc_5522_status("type=read:status=EPERM", None);
            return;
        };
        if requested_mimes.next().is_some() || !Self::is_valid_osc_5522_mime(mime_type) {
            self.append_osc_5522_status("type=read:status=EPERM", None);
            return;
        }

        if !self.is_paste_events_enabled() {
            self.osc5522.pending_paste_grant = None;
            self.append_osc_5522_status("type=read:status=EPERM", None);
            return;
        }

        let Some(grant) = self.osc5522.pending_paste_grant.as_ref() else {
            self.append_osc_5522_status("type=read:status=EPERM", None);
            return;
        };
        if std::time::Instant::now() >= grant.expires_at {
            self.osc5522.pending_paste_grant = None;
            self.append_osc_5522_status("type=read:status=EPERM", None);
            return;
        }
        if password.as_deref() != Some(grant.token.as_str())
            || human_name.as_deref() != Some("Paste event")
        {
            self.append_osc_5522_status("type=read:status=EPERM", None);
            return;
        }

        // Consume the capability before any asynchronous clipboard I/O is
        // queued. A valid token is single-use and cannot be replayed or raced.
        let Some(grant) = self.osc5522.pending_paste_grant.take() else {
            self.append_osc_5522_status("type=read:status=EPERM", None);
            return;
        };
        if !grant.offered_mimes.contains(mime_type) {
            self.append_osc_5522_status("type=read:status=EPERM", None);
            return;
        }

        self.queue_clipboard_request(ClipboardReadKind::MimeData(mime_type.to_string()));
    }
    fn queue_clipboard_request(&mut self, kind: ClipboardReadKind) {
        if self.osc5522.pending_clipboard_requests.len() >= MAX_PENDING_CLIPBOARD_REQUESTS {
            self.append_osc_5522_status("type=read:status=EBUSY", None);
            return;
        }
        self.osc5522
            .pending_clipboard_requests
            .push(ClipboardReadRequest { kind });
    }
    fn is_valid_osc_5522_mime(mime: &str) -> bool {
        !mime.is_empty()
            && mime.len() <= MAX_OSC_5522_MIME_LEN
            && mime.is_ascii()
            && !mime
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    }
    fn sanitized_osc_5522_mimes(mime_types: &[String]) -> Vec<String> {
        let mut seen = HashSet::new();
        mime_types
            .iter()
            .filter(|mime| Self::is_valid_osc_5522_mime(mime))
            .filter(|mime| seen.insert((*mime).clone()))
            .take(MAX_OSC_5522_MIME_TYPES)
            .cloned()
            .collect()
    }
    fn build_osc_5522_mime_list(mime_types: &[String], password: Option<&str>) -> Vec<u8> {
        let mut output = Vec::new();

        output.extend_from_slice(b"\x1b]5522;type=read:status=OK");
        if let Some(password) = password {
            let encoded_password =
                base64::engine::general_purpose::STANDARD.encode(password.as_bytes());
            output.extend_from_slice(b":pw=");
            output.extend_from_slice(encoded_password.as_bytes());
        }
        output.extend_from_slice(b"\x1b\\");

        for mime_type in mime_types {
            let encoded_mime =
                base64::engine::general_purpose::STANDARD.encode(mime_type.as_bytes());
            output.extend_from_slice(b"\x1b]5522;type=read:status=DATA:mime=");
            output.extend_from_slice(encoded_mime.as_bytes());
            output.extend_from_slice(b"\x1b\\");
        }

        output.extend_from_slice(b"\x1b]5522;type=read:status=DONE\x1b\\");
        output
    }
    fn frozen_paste_event(&mut self, mime_types: &[String]) -> Vec<u8> {
        if !self.is_paste_events_enabled() {
            self.osc5522.pending_paste_grant = None;
            return Vec::new();
        }

        let mime_types = Self::sanitized_osc_5522_mimes(mime_types);
        let token = uuid::Uuid::new_v4().to_string();
        self.osc5522.pending_paste_grant = Some(PendingPasteGrant {
            token: token.clone(),
            offered_mimes: mime_types.iter().cloned().collect(),
            expires_at: std::time::Instant::now() + OSC_5522_PASTE_GRANT_TTL,
        });
        Self::build_osc_5522_mime_list(&mime_types, Some(&token))
    }
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}
fn grant_snapshot(t: &TerminalState) -> Option<(String, Vec<String>, std::time::Instant)> {
    t.osc5522.pending_paste_grant.as_ref().map(|g| {
        let mut mimes: Vec<_> = g.offered_mimes.iter().cloned().collect();
        mimes.sort();
        (g.token.clone(), mimes, g.expires_at)
    })
}
#[test]
fn osc5522_owner_matches_frozen_ember_policy() {
    for enabled in [false, true] {
        for capacity in [0, 7, 8] {
            for grant in 0..3 {
                let expiry = std::time::Instant::now() + std::time::Duration::from_secs(3600);
                for raw in [
                    None,
                    Some("."),
                    Some("text/plain"),
                    Some("image/png"),
                    Some("text/plain image/png"),
                    Some(""),
                    Some("text plain"),
                ] {
                    for (password, name) in [
                        ("token", "Paste event"),
                        ("wrong", "Paste event"),
                        ("token", "wrong"),
                        ("", ""),
                    ] {
                        let make = || {
                            let mut t = TerminalState::new(8, 3);
                            if enabled {
                                t.vt.protocol.modes_mut().insert(5522);
                            }
                            t.osc_reply_terminator = b"\x07";
                            t.osc5522.pending_clipboard_requests = vec![
                                ClipboardReadRequest {
                                    kind: ClipboardReadKind::MimeData("old".into())
                                };
                                capacity
                            ];
                            if grant != 0 {
                                t.osc5522.pending_paste_grant = Some(PendingPasteGrant {
                                    token: "token".into(),
                                    offered_mimes: ["text/plain".into()].into_iter().collect(),
                                    expires_at: if grant == 1 {
                                        expiry
                                    } else {
                                        expiry - std::time::Duration::from_secs(7200)
                                    },
                                });
                            }
                            t
                        };
                        let mut a = make();
                        let mut e = make();
                        let metadata = format!("type=read:pw={}:name={}", b64(password), b64(name));
                        let payload = raw.map(b64);
                        a.handle_osc_5522(&metadata, payload.as_deref());
                        e.frozen_osc5522(&metadata, payload.as_deref());
                        assert_eq!(a.output_buffer, e.output_buffer);
                        assert_eq!(
                            a.osc5522.pending_clipboard_requests,
                            e.osc5522.pending_clipboard_requests
                        );
                        assert_eq!(grant_snapshot(&a), grant_snapshot(&e));
                    }
                }
            }
        }
    }
}
#[test]
fn osc5522_paste_issue_matches_frozen_ember_wire_and_grant() {
    for enabled in [false, true] {
        let mut a = TerminalState::new(8, 3);
        let mut e = TerminalState::new(8, 3);
        if enabled {
            a.vt.protocol.modes_mut().insert(5522);
            e.vt.protocol.modes_mut().insert(5522);
        }
        let mimes = vec![
            "text/plain".into(),
            "text/plain".into(),
            "text plain".into(),
            "image/png".into(),
        ];
        let a_wire = a.build_paste_event(&mimes);
        let e_wire = e.frozen_paste_event(&mimes);
        let normalize = |wire: Vec<u8>, t: &TerminalState| {
            let wire = String::from_utf8(wire).unwrap();
            t.osc5522
                .pending_paste_grant
                .as_ref()
                .map_or_else(|| wire.clone(), |g| wire.replace(&b64(&g.token), "TOKEN"))
        };
        assert_eq!(normalize(a_wire, &a), normalize(e_wire, &e));
        assert_eq!(
            a.osc5522.pending_paste_grant.is_some(),
            e.osc5522.pending_paste_grant.is_some()
        );
        assert_eq!(
            a.take_clipboard_read_requests(),
            e.take_clipboard_read_requests()
        );
    }
}
