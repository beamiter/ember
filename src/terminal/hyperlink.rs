use jterm_core::link::is_openable_osc8;

#[cfg(test)]
pub(super) use jterm_engine::hyperlink::{MAX_OSC8_PARAMS_BYTES, MAX_OSC8_URI_BYTES};
#[cfg(test)]
const MAX_HYPERLINKS: usize = 4096;

pub use jterm_engine::HyperlinkId;

/// Return whether an OSC 8 target may become a click action.
///
/// The same policy admits OSC 8 metadata and resolves user click actions.
pub(crate) fn is_supported_hyperlink_uri(uri: &str) -> bool {
    is_openable_osc8("", uri)
}

#[derive(Debug, Default)]
pub(super) struct HyperlinkTable(jterm_engine::hyperlink::HyperlinkTable);

impl HyperlinkTable {
    #[cfg(test)]
    pub(super) fn intern(&mut self, params: &str, uri: &str) -> Option<HyperlinkId> {
        self.0.intern(params, uri, is_openable_osc8)
    }

    pub(super) fn apply_osc8(&mut self, fields: &str) -> HyperlinkId {
        self.0.apply_osc8(fields, is_openable_osc8)
    }

    pub(super) fn resolve(&self, id: HyperlinkId) -> Option<&str> {
        self.0.resolve(id)
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
            "https://example.com/\u{fffd}",
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
            .intern("id=\u{202e}spoof", "https://safe.test")
            .is_none());
        assert!(table.intern("id=ok\u{fffd}", "https://safe.test").is_none());
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
