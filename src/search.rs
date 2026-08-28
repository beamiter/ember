/// 搜索功能模块
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Bound both renderer work and memory when a broad query (notably a single
/// space) is run against a very large scrollback.
pub const MAX_SEARCH_MATCHES: usize = 20_000;

/// 编译后的正则缓存槽。由 `SearchState` 持有,这样搜索面板打开期间
/// 每次刷新(PTY 输出、按键)只要 pattern 与大小写标志未变,就复用同一个
/// `Regex`,而不是每次都付出一次完整的 `RegexBuilder::build()`。
/// `pattern` 记录的是实际参与编译的字符串(纯文本模式下为转义后的字面量)。
#[derive(Clone, Debug)]
pub struct RegexCache {
    pattern: String,
    case_sensitive: bool,
    regex: regex::Regex,
}

/// 单个搜索匹配项
#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
pub struct SearchMatch {
    /// Monotonic terminal-buffer line id. Unlike a scrollback index or
    /// viewport row, this remains stable while newer output moves the row
    /// through the live grid and scrollback.
    pub line_id: u64,
    /// 列起始位置
    pub col_start: usize,
    /// 列结束位置（不含）
    pub col_end: usize,
}

impl SearchMatch {
    pub fn anchor(self) -> crate::terminal::BufferAnchor {
        crate::terminal::BufferAnchor {
            line_id: self.line_id,
            column: self.col_start,
        }
    }

    /// Map this stable match to the current viewport. `None` means the match
    /// has either scrolled out of view or has been evicted from scrollback.
    pub fn viewport_row(self, terminal: &crate::terminal::TerminalState) -> Option<usize> {
        terminal
            .buffer_anchor_to_viewport(self.anchor())
            .map(|(row, _)| row)
    }
}

/// 搜索功能的完整状态
#[derive(Clone, Debug)]
pub struct SearchState {
    /// 搜索面板是否打开
    pub is_open: bool,

    /// 搜索输入框中的文本
    pub query: String,

    /// 是否使用正则表达式模式
    pub use_regex: bool,

    /// 是否大小写敏感
    pub case_sensitive: bool,

    /// 所有匹配项的列表
    pub matches: Vec<SearchMatch>,

    /// 当前选中的匹配项索引
    pub current_match_index: usize,

    /// 搜索框是否有焦点
    pub search_focused: bool,

    /// 搜索历史队列（最近在前）
    pub history: VecDeque<SearchHistoryEntry>,

    /// 历史导航位置（None 表示在输入框，Some(i) 表示在历史第 i 项）
    pub history_nav_index: Option<usize>,

    /// 上次搜索词（用于检测搜索词变化）
    last_query: String,

    /// 搜索错误消息（正则表达式编译错误等）
    pub error_message: Option<String>,
    /// Projection-only navigation diagnostic. Keep this separate from regex
    /// errors so moving from a hidden match to a visible one cannot leave a
    /// stale error banner behind or erase an engine failure.
    pub projection_message: Option<String>,
    /// A hidden raw match is never expanded implicitly by Next/Previous.
    /// The search panel exposes an explicit reveal action for this owner.
    pub hidden_projection_zone: Option<u64>,
    /// Projection diagnostics are capabilities scoped to the exact policy
    /// revision that classified the current match. A later collapse/expand
    /// must reclassify before a reveal action can mutate session policy.
    pub projection_policy_revision: Option<u64>,
    /// More matches existed than we retain/render. Navigation remains bounded
    /// to the deterministic first [`MAX_SEARCH_MATCHES`] results.
    pub results_truncated: bool,

    /// 当前结果所属的终端版本/会话。用于输出或 tab 变化后按需刷新，
    /// 避免搜索面板显示旧会话的匹配计数与高亮。
    pub results_grid_version: Option<u64>,
    pub results_session_idx: Option<usize>,
    /// Stable owner for the result set. Session indices can be reused or
    /// reordered, while this id follows the terminal session itself.
    pub results_session_id: Option<String>,
    pub results_refreshed_at: Option<std::time::Instant>,
    /// 编译后的正则缓存;pattern 与大小写标志不变时跨刷新复用。
    pub regex_cache: Option<RegexCache>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHistoryEntry {
    pub query: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    pub timestamp: String,
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchState {
    /// 创建新的搜索状态
    pub fn new() -> Self {
        Self {
            is_open: false,
            query: String::new(),
            use_regex: false,
            case_sensitive: false,
            matches: Vec::new(),
            current_match_index: 0,
            search_focused: false,
            history: VecDeque::new(),
            history_nav_index: None,
            last_query: String::new(),
            error_message: None,
            projection_message: None,
            hidden_projection_zone: None,
            projection_policy_revision: None,
            results_truncated: false,
            results_grid_version: None,
            results_session_idx: None,
            results_session_id: None,
            results_refreshed_at: None,
            regex_cache: None,
        }
    }

    pub fn clear_projection_diagnostic(&mut self) {
        self.projection_message = None;
        self.hidden_projection_zone = None;
        self.projection_policy_revision = None;
    }

    pub fn projection_diagnostic_is_current(&self, session_id: &str, policy_revision: u64) -> bool {
        self.projection_message.is_some()
            && self.results_session_id.as_deref() == Some(session_id)
            && self.projection_policy_revision == Some(policy_revision)
    }

    /// 打开并聚焦搜索。重复调用保持打开，而不是意外切换为关闭。
    pub fn open(&mut self) {
        self.is_open = true;
        self.search_focused = true;
    }

    /// 关闭搜索面板
    pub fn close(&mut self) {
        self.is_open = false;
        self.search_focused = false;
        if !self.query.is_empty() && self.last_query != self.query {
            self.save_to_history();
            self.last_query = self.query.clone();
        }
    }

    /// 移动到下一个匹配项
    pub fn next_match(&mut self) {
        if !self.matches.is_empty() {
            self.current_match_index = (self.current_match_index + 1) % self.matches.len();
        }
    }

    /// 移动到上一个匹配项
    pub fn prev_match(&mut self) {
        if !self.matches.is_empty() {
            self.current_match_index = if self.current_match_index == 0 {
                self.matches.len() - 1
            } else {
                self.current_match_index - 1
            };
        }
    }

    /// 保存当前搜索词到历史
    fn save_to_history(&mut self) {
        if self.query.is_empty() {
            return;
        }

        // 检查重复
        if !self.history.is_empty() && self.history[0].query == self.query {
            return;
        }

        self.history.push_front(SearchHistoryEntry {
            query: self.query.clone(),
            is_regex: self.use_regex,
            case_sensitive: self.case_sensitive,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| format!("{}", d.as_secs()))
                .unwrap_or_else(|_| "unknown".to_string()),
        });

        // 限制历史大小
        while self.history.len() > 50 {
            self.history.pop_back();
        }
    }

    /// 从历史中加载前一条
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }

        if let Some(idx) = self.history_nav_index {
            if idx + 1 < self.history.len() {
                self.history_nav_index = Some(idx + 1);
                let entry = &self.history[idx + 1];
                self.query = entry.query.clone();
                self.use_regex = entry.is_regex;
                self.case_sensitive = entry.case_sensitive;
            }
        } else {
            self.history_nav_index = Some(0);
            let entry = &self.history[0];
            self.query = entry.query.clone();
            self.use_regex = entry.is_regex;
            self.case_sensitive = entry.case_sensitive;
        }
    }

    /// 从历史中加载后一条
    pub fn history_next(&mut self) {
        if let Some(idx) = self.history_nav_index {
            if idx > 0 {
                self.history_nav_index = Some(idx - 1);
                let entry = &self.history[idx - 1];
                self.query = entry.query.clone();
                self.use_regex = entry.is_regex;
                self.case_sensitive = entry.case_sensitive;
            } else {
                // 返回输入框
                self.history_nav_index = None;
                self.query.clear();
            }
        }
    }
}

/// 搜索引擎（用于在完整终端缓冲区中进行搜索）
pub struct SearchEngine;

impl SearchEngine {
    /// Search scrollback followed by the live grid. Matches use monotonic
    /// line ids so rendering can map them to the current viewport without
    /// confusing live-grid rows with historical rows.
    pub fn search(
        terminal: &crate::terminal::TerminalState,
        query: &str,
        use_regex: bool,
        case_sensitive: bool,
        regex_cache: &mut Option<RegexCache>,
    ) -> (Vec<SearchMatch>, Option<String>, bool) {
        if query.is_empty() {
            return (Vec::new(), None, false);
        }

        if use_regex {
            Self::search_regex(terminal, query, case_sensitive, regex_cache)
        } else {
            let (matches, truncated) =
                Self::search_plaintext(terminal, query, case_sensitive, regex_cache);
            (matches, None, truncated)
        }
    }

    /// 取得缓存的编译正则;pattern(实际参与编译的字符串)或大小写标志
    /// 变化时才重新编译并写回缓存,编译失败会清空缓存并返回错误。
    fn cached_regex<'a>(
        cache: &'a mut Option<RegexCache>,
        pattern: &str,
        case_sensitive: bool,
    ) -> Result<&'a regex::Regex, String> {
        let stale = match cache.as_ref() {
            Some(c) => c.pattern != pattern || c.case_sensitive != case_sensitive,
            None => true,
        };
        if stale {
            let mut builder = RegexBuilder::new(pattern);
            if !case_sensitive {
                builder.case_insensitive(true);
            }
            match builder.build() {
                Ok(regex) => {
                    *cache = Some(RegexCache {
                        pattern: pattern.to_string(),
                        case_sensitive,
                        regex,
                    });
                }
                Err(e) => {
                    *cache = None;
                    return Err(format!("Invalid regex: {}", e));
                }
            }
        }
        Ok(&cache.as_ref().expect("cache was just rebuilt").regex)
    }

    /// 逐行遍历 scrollback(主缓冲时)与活动网格;回调返回 true 表示达到
    /// 匹配上限、停止遍历。scrollback 的 Plain 行借用内部字符串,不做
    /// decompress → 重建字符串的往返。
    fn for_each_line(
        terminal: &crate::terminal::TerminalState,
        mut f: impl FnMut(u64, &str, Option<&[usize]>, usize) -> bool,
    ) -> bool {
        if !terminal.is_alt_buffer() {
            let first_line_id = terminal
                .total_lines_scrolled
                .saturating_sub(terminal.scrollback.len() as u64);
            for (line_idx, compressed) in terminal.scrollback.iter().enumerate() {
                let (line_str, col_map, total_cols) = compressed.search_text();
                if f(
                    first_line_id.saturating_add(line_idx as u64),
                    &line_str,
                    col_map.as_deref(),
                    total_cols,
                ) {
                    return true;
                }
            }
        }

        for (line_idx, line) in terminal.grid.iter().enumerate() {
            let (line_str, col_map, total_cols) = crate::terminal::searchable_line_text(line);
            if f(
                terminal
                    .total_lines_scrolled
                    .saturating_add(line_idx as u64),
                &line_str,
                Some(&col_map),
                total_cols,
            ) {
                return true;
            }
        }
        false
    }

    /// 字符索引 → 网格列号。col_map 为 None 表示恒等映射(Plain 行只含
    /// 窄字符,每个字符恰好占一列);越界统一落到 total_cols。
    fn column_of(col_map: Option<&[usize]>, total_cols: usize, char_idx: usize) -> usize {
        match col_map {
            Some(map) => map.get(char_idx).copied().unwrap_or(total_cols),
            None => char_idx.min(total_cols),
        }
    }

    /// 普通文本搜索
    fn search_plaintext(
        terminal: &crate::terminal::TerminalState,
        query: &str,
        case_sensitive: bool,
        regex_cache: &mut Option<RegexCache>,
    ) -> (Vec<SearchMatch>, bool) {
        let mut matches = Vec::new();

        // 大小写敏感时直接做子串查找(std 的 str::find 走 memmem),
        // 连转义字面量正则的编译都省掉。
        if case_sensitive {
            let truncated =
                Self::for_each_line(terminal, |line_id, line_str, col_map, total_cols| {
                    Self::append_substring_matches(
                        &mut matches,
                        line_id,
                        line_str,
                        col_map,
                        total_cols,
                        query,
                    )
                });
            return (matches, truncated);
        }

        // 大小写不敏感仍在原字符串上做 Unicode 匹配。整行 `to_lowercase()` 会让
        // 某些字符展开成多个码位（例如 İ → i + 组合点），从而把后续匹配映射
        // 到错误的终端列。转义后的字面量正则既保留原始字节偏移,也支持 Unicode;
        // 编译结果经 RegexCache 在搜索面板打开期间跨刷新复用。
        let regex = Self::cached_regex(regex_cache, &regex::escape(query), false)
            .expect("an escaped literal must compile as a regex");
        let truncated = Self::for_each_line(terminal, |line_id, line_str, col_map, total_cols| {
            Self::append_plaintext_regex_matches(
                &mut matches,
                line_id,
                line_str,
                col_map,
                total_cols,
                regex,
            )
        });
        (matches, truncated)
    }

    fn append_substring_matches(
        matches: &mut Vec<SearchMatch>,
        line_id: u64,
        line_str: &str,
        col_map: Option<&[usize]>,
        total_cols: usize,
        query: &str,
    ) -> bool {
        let mut start_byte = 0;
        while let Some(rel) = line_str[start_byte..].find(query) {
            let found_start = start_byte + rel;
            // 字节偏移 → 字符索引 → 网格列号(col_map 已跳过宽字符续接单元)。
            let start_char = line_str[..found_start].chars().count();
            let end_char = line_str[..found_start + query.len()].chars().count();
            matches.push(SearchMatch {
                line_id,
                col_start: Self::column_of(col_map, total_cols, start_char),
                col_end: Self::column_of(col_map, total_cols, end_char),
            });
            if matches.len() >= MAX_SEARCH_MATCHES {
                return true;
            }
            // 前进到下一个字符边界:既能找到重叠匹配,又不会切到多字节字符中间导致 panic。
            let step = line_str[found_start..]
                .chars()
                .next()
                .map_or(1, |c| c.len_utf8());
            start_byte = found_start + step;
        }
        false
    }

    fn append_plaintext_regex_matches(
        matches: &mut Vec<SearchMatch>,
        line_id: u64,
        line_str: &str,
        col_map: Option<&[usize]>,
        total_cols: usize,
        regex: &regex::Regex,
    ) -> bool {
        let mut start_byte = 0;
        while let Some(found) = regex.find_at(line_str, start_byte) {
            // 字节偏移 → 字符索引 → 网格列号(col_map 已跳过宽字符续接单元)。
            let start_char = line_str[..found.start()].chars().count();
            let end_char = line_str[..found.end()].chars().count();
            matches.push(SearchMatch {
                line_id,
                col_start: Self::column_of(col_map, total_cols, start_char),
                col_end: Self::column_of(col_map, total_cols, end_char),
            });
            if matches.len() >= MAX_SEARCH_MATCHES {
                return true;
            }
            // 前进到下一个字符边界:既能找到重叠匹配,又不会切到多字节字符中间导致 panic。
            let step = line_str[found.start()..]
                .chars()
                .next()
                .map_or(1, |c| c.len_utf8());
            start_byte = found.start() + step;
        }
        false
    }

    /// 正则表达式搜索
    fn search_regex(
        terminal: &crate::terminal::TerminalState,
        pattern: &str,
        case_sensitive: bool,
        regex_cache: &mut Option<RegexCache>,
    ) -> (Vec<SearchMatch>, Option<String>, bool) {
        let mut matches = Vec::new();

        let regex = match Self::cached_regex(regex_cache, pattern, case_sensitive) {
            Ok(regex) => regex,
            Err(e) => return (Vec::new(), Some(e), false),
        };
        let truncated = Self::for_each_line(terminal, |line_id, line_str, col_map, total_cols| {
            Self::append_regex_matches(&mut matches, line_id, line_str, col_map, total_cols, regex)
        });
        (matches, None, truncated)
    }

    fn append_regex_matches(
        matches: &mut Vec<SearchMatch>,
        line_id: u64,
        line_str: &str,
        col_map: Option<&[usize]>,
        total_cols: usize,
        regex: &regex::Regex,
    ) -> bool {
        for mat in regex.find_iter(line_str) {
            // regex 返回字节偏移,需转成字符索引再映射到网格列号。
            let start_char = line_str[..mat.start()].chars().count();
            let end_char = line_str[..mat.end()].chars().count();
            matches.push(SearchMatch {
                line_id,
                col_start: Self::column_of(col_map, total_cols, start_char),
                col_end: Self::column_of(col_map, total_cols, end_char),
            });
            if matches.len() >= MAX_SEARCH_MATCHES {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_state_open_and_close() {
        let mut state = SearchState::new();
        assert!(!state.is_open);
        state.open();
        assert!(state.is_open);
        state.close();
        assert!(!state.is_open);
    }

    #[test]
    fn opening_search_is_idempotent() {
        let mut state = SearchState::new();
        state.open();
        state.open();
        assert!(state.is_open);
        assert!(state.search_focused);
    }

    #[test]
    fn test_match_navigation() {
        let mut state = SearchState::new();
        state.matches = vec![
            SearchMatch {
                line_id: 0,
                col_start: 0,
                col_end: 5,
            },
            SearchMatch {
                line_id: 1,
                col_start: 10,
                col_end: 15,
            },
        ];

        assert_eq!(state.current_match_index, 0);
        state.next_match();
        assert_eq!(state.current_match_index, 1);
        state.next_match();
        assert_eq!(state.current_match_index, 0); // 循环

        state.prev_match();
        assert_eq!(state.current_match_index, 1);
    }

    #[test]
    fn case_insensitive_search_keeps_columns_after_unicode_case_expansion() {
        let mut terminal = crate::terminal::TerminalState::new(3, 1);
        terminal.grid.get_mut(0, 0).character = 'İ';
        terminal.grid.get_mut(0, 1).character = 'x';

        let (matches, error, truncated) =
            SearchEngine::search(&terminal, "x", false, false, &mut None);

        assert!(error.is_none());
        assert!(!truncated);
        assert_eq!(
            matches,
            vec![SearchMatch {
                line_id: 0,
                col_start: 1,
                col_end: 2,
            }]
        );
    }

    #[test]
    fn plaintext_search_still_finds_overlapping_matches() {
        let mut terminal = crate::terminal::TerminalState::new(3, 1);
        for col in 0..3 {
            terminal.grid.get_mut(0, col).character = 'a';
        }

        let (matches, error, truncated) =
            SearchEngine::search(&terminal, "aa", false, true, &mut None);

        assert!(error.is_none());
        assert!(!truncated);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].col_start, 0);
        assert_eq!(matches[1].col_start, 1);
    }

    #[test]
    fn search_text_borrows_plain_scrollback_rows_with_identity_columns() {
        let mut cells = vec![crate::terminal::TerminalCell::default(); 8];
        cells[0].character = 'h';
        cells[1].character = 'i';
        let line = crate::terminal::ScrollbackLine::compress(&cells, false);

        let (text, col_map, total_cols) = line.search_text();
        assert_eq!(text.as_ref(), "hi");
        assert!(col_map.is_none(), "plain rows use the identity column map");
        assert_eq!(total_cols, 2);

        let mut terminal = crate::terminal::TerminalState::new(8, 1);
        terminal.scrollback.push_back(line);
        terminal.total_lines_scrolled = 1;

        let (matches, error, truncated) =
            SearchEngine::search(&terminal, "hi", false, true, &mut None);
        assert!(error.is_none());
        assert!(!truncated);
        assert_eq!(
            matches,
            vec![SearchMatch {
                line_id: 0,
                col_start: 0,
                col_end: 2,
            }]
        );
    }

    #[test]
    fn search_text_maps_wide_char_columns_through_encoded_scrollback() {
        // Styled cells force the Encoded representation; the wide char at
        // column 2 occupies columns 2-3, so its continuation cell must not
        // shift the column of the following 'x'.
        let mut cells = vec![crate::terminal::TerminalCell::default(); 8];
        cells[0].character = 'a';
        cells[1].character = 'b';
        cells[2].character = '好';
        cells[2].flags.set_wide(true);
        cells[3].flags.set_wide_continuation(true);
        cells[4].character = 'x';
        for cell in cells.iter_mut().take(5) {
            cell.foreground = crate::terminal::Color::Red;
        }
        let line = crate::terminal::ScrollbackLine::compress(&cells, false);
        let (text, col_map, total_cols) = line.search_text();
        assert_eq!(text.as_ref(), "ab好x");
        assert_eq!(col_map.as_deref(), Some(&[0, 1, 2, 4][..]));
        assert_eq!(total_cols, 5);

        let mut terminal = crate::terminal::TerminalState::new(8, 1);
        terminal.scrollback.push_back(line);
        terminal.total_lines_scrolled = 1;

        for case_sensitive in [true, false] {
            let (matches, error, truncated) =
                SearchEngine::search(&terminal, "x", false, case_sensitive, &mut None);
            assert!(error.is_none());
            assert!(!truncated);
            assert_eq!(
                matches,
                vec![SearchMatch {
                    line_id: 0,
                    col_start: 4,
                    col_end: 5,
                }],
                "wide-char column mapping broke (case_sensitive={case_sensitive})"
            );
        }

        // The wide character itself is searchable and spans both columns.
        let (matches, error, _) = SearchEngine::search(&terminal, "好", false, true, &mut None);
        assert!(error.is_none());
        assert_eq!(
            matches,
            vec![SearchMatch {
                line_id: 0,
                col_start: 2,
                col_end: 4,
            }]
        );
    }

    #[test]
    fn regex_cache_invalidates_only_on_pattern_or_case_change() {
        let mut terminal = crate::terminal::TerminalState::new(4, 1);
        terminal.grid.get_mut(0, 0).character = 'a';
        let mut cache = None;

        // Case-sensitive plaintext never compiles a regex at all.
        let (matches, error, _) = SearchEngine::search(&terminal, "a", false, true, &mut cache);
        assert!(error.is_none() && matches.len() == 1);
        assert!(cache.is_none());

        // Regex mode compiles once and keeps the slot while nothing changes.
        let (matches, error, _) = SearchEngine::search(&terminal, "a", true, true, &mut cache);
        assert!(error.is_none() && matches.len() == 1);
        assert_eq!(
            cache
                .as_ref()
                .map(|c| (c.pattern.as_str(), c.case_sensitive)),
            Some(("a", true))
        );
        let (matches, error, _) = SearchEngine::search(&terminal, "a", true, true, &mut cache);
        assert!(error.is_none() && matches.len() == 1);
        assert_eq!(
            cache
                .as_ref()
                .map(|c| (c.pattern.as_str(), c.case_sensitive)),
            Some(("a", true))
        );

        // A case-flag flip rebuilds; an invalid pattern reports the error and
        // clears the slot so the next valid pattern compiles fresh.
        let (_, error, _) = SearchEngine::search(&terminal, "a", true, false, &mut cache);
        assert!(error.is_none());
        assert_eq!(
            cache
                .as_ref()
                .map(|c| (c.pattern.as_str(), c.case_sensitive)),
            Some(("a", false))
        );
        let (matches, error, _) = SearchEngine::search(&terminal, "a(", true, true, &mut cache);
        assert!(matches.is_empty());
        assert!(error.is_some());
        assert!(cache.is_none());
    }

    #[test]
    fn search_covers_scrollback_and_maps_matches_back_to_the_viewport() {
        let mut terminal = crate::terminal::TerminalState::new(12, 2);
        terminal.process_input(b"old-needle\r\nnew-one\r\nnew-two\r\n");
        assert!(!terminal.scrollback.is_empty());

        let (matches, error, truncated) =
            SearchEngine::search(&terminal, "old-needle", false, true, &mut None);

        assert!(error.is_none());
        assert!(!truncated);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].viewport_row(&terminal).is_none());
        let original_match = matches[0];

        terminal.process_input(b"later-a\r\nlater-b\r\n");
        let (refreshed, error, truncated) =
            SearchEngine::search(&terminal, "old-needle", false, true, &mut None);
        assert!(error.is_none());
        assert!(!truncated);
        assert_eq!(refreshed, vec![original_match]);

        assert!(terminal.scroll_to_buffer_anchor(original_match.anchor()));
        assert_eq!(original_match.viewport_row(&terminal), Some(0));
    }

    #[test]
    fn broad_queries_are_bounded_and_ignore_default_padding() {
        let (cols, rows) = (64, crate::terminal::MAX_TERMINAL_ROWS);
        let mut terminal = crate::terminal::TerminalState::new(cols, rows);
        for row in 0..rows {
            for col in 0..cols {
                terminal.grid.get_mut(row, col).character = 'x';
            }
        }

        let (matches, error, truncated) =
            SearchEngine::search(&terminal, "x", false, true, &mut None);
        assert!(error.is_none());
        assert!(truncated);
        assert_eq!(matches.len(), MAX_SEARCH_MATCHES);

        let blank_terminal = crate::terminal::TerminalState::new(64, 2);
        let (padding_matches, error, truncated) =
            SearchEngine::search(&blank_terminal, " ", false, true, &mut None);
        assert!(error.is_none());
        assert!(!truncated);
        assert!(padding_matches.is_empty());
    }

    #[test]
    fn resized_scrollback_never_reports_a_wrong_highlight_coordinate() {
        let mut terminal = crate::terminal::TerminalState::new(4, 2);
        let old_width_line = vec![crate::terminal::TerminalCell::default(); 8];
        terminal
            .scrollback
            .push_back(crate::terminal::ScrollbackLine::compress(
                &old_width_line,
                false,
            ));
        terminal.total_lines_scrolled = 1;
        terminal.scroll_offset = 1;

        let search_match = SearchMatch {
            line_id: 0,
            col_start: 5,
            col_end: 6,
        };
        assert!(!terminal.viewport_buffer_mapping_is_exact());
        assert_eq!(search_match.viewport_row(&terminal), None);
    }

    #[test]
    fn projection_diagnostic_is_scoped_to_stable_session_and_policy() {
        let mut state = SearchState::new();
        state.projection_message = Some("hidden".to_owned());
        state.hidden_projection_zone = Some(1);
        state.projection_policy_revision = Some(4);
        state.results_session_id = Some("session-a".to_owned());

        assert!(state.projection_diagnostic_is_current("session-a", 4));
        assert!(!state.projection_diagnostic_is_current("session-b", 4));
        assert!(!state.projection_diagnostic_is_current("session-a", 5));

        state.clear_projection_diagnostic();
        assert!(!state.projection_diagnostic_is_current("session-a", 4));
        assert!(state.hidden_projection_zone.is_none());
    }
}
