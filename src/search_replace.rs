use serde::{Deserialize, Serialize};

/// 高级搜索配置
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SearchConfig {
    pub use_regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub multi_line: bool,
}

/// 替换选项
#[derive(Clone, Debug, Default)]
pub struct ReplaceOptions {
    pub replace_all: bool,
}

/// 搜索和替换引擎
pub struct SearchAndReplaceEngine;

impl SearchAndReplaceEngine {
    /// 执行搜索和替换
    pub fn search_and_replace(
        text: &str,
        search_pattern: &str,
        replacement: &str,
        config: &SearchConfig,
        options: &ReplaceOptions,
    ) -> Result<(String, usize), String> {
        // An empty Find field is a no-op in both literal and regex modes.
        if search_pattern.is_empty() {
            return Ok((text.to_string(), 0));
        }
        if config.use_regex {
            Self::regex_replace(text, search_pattern, replacement, config, options)
        } else {
            Self::literal_replace(text, search_pattern, replacement, config, options)
        }
    }

    /// 文字替换
    fn literal_replace(
        text: &str,
        pattern: &str,
        replacement: &str,
        config: &SearchConfig,
        options: &ReplaceOptions,
    ) -> Result<(String, usize), String> {
        if pattern.is_empty() {
            return Ok((text.to_string(), 0));
        }

        let mut result = String::with_capacity(text.len());
        let mut count = 0;
        let mut i = 0;

        while i < text.len() {
            let take_more = options.replace_all || count == 0;
            if take_more {
                if let Some(mlen) = match_len_at(text, i, pattern, config.case_sensitive) {
                    if mlen > 0 && (!config.whole_word || is_whole_word(text, i, i + mlen)) {
                        result.push_str(replacement);
                        i += mlen;
                        count += 1;
                        continue;
                    }
                }
            }
            // Copy a single UTF-8 character from the original text.
            // text.get 避免 i 落在非字符边界时的切片 panic(mlen 理论上总落边界,
            // 但此处容错跳过一字节,绝不 panic)。
            let Some(ch) = text.get(i..).and_then(|s| s.chars().next()) else {
                i += 1;
                continue;
            };
            result.push(ch);
            i += ch.len_utf8();
        }

        Ok((result, count))
    }

    /// 正则表达式替换
    fn regex_replace(
        text: &str,
        pattern: &str,
        replacement: &str,
        config: &SearchConfig,
        options: &ReplaceOptions,
    ) -> Result<(String, usize), String> {
        use regex::RegexBuilder;

        let effective = if config.whole_word {
            format!(r"\b(?:{pattern})\b")
        } else {
            pattern.to_string()
        };

        let regex = RegexBuilder::new(&effective)
            .case_insensitive(!config.case_sensitive)
            .multi_line(config.multi_line)
            .build()
            .map_err(crate::search::safe_regex_error)?;

        let result = if options.replace_all {
            regex.replace_all(text, replacement).to_string()
        } else {
            regex.replace(text, replacement).to_string()
        };

        let count = if options.replace_all {
            regex.find_iter(text).count()
        } else if regex.is_match(text) {
            1
        } else {
            0
        };

        Ok((result, count))
    }
}

fn is_word_char(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    // Share the regex mode's Unicode word boundary: combining marks,
    // connector punctuation and non-Latin letters all belong to words.
    static WORD_CHAR: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\A\w\z").expect("valid word class"));
    WORD_CHAR.is_match(c.encode_utf8(&mut [0; 4]))
}

/// True when `text[start..end]` is bounded by non-word characters (or the ends
/// of the string), i.e. it forms a whole word.
fn is_whole_word(text: &str, start: usize, end: usize) -> bool {
    let before_ok = text[..start]
        .chars()
        .next_back()
        .is_none_or(|c| !is_word_char(c));
    let after_ok = text[end..].chars().next().is_none_or(|c| !is_word_char(c));
    before_ok && after_ok
}

/// If `pattern` matches `text` at byte offset `idx`, returns the byte length of
/// the match within `text`; otherwise `None`. Honors case sensitivity using
/// Unicode-aware case folding.
fn match_len_at(text: &str, idx: usize, pattern: &str, case_sensitive: bool) -> Option<usize> {
    let mut chars = text[idx..].chars();
    let mut consumed = 0;
    for pc in pattern.chars() {
        let tc = chars.next()?;
        let eq = if case_sensitive {
            tc == pc
        } else {
            tc.to_lowercase().eq(pc.to_lowercase())
        };
        if !eq {
            return None;
        }
        consumed += tc.len_utf8();
    }
    Some(consumed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_word_literal_uses_unicode_regex_boundaries() {
        for (text, pattern, expected, count) in [
            ("cafe\u{301} cafe", "cafe", "cafe\u{301} X", 1),
            ("a\u{301}\u{327} a", "a", "a\u{301}\u{327} X", 1),
            (
                "\u{301} \u{301}a a\u{301} \u{301}",
                "\u{301}",
                "X \u{301}a a\u{301} X",
                2,
            ),
            ("界2 界٢ 界 界_ _界", "界", "界2 界٢ X 界_ _界", 1),
            ("٣a ٣ ٣_ _٣", "٣", "٣a X ٣_ _٣", 1),
            ("a_ _ _٣ _界", "_", "a_ X _٣ _界", 1),
            ("a\u{203f} a \u{203f}a", "a", "a\u{203f} X \u{203f}a", 1),
        ] {
            for use_regex in [false, true] {
                let config = SearchConfig {
                    use_regex,
                    whole_word: true,
                    case_sensitive: true,
                    ..Default::default()
                };
                let actual = SearchAndReplaceEngine::search_and_replace(
                    text,
                    pattern,
                    "X",
                    &config,
                    &ReplaceOptions { replace_all: true },
                )
                .unwrap();
                assert_eq!(
                    actual,
                    (expected.to_owned(), count),
                    "text={text:?}, regex={use_regex}"
                );
            }
        }
    }

    #[test]
    fn empty_find_is_noop_in_every_mode() {
        for use_regex in [false, true] {
            for whole_word in [false, true] {
                for replace_all in [false, true] {
                    let config = SearchConfig {
                        use_regex,
                        whole_word,
                        ..Default::default()
                    };
                    let actual = SearchAndReplaceEngine::search_and_replace(
                        "abc",
                        "",
                        "x",
                        &config,
                        &ReplaceOptions { replace_all },
                    )
                    .unwrap();
                    assert_eq!(actual, ("abc".to_owned(), 0));
                }
            }
        }
    }

    #[test]
    fn replacement_does_not_rescan_inserted_text_and_regex_expands_captures() {
        let options = ReplaceOptions { replace_all: true };
        let actual = SearchAndReplaceEngine::search_and_replace(
            "aa",
            "a",
            "aa",
            &SearchConfig::default(),
            &options,
        )
        .unwrap();
        assert_eq!(actual, ("aaaa".to_owned(), 2));
        let config = SearchConfig {
            use_regex: true,
            multi_line: true,
            ..Default::default()
        };
        let actual = SearchAndReplaceEngine::search_and_replace(
            "one\ntwo", r"^(\w+)$", "$1!", &config, &options,
        )
        .unwrap();
        assert_eq!(actual, ("one!\ntwo!".to_owned(), 2));
    }

    #[test]
    fn test_literal_replace() {
        let config = SearchConfig::default();
        let options = ReplaceOptions::default();

        let (result, count) = SearchAndReplaceEngine::search_and_replace(
            "hello world hello",
            "hello",
            "hi",
            &config,
            &options,
        )
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(result, "hi world hello");
    }

    #[test]
    fn test_replace_all() {
        let config = SearchConfig::default();
        let options = ReplaceOptions { replace_all: true };

        let (result, count) = SearchAndReplaceEngine::search_and_replace(
            "hello world hello",
            "hello",
            "hi",
            &config,
            &options,
        )
        .unwrap();

        assert_eq!(count, 2);
        assert_eq!(result, "hi world hi");
    }

    #[test]
    fn test_whole_word_literal() {
        let config = SearchConfig {
            whole_word: true,
            ..Default::default()
        };
        let options = ReplaceOptions { replace_all: true };

        let (result, count) = SearchAndReplaceEngine::search_and_replace(
            "cat category cat",
            "cat",
            "dog",
            &config,
            &options,
        )
        .unwrap();

        assert_eq!(count, 2);
        assert_eq!(result, "dog category dog");
    }

    #[test]
    fn test_case_insensitive_literal() {
        let config = SearchConfig {
            case_sensitive: false,
            ..Default::default()
        };
        let options = ReplaceOptions { replace_all: true };

        let (result, count) = SearchAndReplaceEngine::search_and_replace(
            "Foo FOO foo",
            "foo",
            "bar",
            &config,
            &options,
        )
        .unwrap();

        assert_eq!(count, 3);
        assert_eq!(result, "bar bar bar");
    }

    #[test]
    fn test_regex_case_insensitive_and_whole_word() {
        let config = SearchConfig {
            use_regex: true,
            case_sensitive: false,
            whole_word: true,
            ..Default::default()
        };
        let options = ReplaceOptions { replace_all: true };

        let (result, count) = SearchAndReplaceEngine::search_and_replace(
            "Err error ERRORS",
            "err",
            "X",
            &config,
            &options,
        )
        .unwrap();

        // "Err" matches (whole word, case-insensitive); "error" and "ERRORS"
        // do not because of the word boundary.
        assert_eq!(count, 1);
        assert_eq!(result, "X error ERRORS");
    }
}
