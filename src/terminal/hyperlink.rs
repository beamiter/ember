use std::collections::HashMap;
use std::sync::Arc;

/// Maximum OSC 8 parameter payload accepted from an attached process.
pub(super) const MAX_OSC8_PARAMS_BYTES: usize = 256;
/// Maximum OSC 8 target accepted from an attached process.
#[cfg(test)]
pub(super) const MAX_OSC8_URI_BYTES: usize = jterm_core::link::MAX_OPENABLE_URL_BYTES;
/// Bound hyperlink metadata independently of terminal scrollback size.
const MAX_HYPERLINKS: usize = 4096;

/// Compact reference stored in each terminal cell. Zero means no hyperlink.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HyperlinkId(u16);

impl HyperlinkId {
    pub const NONE: Self = Self(0);

    #[inline]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub(super) const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    #[inline]
    pub(super) const fn as_raw(self) -> u16 {
        self.0
    }
}

/// Return whether an OSC 8 target may become a click action.
///
/// The policy lives in `jterm_core::link::is_openable_url` and is shared by
/// every jterm: an allow-list of exactly one shape, an absolute HTTP(S) URL
/// with an authority and no userinfo. Everything a terminal-controlled string
/// could otherwise reach — `javascript:`, `data:`, `file:` (a click that opens
/// a local file with its default application), `ssh:` and `git:` (a click that
/// starts a network client), or `https://user:token@host` (a credential the
/// user never typed) — fails closed there rather than at the opener.
pub(crate) fn is_supported_hyperlink_uri(uri: &str) -> bool {
    jterm_core::link::is_openable_url(uri)
}

fn params_are_valid(params: &str) -> bool {
    params.len() <= MAX_OSC8_PARAMS_BYTES && !params.chars().any(char::is_control)
}

#[derive(Debug, Default)]
pub(super) struct HyperlinkTable {
    targets: Vec<Arc<str>>,
    ids_by_target: HashMap<Arc<str>, HyperlinkId>,
}

impl HyperlinkTable {
    /// Validate and intern a target. Existing targets remain usable when the
    /// table reaches capacity; only genuinely new entries are refused.
    pub(super) fn intern(&mut self, params: &str, uri: &str) -> Option<HyperlinkId> {
        if !params_are_valid(params) || !is_supported_hyperlink_uri(uri) {
            return None;
        }
        if let Some(id) = self.ids_by_target.get(uri) {
            return Some(*id);
        }
        if self.targets.len() >= MAX_HYPERLINKS {
            return None;
        }

        let target: Arc<str> = Arc::from(uri);
        let id = HyperlinkId::from_raw((self.targets.len() + 1) as u16);
        self.targets.push(Arc::clone(&target));
        self.ids_by_target.insert(target, id);
        Some(id)
    }

    pub(super) fn resolve(&self, id: HyperlinkId) -> Option<&str> {
        let index = usize::from(id.as_raw()).checked_sub(1)?;
        self.targets.get(index).map(AsRef::as_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_http_targets_with_an_authority_become_clickable() {
        assert!(is_supported_hyperlink_uri("HTTPS://example.com/path"));
        assert!(is_supported_hyperlink_uri("http://example.com"));
        assert!(is_supported_hyperlink_uri("https://example.com/a?b=c#d"));

        for rejected in [
            // Schemes that would start a client or open a local file.
            "mailto:person@example.com",
            "ssh://host.example/path",
            "git://host.example/repository",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,hello",
            // Not an absolute URL at all.
            "relative/path",
            "https:/example.com",
            // No authority: resolves against the opener's default, not the
            // origin the target appears to name.
            "https:///path",
            // Userinfo would hand a credential to the opener.
            "https://user:token@example.com/",
            "https://user@example.com/",
            // Ambiguous or invisible characters in the authority.
            "https://exam\u{200b}ple.com/",
            "https://example.com/\u{202e}path",
            "https://example.com/a b",
            "https://example.com\\evil",
        ] {
            assert!(
                !is_supported_hyperlink_uri(rejected),
                "{rejected:?} must not be clickable"
            );
        }
    }

    #[test]
    fn table_deduplicates_targets_and_rejects_invalid_metadata() {
        let mut table = HyperlinkTable::default();
        let first = table.intern("id=one", "https://example.com").unwrap();
        let second = table.intern("id=two", "https://example.com").unwrap();
        assert_eq!(first, second);
        assert_eq!(table.resolve(first), Some("https://example.com"));

        assert!(table
            .intern(&"p".repeat(MAX_OSC8_PARAMS_BYTES + 1), "https://safe.test")
            .is_none());
        assert!(table.intern("id=bad\nparam", "https://safe.test").is_none());
        assert!(table
            .intern(
                "",
                &format!("https://safe.test/{}", "x".repeat(MAX_OSC8_URI_BYTES))
            )
            .is_none());
        assert!(table.intern("", "https://safe.test/\u{7f}").is_none());
    }

    #[test]
    fn table_is_bounded_but_still_resolves_and_deduplicates_when_full() {
        let mut table = HyperlinkTable::default();
        for index in 0..MAX_HYPERLINKS {
            assert!(table
                .intern("", &format!("https://example.test/{index}"))
                .is_some());
        }

        assert!(table.intern("", "https://example.test/overflow").is_none());
        let existing = table.intern("id=again", "https://example.test/0").unwrap();
        assert_eq!(table.resolve(existing), Some("https://example.test/0"));
    }
}
