/// 搜索功能模块
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// 单个搜索匹配项
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// 所在行
    pub line: usize,
    /// 列起始位置
    pub col_start: usize,
    /// 列结束位置（不含）
    pub col_end: usize,
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

    /// 当前结果所属的终端版本/会话。用于输出或 tab 变化后按需刷新，
    /// 避免搜索面板显示旧会话的匹配计数与高亮。
    pub results_grid_version: Option<u64>,
    pub results_session_idx: Option<usize>,
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
            results_grid_version: None,
            results_session_idx: None,
        }
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

/// 搜索引擎（用于在网格中进行搜索）
pub struct SearchEngine;

impl SearchEngine {
    /// 在网格中搜索文本
    pub fn search(
        grid: &crate::terminal::TerminalGrid,
        query: &str,
        use_regex: bool,
        case_sensitive: bool,
    ) -> (Vec<SearchMatch>, Option<String>) {
        if query.is_empty() {
            return (Vec::new(), None);
        }

        if use_regex {
            Self::search_regex(grid, query, case_sensitive)
        } else {
            (Self::search_plaintext(grid, query, case_sensitive), None)
        }
    }

    /// 普通文本搜索
    fn search_plaintext(
        grid: &crate::terminal::TerminalGrid,
        query: &str,
        case_sensitive: bool,
    ) -> Vec<SearchMatch> {
        let mut matches = Vec::new();
        // 在原字符串上做 Unicode 大小写不敏感匹配。整行 `to_lowercase()` 会让
        // 某些字符展开成多个码位（例如 İ → i + 组合点），从而把后续匹配映射
        // 到错误的终端列。转义后的字面量正则既保留原始字节偏移，也支持 Unicode。
        let mut builder = RegexBuilder::new(&regex::escape(query));
        builder.case_insensitive(!case_sensitive);
        let regex = builder
            .build()
            .expect("an escaped literal must compile as a regex");

        for (line_idx, line) in grid.iter().enumerate() {
            let (line_str, col_map) = Self::grid_line_to_string(line);
            let total_cols = line.len();

            let mut start_byte = 0;
            while let Some(found) = regex.find_at(&line_str, start_byte) {
                // 字节偏移 → 字符索引 → 网格列号(col_map 已跳过宽字符续接单元)。
                let start_char = line_str[..found.start()].chars().count();
                let end_char = line_str[..found.end()].chars().count();
                let col_start = col_map.get(start_char).copied().unwrap_or(total_cols);
                let col_end = col_map.get(end_char).copied().unwrap_or(total_cols);
                matches.push(SearchMatch {
                    line: line_idx,
                    col_start,
                    col_end,
                });
                // 前进到下一个字符边界:既能找到重叠匹配,又不会切到多字节字符中间导致 panic。
                let step = line_str[found.start()..]
                    .chars()
                    .next()
                    .map_or(1, |c| c.len_utf8());
                start_byte = found.start() + step;
            }
        }

        matches
    }

    /// 正则表达式搜索
    fn search_regex(
        grid: &crate::terminal::TerminalGrid,
        pattern: &str,
        case_sensitive: bool,
    ) -> (Vec<SearchMatch>, Option<String>) {
        let mut matches = Vec::new();

        // 编译正则表达式
        let mut builder = RegexBuilder::new(pattern);
        if !case_sensitive {
            builder.case_insensitive(true);
        }

        let regex = match builder.build() {
            Ok(r) => r,
            Err(e) => {
                return (Vec::new(), Some(format!("Invalid regex: {}", e)));
            }
        };

        for (line_idx, line) in grid.iter().enumerate() {
            let (line_str, col_map) = Self::grid_line_to_string(line);
            let total_cols = line.len();

            for mat in regex.find_iter(&line_str) {
                // regex 返回字节偏移,需转成字符索引再映射到网格列号。
                let start_char = line_str[..mat.start()].chars().count();
                let end_char = line_str[..mat.end()].chars().count();
                let col_start = col_map.get(start_char).copied().unwrap_or(total_cols);
                let col_end = col_map.get(end_char).copied().unwrap_or(total_cols);
                matches.push(SearchMatch {
                    line: line_idx,
                    col_start,
                    col_end,
                });
            }
        }

        (matches, None)
    }

    /// 将网格行转换为字符串,并返回每个字符对应的网格列号。
    /// 跳过宽字符的续接单元(否则相邻宽字符间会被插入空格导致匹配失败),
    /// 因此字符索引与字节偏移都不再等于列号,需经 col_map 映射。
    fn grid_line_to_string(line: &[crate::terminal::TerminalCell]) -> (String, Vec<usize>) {
        let mut s = String::with_capacity(line.len());
        let mut col_map = Vec::with_capacity(line.len());
        for (col, cell) in line.iter().enumerate() {
            if cell.flags.wide_continuation() {
                continue;
            }
            s.push(cell.character);
            col_map.push(col);
        }
        (s, col_map)
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
                line: 0,
                col_start: 0,
                col_end: 5,
            },
            SearchMatch {
                line: 1,
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
        let mut grid = crate::terminal::TerminalGrid::new(1, 3);
        grid.get_mut(0, 0).character = 'İ';
        grid.get_mut(0, 1).character = 'x';

        let (matches, error) = SearchEngine::search(&grid, "x", false, false);

        assert!(error.is_none());
        assert_eq!(
            matches,
            vec![SearchMatch {
                line: 0,
                col_start: 1,
                col_end: 2,
            }]
        );
    }

    #[test]
    fn plaintext_search_still_finds_overlapping_matches() {
        let mut grid = crate::terminal::TerminalGrid::new(1, 3);
        for col in 0..3 {
            grid.get_mut(0, col).character = 'a';
        }

        let (matches, error) = SearchEngine::search(&grid, "aa", false, true);

        assert!(error.is_none());
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].col_start, 0);
        assert_eq!(matches[1].col_start, 1);
    }
}
