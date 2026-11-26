//! Original native token traversal/classifiers frozen before shared adoption.
//! These expected spans never call the candidate engine token implementation.
use super::*;

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_extended_token_separator(c: char) -> bool {
    matches!(
        c,
        '/' | '\\' | '.' | ':' | '-' | '~' | '?' | '&' | '=' | '#' | '%' | '+' | '@'
    )
}

fn is_extended_token_char(c: char) -> bool {
    is_word_char(c) || is_extended_token_separator(c)
}

fn is_token_prefix_wrapper(c: char) -> bool {
    matches!(c, '"' | '\'' | '`' | '(' | '[' | '{' | '<')
}

fn is_token_suffix_wrapper(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | '`' | ')' | ']' | '}' | '>' | ',' | ';' | '!'
    )
}

impl TerminalState {
    fn frozen_extended_token_span(
        line: &[TerminalCell],
        start_col: usize,
    ) -> Option<(usize, usize)> {
        let cols = line.len();
        if start_col >= cols {
            return None;
        }

        let start_char = line[start_col].character;
        if !is_extended_token_char(start_char) {
            return None;
        }

        let mut left = start_col;
        while left > 0 {
            let prev = left - 1;
            if line[prev].flags.wide_continuation() {
                left = prev;
                continue;
            }
            if !is_extended_token_char(line[prev].character) {
                break;
            }
            left = prev;
        }

        let mut right = start_col;
        loop {
            let next = if line[right].flags.wide() {
                right + 2
            } else {
                right + 1
            };
            if next >= cols {
                break;
            }
            if line[next].flags.wide_continuation() {
                if next + 1 < cols && is_extended_token_char(line[next + 1].character) {
                    right = next + 1;
                    continue;
                }
                break;
            }
            if !is_extended_token_char(line[next].character) {
                break;
            }
            right = next;
        }

        while left < start_col && is_token_prefix_wrapper(line[left].character) {
            left += 1;
        }

        while right > start_col && is_token_suffix_wrapper(line[right].character) {
            right -= if line[right].flags.wide_continuation() && right > 0 {
                2
            } else {
                1
            };
        }

        if left > right || start_col < left || start_col > right {
            return None;
        }

        let mut has_alnum = false;
        let mut has_separator = false;
        for cell in &line[left..=right] {
            if cell.flags.wide_continuation() {
                continue;
            }
            let ch = cell.character;
            has_alnum |= ch.is_alphanumeric();
            has_separator |= is_extended_token_separator(ch);
        }

        if !has_alnum || !has_separator {
            return None;
        }

        if line[right].flags.wide() && right + 1 < cols {
            right += 1;
        }

        Some((left, right))
    }
}

fn physical_cells(text: &str) -> Vec<TerminalCell> {
    let mut row = Vec::new();
    for ch in text.chars() {
        let wide = crate::char_width::cached_char_width(ch) == 2;
        let mut cell = TerminalCell {
            character: ch,
            ..TerminalCell::default()
        };
        cell.flags.set_wide(wide);
        cell.flags.set_bold(row.len() % 2 == 0);
        row.push(cell);
        if wide {
            let mut continuation = TerminalCell::default();
            continuation.flags.set_wide_continuation(true);
            row.push(continuation);
        }
    }
    row
}

#[test]
fn shared_token_spans_match_frozen_native_cells_at_every_boundary() {
    for text in [
        "",
        "plain_word",
        "/",
        "--",
        "_.",
        "abc-42",
        "/tmp/file.rs:42:7",
        "https://example.test/a?q=1&x=2#tag",
        r"C:\Users\name\file.txt:8",
        "(foo/bar)",
        "[foo/bar],",
        "\"https://example.test/path\"",
        "'./relative/file'",
        "目录/文件.rs",
        "界/a?b=中",
        "e\u{301}/file",
        "a\u{17d8}/file",
        " foo/bar\tsecond/path ",
        "~/.config/app.toml",
        "a+b@example.test",
        "../foo%20bar/file",
        "a@b#c?d=e&f-g:h",
    ] {
        let physical = physical_cells(text);
        for width in [0, 1, 2, 3, 5, 9, usize::MAX] {
            let line = &physical[..width.min(physical.len())];
            for cursor in (0..=line.len().saturating_add(1)).chain(std::iter::once(usize::MAX)) {
                assert_eq!(
                    TerminalState::select_extended_token_span(line, cursor),
                    TerminalState::frozen_extended_token_span(line, cursor),
                    "text={text:?} width={width} cursor={cursor}"
                );
            }
        }
    }
}

#[test]
fn shared_token_classification_preserves_native_admission() {
    for ch in (0..=127)
        .filter_map(char::from_u32)
        .chain("目录é界🙂\u{301}\u{17d8}\u{00a0}\u{200d}".chars())
    {
        let row: Vec<_> = ['a', '/', ch]
            .into_iter()
            .map(|character| TerminalCell {
                character,
                ..TerminalCell::default()
            })
            .collect();
        assert_eq!(
            TerminalState::select_extended_token_span(&row, 2),
            TerminalState::frozen_extended_token_span(&row, 2),
            "{ch:?}"
        );
    }
}
