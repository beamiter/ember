//! Nonmodal, read-only inspection of the exact bounded startup snapshot.
//! Original bytes are private, never logged, uploaded, or executed.
use crate::session_persistence::{
    inspect_recovery, RepairSummary, SessionsSnapshot, MAX_SESSION_SNAPSHOT_BYTES,
};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

pub(crate) struct SessionRecovery {
    pub open: bool,
    pub summary: Option<RepairSummary>,
    original: Option<Arc<[u8]>>,
    source: PathBuf,
    export_path: String,
    notice: String,
    pending: Option<mpsc::Receiver<Result<String, String>>>,
}

impl SessionRecovery {
    fn new(source: &Path, original: Option<Arc<[u8]>>, summary: Option<RepairSummary>) -> Self {
        Self {
            open: true,
            summary,
            original,
            source: source.to_owned(),
            export_path: String::new(),
            notice: String::new(),
            pending: None,
        }
    }

    pub fn load(path: &Path) -> (Result<Option<SessionsSnapshot>, String>, Option<Self>) {
        let revision =
            match crate::persistence_file::read_revision(path, MAX_SESSION_SNAPSHOT_BYTES) {
                Ok(revision) => revision,
                Err(error) => return (Err(error.to_string()), Some(Self::new(path, None, None))),
            };
        let Some(bytes) = revision.bytes() else {
            return (Ok(None), None);
        };
        let inspected = std::str::from_utf8(bytes)
            .map_err(|_| "session snapshot is not valid UTF-8".to_string())
            .and_then(|content| {
                inspect_recovery(content).map_err(|error| {
                    match error.classify() {
                        serde_json::error::Category::Syntax => {
                            "session snapshot has invalid JSON syntax"
                        }
                        serde_json::error::Category::Data => {
                            "session snapshot contains invalid data"
                        }
                        serde_json::error::Category::Eof => "session snapshot JSON is incomplete",
                        serde_json::error::Category::Io => "session snapshot could not be decoded",
                    }
                    .to_string()
                })
            });
        match inspected {
            Ok((snapshot, summary)) => {
                let recovery = (!summary.is_empty())
                    .then(|| Self::new(path, Some(Arc::from(bytes)), Some(summary)));
                (
                    Ok((!snapshot.sessions.is_empty()).then_some(snapshot)),
                    recovery,
                )
            }
            Err(error) => (
                Err(error),
                Some(Self::new(path, Some(Arc::from(bytes)), None)),
            ),
        }
    }

    fn recheck(&mut self) {
        let Some(original) = &self.original else {
            return;
        };
        // Deliberately retain the startup report. A later recheck is a preview,
        // not evidence that any of the live sessions have been restored.
        self.notice = match std::str::from_utf8(original).ok().and_then(|text| inspect_recovery(text).ok()) {
            Some((snapshot, summary)) => format!("Recheck: {} session records readable; {} skipped; {} unavailable or invalid directories. Live sessions unchanged.", snapshot.sessions.len(), summary.skipped_sessions, summary.invalid_cwds.iter().filter(|value| **value).count()),
            None => "Recheck: original metadata is still unreadable. Live sessions unchanged.".into(),
        };
    }

    fn start_export(&mut self, ctx: &egui::Context) {
        let Some(original) = self.original.clone() else {
            return;
        };
        let path = PathBuf::from(&self.export_path);
        if !path.is_absolute()
            || path == self.source
            || self.export_path.len() > 4096
            || self.export_path.chars().any(char::is_control)
        {
            self.notice =
                "Choose a new absolute local path, different from the original snapshot.".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        self.pending = Some(rx);
        std::thread::spawn(move || {
            let result = export_original(&path, &original)
                .map(|()| "Original exported locally. It may contain private session data.".into())
                .map_err(|error| format!("Export did not complete: {error}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(result) => {
                    self.notice =
                        crate::review_text::bound_toast_text(result.unwrap_or_else(|error| error));
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.notice = "Export worker stopped before reporting completion. Check the chosen local file before trying again.".into();
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.open {
            egui::Area::new(egui::Id::new("session_recovery_reopen"))
                .anchor(egui::Align2::LEFT_BOTTOM, [12.0, -12.0])
                .show(ctx, |ui| {
                    if ui.button("Session recovery details").clicked() {
                        self.open = true;
                    }
                });
            return;
        }
        let mut open = self.open;
        let mut dismiss = false;
        egui::Window::new("Session recovery summary").id(egui::Id::new("session_recovery"))
            .open(&mut open).default_width(450.0).vscroll(true).show(ctx, |ui| {
                ui.label("Startup metadata needs review. Current terminals remain usable.");
                if let Some(summary) = &self.summary {
                    ui.label(format!("{} session records retained; {} skipped above the 64-session limit.", summary.retained_sessions, summary.skipped_sessions));
                    if summary.skipped_sessions > 0 { ui.label(format!("Skipped original session positions: 65–{}.", 64 + summary.skipped_sessions)); }
                    if summary.layout_adjusted { ui.label("Saved pane layout adjusted: invalid or oversized layout references. Retained sessions use the safe available layout."); }
                    if summary.selection_adjusted { ui.label("Saved active selection adjusted to the retained list."); }
                    for index in 0..summary.retained_sessions {
                        if summary.session_fields[index] > 0 { ui.label(format!("Session {}: {} invalid or oversized metadata field(s) adjusted.", index + 1, summary.session_fields[index])); }
                        if summary.invalid_cwds[index] { ui.label(format!("Session {}: saved directory invalid or unavailable; default directory used.", index + 1)); }
                    }
                } else {
                    ui.label("Original metadata could not be loaded safely. Affected session count is unknown. Existing startup quarantine and save protections remain in effect.");
                }
                ui.separator();
                ui.label("Recheck only validates the captured original metadata. It does not run commands, replace live sessions, or write the snapshot.");
                if ui.add_enabled(self.original.is_some(), egui::Button::new("Recheck original metadata")).clicked() { self.recheck(); }
                ui.label("Export copies the captured original to a NEW local file. It may contain private names, paths, and other session data. Nothing is uploaded.");
                ui.add(egui::TextEdit::singleline(&mut self.export_path).hint_text("New absolute export path").char_limit(4096));
                if ui.add_enabled(self.original.is_some() && self.pending.is_none(), egui::Button::new("Export original locally")).clicked() { self.start_export(ctx); }
                if self.original.is_none() { ui.label("Original export unavailable: startup could not safely read the file within its size and ownership limits."); }
                if self.pending.is_some() { ui.label("Exporting…"); }
                if !self.notice.is_empty() { ui.label(&self.notice); }
                ui.label("Closing this summary keeps the original metadata available for this app session.");
                dismiss = ui.button("Dismiss summary").clicked();
            });
        self.open = open && !dismiss;
    }
}

fn export_original(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::persistence_file::write_new_private_file(path, bytes, MAX_SESSION_SNAPSHOT_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn temporary(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ember-recovery-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn fixture(count: usize) -> serde_json::Value {
        serde_json::json!({"version":4,"sessions":(0..count).map(|index| serde_json::json!({"name":format!("private-{index}"),"tags":[],"session_id":format!("session-{index}")})).collect::<Vec<_>>()})
    }
    fn load_fixture(value: &serde_json::Value) -> (PathBuf, Option<SessionRecovery>) {
        let path = temporary("snapshot.json");
        crate::persistence_file::write_atomic(&path, &serde_json::to_vec(value).unwrap()).unwrap();
        let (loaded, recovery) = SessionRecovery::load(&path);
        assert!(loaded.is_ok());
        (path, recovery)
    }
    #[test]
    fn normal_startup_is_quiet_and_over_limit_is_exact() {
        let (path, recovery) = load_fixture(&fixture(64));
        assert!(recovery.is_none());
        std::fs::remove_file(path).unwrap();
        let (path, recovery) = load_fixture(&fixture(67));
        let recovery = recovery.unwrap();
        let summary = recovery.summary.as_ref().unwrap();
        assert_eq!(summary.retained_sessions, 64);
        assert_eq!(summary.skipped_sessions, 3);
        assert!(!summary.layout_adjusted);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn invalid_layout_and_cwd_have_structured_causes() {
        let mut value = fixture(2);
        value["tabs"] = serde_json::json!([{"root":{"kind":"unknown"}}]);
        value["sessions"][0]["cwd"] = serde_json::json!("bad\npath");
        value["sessions"][1]["cwd"] = serde_json::json!(temporary("missing-directory"));
        let (path, recovery) = load_fixture(&value);
        let summary = recovery.unwrap().summary.unwrap();
        assert!(summary.layout_adjusted);
        assert_eq!(summary.retained_sessions, 2);
        assert_eq!(summary.skipped_sessions, 0);
        assert_eq!(summary.invalid_cwds.iter().filter(|x| **x).count(), 2);
        assert_eq!(summary.session_fields[0], 1);
        assert_eq!(summary.session_fields[1], 0);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn dismiss_recheck_and_export_retain_exact_original() {
        let mut value = fixture(1);
        value["sessions"][0]["cwd"] = serde_json::json!(temporary("missing-directory"));
        let (path, recovery) = load_fixture(&value);
        let bytes = std::fs::read(&path).unwrap();
        let mut recovery = recovery.unwrap();
        let report = recovery.summary.clone();
        recovery.open = false;
        // Simulate a subsequent normal save: export must still be the exact
        // bounded bytes captured at startup, not a freshly read replacement.
        crate::persistence_file::write_atomic(&path, b"replacement").unwrap();
        recovery.recheck();
        assert_eq!(report, recovery.summary);
        assert_eq!(recovery.original.as_deref().unwrap(), &bytes);
        assert!(!recovery.open);
        let output = temporary("export.json");
        export_original(&output, recovery.original.as_deref().unwrap()).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), bytes);
        assert!(export_original(&output, b"overwrite").is_err());
        assert_eq!(std::fs::read(&output).unwrap(), bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(output).unwrap();
    }
    #[test]
    fn decode_failure_diagnostics_never_echo_session_data() {
        let path = temporary("private-invalid-version");
        crate::persistence_file::write_atomic(
            &path,
            br#"{"version":"PRIVATE_SECRET_SENTINEL","sessions":[]}"#,
        )
        .unwrap();
        let (result, recovery) = SessionRecovery::load(&path);
        let error = result.unwrap_err();
        assert_eq!(error, "session snapshot contains invalid data");
        // The startup diagnostic only receives this categorical error, not
        // the serde error that can contain quoted private input values.
        let diagnostic =
            crate::review_text::bound_toast_text(format!("Session restore failed: {error}"));
        assert!(!diagnostic.contains("PRIVATE_SECRET_SENTINEL"));
        let mut recovery = recovery.unwrap();
        assert!(recovery.summary.is_none());
        recovery.recheck();
        assert!(!recovery.notice.contains("PRIVATE_SECRET_SENTINEL"));
        assert_eq!(
            recovery.notice,
            "Recheck: original metadata is still unreadable. Live sessions unchanged."
        );
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| recovery.show(ui.ctx()));
        output.textures_delta.clear();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn corrupt_utf8_remains_exportable_but_unknown_count() {
        let path = temporary("invalid-utf8");
        crate::persistence_file::write_atomic(&path, &[0xff, 0xfe]).unwrap();
        let (result, recovery) = SessionRecovery::load(&path);
        assert!(result.is_err());
        let recovery = recovery.unwrap();
        assert!(recovery.summary.is_none());
        assert_eq!(recovery.original.as_deref().unwrap(), &[0xff, 0xfe]);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    #[cfg(unix)]
    fn unsafe_sources_and_destinations_are_not_followed() {
        use std::os::unix::fs::symlink;
        let path = temporary("original");
        let link = temporary("symlink");
        crate::persistence_file::write_atomic(&path, b"{}").unwrap();
        symlink(&path, &link).unwrap();
        let (result, recovery) = SessionRecovery::load(&link);
        assert!(result.is_err());
        assert!(recovery.unwrap().original.is_none());
        assert!(export_original(&link, b"overwrite").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        std::fs::remove_file(link).unwrap();
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn recovery_panel_renders_nonmodal_and_dismissal_reopens() {
        let mut value = fixture(1);
        value["tabs"] = serde_json::json!([{"root":{"kind":"unknown"}}]);
        let (path, recovery) = load_fixture(&value);
        let mut recovery = recovery.unwrap();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| recovery.show(ui.ctx()));
        output.textures_delta.clear();
        recovery.open = false;
        let mut output = ctx.run_ui(Default::default(), |ui| recovery.show(ui.ctx()));
        output.textures_delta.clear();
        assert!(recovery.original.is_some());
        assert!(recovery.summary.as_ref().unwrap().layout_adjusted);
        std::fs::remove_file(path).unwrap();
    }
}
