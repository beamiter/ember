//! Recover the parenthesized destinations that a TUI may wrap before printing.
use std::ops::Range;
use std::sync::LazyLock;

const MAX_BYTES: usize = 4096;

pub(crate) struct Reference {
    pub target: String,
    pub web: bool,
    pub ambiguous: bool,
    pub spans: Vec<(usize, Range<usize>)>,
}

pub(crate) fn detect(rows: &[(&str, bool)], soft_wraps: &[bool]) -> Vec<Reference> {
    static DESTINATION: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"\(((?:(?i:https?)://|(?:\.{0,2}|~)/|[\p{L}\p{N}_][\p{L}\p{N}\p{M}_.-]*/)[^()\r\n]*(?:\n[^()\r\n]+){0,7})\)",
        )
        .unwrap()
    });
    static FILE_END: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i-u:\.(?:canvas\.tsx|tsx|jsx|mdx?|txt|rs|py|jsonc?|toml|ya?ml|css|html|svg|png|jpe?g|webp|ts|js|pdf|csv|sh))(?::[0-9]+){0,2}$",
        )
        .unwrap()
    });

    if !rows.iter().any(|(text, _)| text.contains('(')) {
        return Vec::new();
    }
    let mut text = String::new();
    let mut row_starts = Vec::with_capacity(rows.len());
    for (row, _) in rows {
        row_starts.push(text.len());
        text.push_str(row);
        text.push('\n');
    }

    let mut references = Vec::new();
    for matched in DESTINATION.captures_iter(&text) {
        let raw = matched.get(1).unwrap();
        if raw.len() > MAX_BYTES {
            continue;
        }
        let first_row = row_starts.partition_point(|&start| start <= raw.start()) - 1;
        let raw_web = raw.as_str().split_once("://").is_some_and(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        });
        if !raw_web {
            // Physical terminal spaces are filename bytes, never indentation.
            // Only authoritative soft-wrap metadata permits joining file rows.
            let mut target = String::new();
            let mut spans = Vec::new();
            let mut ambiguous = false;
            for (index, fragment) in raw.as_str().split('\n').enumerate() {
                let row = first_row + index;
                let start = if index == 0 {
                    raw.start() - row_starts[row]
                } else {
                    0
                };
                if fragment.is_empty() {
                    break;
                }
                spans.push((row, start..start + fragment.len()));
                target.push_str(fragment);
                if index > 0 && (soft_wraps.len() != rows.len() || !soft_wraps[row - 1]) {
                    ambiguous = true;
                }
            }
            // Suppress every fragment of an ambiguous physical-line group,
            // even if its suffix would not itself qualify for preview.
            if ambiguous && spans.len() == raw.as_str().split('\n').count() {
                references.push(Reference {
                    target,
                    web: false,
                    ambiguous: true,
                    spans,
                });
                continue;
            }
            if spans.len() != raw.as_str().split('\n').count() {
                continue;
            }
            if spans.len() == 1 && !target.contains(char::is_whitespace) {
                continue;
            }
            let ambiguous = target.contains("://")
                || target.contains(['\\', '<', '>', '|'])
                || !FILE_END.is_match(&target);
            references.push(Reference {
                target,
                web: false,
                ambiguous,
                spans,
            });
            continue;
        }
        // Never infer a URL by trimming physical lines. Keep the complete
        // range as a blocker so the ordinary detector cannot open fragments.
        if raw.as_str().contains('\n') {
            let spans = raw
                .as_str()
                .split('\n')
                .enumerate()
                .map(|(index, fragment)| {
                    let start = if index == 0 {
                        raw.start() - row_starts[first_row]
                    } else {
                        0
                    };
                    (first_row + index, start..start + fragment.len())
                })
                .collect();
            references.push(Reference {
                target: raw.as_str().to_owned(),
                web: true,
                ambiguous: true,
                spans,
            });
        }
    }
    references
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_delimited_paths_with_wide_text_and_real_spaces() {
        let rows = [
            ("路线图 (/tmp/中 文/PMG-SOTA-", false),
            (
                "    roadmap.canvas.tsx) · 报告 (reports/接触 优化.md)",
                false,
            ),
        ];
        let references = detect(&rows, &[]);
        assert_eq!(references.len(), 2);
        assert_eq!(
            references[0].target,
            "/tmp/中 文/PMG-SOTA-    roadmap.canvas.tsx"
        );
        assert!(references[0].ambiguous);
        assert_eq!(references[0].spans[1], (1, 0..22));
        assert_eq!(references[1].target, "reports/接触 优化.md");
    }

    #[test]
    fn joins_only_with_a_supported_boundary_and_delimiter() {
        assert!(detect(&[("(/tmp/first", false), ("second.md)", false)], &[])[0].ambiguous);
        assert!(detect(&[("(/tmp/first-", false), ("second.md", false)], &[]).is_empty());
        let rejected = detect(&[("(https://user@host/a-", false), ("b)", false)], &[]);
        assert!(!jterm_core::link::is_openable_url(&rejected[0].target));
        let links = detect(
            &[("(https://example.org/one-", false), ("two)", false)],
            &[],
        );
        assert!(links[0].web);
        assert!(links[0].ambiguous);
        assert_eq!(links[0].target, "https://example.org/one-\ntwo");
    }
}
