use std::collections::HashMap;
use std::sync::Arc;

/// Maximum OSC 8 parameter payload accepted from an attached process.
pub(super) const MAX_OSC8_PARAMS_BYTES: usize = 256;
/// Maximum OSC 8 target accepted from an attached process.
pub(super) const MAX_OSC8_URI_BYTES: usize = 2 * 1024;
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

/// Return whether an OSC 8 target is safe for the platform URL opener.
///
/// Keeping this as an allow-list makes an accidental construction of a
/// `Link` elsewhere unable to turn `javascript:`, `data:`, or shell-like
/// schemes into a click action.
pub(crate) fn is_supported_hyperlink_uri(uri: &str) -> bool {
    if uri.is_empty()
        || uri.len() > MAX_OSC8_URI_BYTES
        || uri
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return false;
    }

    let Some((scheme, _rest)) = uri.split_once(':') else {
        return false;
    };
    if scheme.is_empty()
        || !scheme.as_bytes()[0].is_ascii_alphabetic()
        || !scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    {
        return false;
    }

    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "ftp" | "ftps" | "git" | "ssh" | "mailto" | "file"
    )
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
    fn validates_supported_schemes_case_insensitively() {
        assert!(is_supported_hyperlink_uri("HTTPS://example.com/path"));
        assert!(is_supported_hyperlink_uri("mailto:person@example.com"));
        assert!(is_supported_hyperlink_uri("ssh://host.example/path"));
        assert!(is_supported_hyperlink_uri("git://host.example/repository"));
        assert!(!is_supported_hyperlink_uri("javascript:alert(1)"));
        assert!(!is_supported_hyperlink_uri("data:text/html,hello"));
        assert!(!is_supported_hyperlink_uri("relative/path"));
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
