//! 历史命令选择器：跨重启持久化命令的模糊搜索浮层（Ctrl+Shift+H 打开）。
//!
//! 记录与检索都建立在家族共享的 `jterm_core::command_history` JSONL 索引上
//! （与 anvil/forge/frost 同名配置键、同文件格式），因此几个兄弟终端可以
//! 指向同一份历史文件。Enter 只把选中的命令回填到活动 pane 的提示符，
//! 从不执行。
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use jterm_core::command_history::{self, CommandHistoryRecord};
use std::borrow::Cow;

/// 打开选择器时加载的最大条数。与 forge/frost 的历史面板一致：交互检索只
/// 需要一个近期工作集，`read_recent` 本身也把读取限制在有界的文件尾部。
pub const PICKER_MAX_ENTRIES: usize = 2_000;

// Bound dynamic-programming work rather than only the source byte count.
// Skim's linear fallback preserves full-input membership and smart case; only
// expensive long-input ranking changes. Bytes conservatively bound scalars.
const MAX_HISTORY_FUZZY_WORK: usize = 64 * 1024;

fn use_linear_history_match(haystack: &str, query: &str) -> bool {
    haystack.len().saturating_mul(query.len()) > MAX_HISTORY_FUZZY_WORK
}

/// 一次渲染/导航的最大结果数。键盘选择与绘制共用 `filtered()`，因此上限
/// 同时约束两者——更早的命令通过输入查询来召回。
pub const MAX_RESULTS: usize = 15;
/// 与共享历史文件的写入方保持一致：`jterm_core::command_history` 的
/// `MAX_CWD_BYTES`（jterm_core/src/command_history.rs:31）是 16 KiB，forge 的
/// 读取侧用的也是同一个数。核心没有导出这个常量，所以这里照抄并注明出处。
///
/// 之前这里是 4 KiB：ember 在写入前用 `sanitized_cwd` 过滤，于是深层目录里
/// 完成的命令被静默地写成 `cwd: None`（永久丢失，没有任何提示），读取时又把
/// 兄弟终端写进来的 4–16 KiB cwd 抹掉，连按目录模糊召回都找不到。
const MAX_HISTORY_CWD_BYTES: usize = 16 * 1024;

/// 共享历史文件里一条命令的上限，取自核心自己的写入契约：
/// `jterm_core::command_history` 的 `MAX_COMMAND_BYTES` 就是
/// `review_input::MAX_REVIEW_INPUT_BYTES`（256 KiB），这个常量是导出的，
/// 所以这里直接引用而不是再抄一份数字。
///
/// 之前这里用的是 ember 自己的 64 KiB（`review_text::MAX_HISTORY_COMMAND_BYTES`，
/// 那是 OSC 133 重放通道的预算，不是共享文件的）：兄弟终端写进来的
/// 64 KiB–256 KiB 命令是合法记录，读取侧却把它们整条丢掉——既不显示也不
/// 参与模糊匹配，用户只会看到历史里少了几条，没有任何提示。
pub(crate) const MAX_SHARED_HISTORY_COMMAND_BYTES: usize =
    jterm_core::review_input::MAX_REVIEW_INPUT_BYTES;
pub(crate) const MAX_HISTORY_QUERY_BYTES: usize = jterm_core::workflows::MAX_PICKER_QUERY_BYTES;

fn history_query_is_unsafe(query: &str) -> bool {
    query.contains('\u{fffd}') || jterm_core::review_input::contains_visual_spoofing(query)
}

fn bound_history_query(query: impl Into<String>) -> String {
    let mut query: String = query
        .into()
        .chars()
        .filter_map(|character| {
            if character.is_control() {
                None
            } else if jterm_core::review_input::is_visual_spoofing_character(character) {
                Some('\u{fffd}')
            } else {
                Some(character)
            }
        })
        .collect();
    if query.len() > MAX_HISTORY_QUERY_BYTES {
        let mut end = MAX_HISTORY_QUERY_BYTES;
        while end > 0 && !query.is_char_boundary(end) {
            end -= 1;
        }
        query.truncate(end);
    }
    query
}

/// 把一条 OSC 133 重建的命令行修剪并校验为可持久化文本。返回 `None` 表示
/// 不应写入历史：空白命令，或含换行/控制字符的重建文本（例如 heredoc 的
/// 多行命令）——家族的 review-only 历史格式拒绝控制字符，这类文本也无法
/// 安全地回填到提示符。
pub fn sanitized_command(command: &str) -> Option<&str> {
    let trimmed = command.trim_matches(' ');
    crate::review_text::validate_single_line(trimmed, MAX_SHARED_HISTORY_COMMAND_BYTES).ok()
}

pub fn sanitized_cwd(cwd: &str) -> Option<&str> {
    if cwd.len() > MAX_HISTORY_CWD_BYTES
        || cwd.contains('\u{fffd}')
        || cwd.chars().any(char::is_control)
        || jterm_core::review_input::contains_visual_spoofing(cwd)
    {
        None
    } else {
        Some(cwd)
    }
}

/// 行展示的单行截断（按字符计，避免超长命令把浮层撑成多行）。只影响显示；
/// 回填到提示符的始终是完整命令文本。
pub fn display_command(command: &str) -> String {
    const MAX_DISPLAY_CHARS: usize = 120;
    if command.len() > MAX_SHARED_HISTORY_COMMAND_BYTES {
        return "(command omitted: exceeds review limit)".to_string();
    }
    let visible = crate::review_text::visible_bounded(command, 4 * 1024);
    if visible.chars().count() <= MAX_DISPLAY_CHARS {
        return visible;
    }
    let mut shortened: String = visible.chars().take(MAX_DISPLAY_CHARS - 1).collect();
    shortened.push('…');
    shortened
}

/// 历史选择器状态。`entries` 最新在前（`read_recent` 的顺序），在打开浮层
/// 时加载一次；期间新完成的命令会在下一次打开时出现。
pub struct HistoryPickerState {
    query: String,
    query_buffer: String,
    /// 当前过滤结果中的高亮位置。
    pub selected: usize,
    /// 是否需要聚焦搜索框（egui 文本框在浮层打开后的第一帧取焦）。
    pub needs_focus: bool,
    entries: Vec<CommandHistoryRecord>,
    matcher: SkimMatcherV2,
    linear_matcher: SkimMatcherV2,
    results: Vec<usize>,
    scroll_to_selected: bool,
    confirm_requested: bool,
    #[cfg(test)]
    rebuild_count: usize,
}

impl HistoryPickerState {
    pub fn new(mut entries: Vec<CommandHistoryRecord>) -> Self {
        entries.retain_mut(|record| {
            let Some(command) = sanitized_command(&record.command) else {
                return false;
            };
            if command.len() != record.command.len() {
                record.command = command.to_string();
            }
            if record
                .cwd
                .as_deref()
                .is_some_and(|cwd| sanitized_cwd(cwd).is_none())
            {
                record.cwd = None;
            }
            true
        });
        let mut state = Self {
            query: String::new(),
            query_buffer: String::new(),
            selected: 0,
            needs_focus: true,
            entries,
            matcher: SkimMatcherV2::default(),
            linear_matcher: SkimMatcherV2::default().element_limit(1),
            results: Vec::new(),
            scroll_to_selected: true,
            confirm_requested: false,
            #[cfg(test)]
            rebuild_count: 0,
        };
        state.rebuild_results();
        state
    }

    /// 从持久化索引加载最近的一段。读取是有界的（文件尾部窗口 + 条数上限），
    /// 文件缺失或损坏时得到一个空的选择器而不是错误。
    pub fn load(path: &std::path::Path) -> Self {
        let records = command_history::prepare_path(path, false)
            .and_then(|()| command_history::read_recent(path, PICKER_MAX_ENTRIES))
            .unwrap_or_default();
        Self::new(records)
    }

    /// 当前过滤结果（最多 [`MAX_RESULTS`] 条）。空查询保持最新在前；否则按
    /// 模糊匹配分数降序，同分保持新旧顺序（稳定排序），命令与 cwd 一起参与
    /// 匹配，便于按项目目录召回。
    pub fn filtered(&self) -> Vec<&CommandHistoryRecord> {
        self.results
            .iter()
            .map(|index| &self.entries[*index])
            .collect()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// egui edits a draft; sync it once after a real edit, never on idle frames.
    pub fn query_buffer_mut(&mut self) -> &mut String {
        &mut self.query_buffer
    }

    pub fn sync_query(&mut self) {
        let query = std::mem::take(&mut self.query_buffer);
        self.set_query(query);
    }

    /// Only a changed normalized query invalidates matching and navigation.
    pub fn set_query(&mut self, query: impl Into<String>) {
        let query = bound_history_query(query);
        self.query_buffer.clone_from(&query);
        if self.query == query {
            return;
        }
        self.query = query;
        self.selected = 0;
        self.scroll_to_selected = true;
        self.rebuild_results();
    }

    fn rebuild_results(&mut self) {
        #[cfg(test)]
        {
            self.rebuild_count += 1;
        }
        if self.query.is_empty() {
            self.results = (0..self.entries.len().min(MAX_RESULTS)).collect();
            return;
        }
        if history_query_is_unsafe(&self.query) {
            self.results.clear();
            return;
        }
        let mut scored: Vec<(i64, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, record)| {
                let haystack = match record.cwd.as_deref() {
                    Some(cwd) => Cow::Owned(format!("{} {cwd}", record.command)),
                    None => Cow::Borrowed(record.command.as_str()),
                };
                let matcher = if use_linear_history_match(&haystack, &self.query) {
                    &self.linear_matcher
                } else {
                    &self.matcher
                };
                matcher
                    .fuzzy_match(&haystack, &self.query)
                    .map(|score| (score, index))
            })
            .collect();
        // Stable sort retains newest-first order for equal scores.
        scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        self.results = scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(_, index)| index)
            .collect();
    }

    /// The input prepass records intent; the renderer confirms only after
    /// TextEdit applies this frame's text, paste and IME events.
    pub fn request_confirm(&mut self) {
        self.confirm_requested = true;
    }

    pub fn take_confirm_request(&mut self) -> bool {
        std::mem::take(&mut self.confirm_requested)
    }

    /// One-shot keyboard/query scroll intent; pointer and wheel retain control.
    pub fn take_scroll_to_selected(&mut self) -> bool {
        std::mem::take(&mut self.scroll_to_selected)
    }

    pub fn select_hovered(&mut self, index: usize) {
        if index < self.results.len() {
            self.selected = index;
        }
    }

    pub fn select_next(&mut self) {
        let len = self.results.len();
        self.selected = if len == 0 {
            0
        } else {
            (self.selected + 1) % len
        };
        self.scroll_to_selected = len > 0;
        self.needs_focus = true;
    }

    pub fn select_prev(&mut self) {
        let len = self.results.len();
        self.selected = if len == 0 {
            0
        } else if self.selected == 0 {
            len - 1
        } else {
            self.selected - 1
        };
        self.scroll_to_selected = len > 0;
        self.needs_focus = true;
    }

    pub fn selected_command(&self) -> Option<String> {
        self.results
            .get(self.selected)
            .and_then(|index| sanitized_command(&self.entries[*index].command))
            .map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_bound_matches_the_shared_history_writer() {
        // 家族共享同一个 JSONL 历史文件：核心的写入方（以及 forge 的读取方）
        // 用 16 KiB。这里如果更小，ember 写入时会把深层目录静默降级成
        // cwd: None（永久丢失），读取时又会把兄弟终端写进来的 cwd 抹掉。
        let deep = "/".to_string() + &"segment/".repeat(1024);
        assert!(deep.len() > 4 * 1024 && deep.len() < 16 * 1024);
        assert_eq!(
            sanitized_cwd(&deep),
            Some(deep.as_str()),
            "a cwd the shared writer accepts must survive ember's filter"
        );
        // 超过共享上限的仍然拒绝，而且拒绝的理由与控制字符/欺骗字符一致。
        assert_eq!(sanitized_cwd(&"x".repeat(MAX_HISTORY_CWD_BYTES + 1)), None);
        assert_eq!(sanitized_cwd("/tmp/\u{202e}gnp.sh"), None);
        assert_eq!(sanitized_cwd("/tmp/a\nb"), None);
        assert_eq!(sanitized_cwd("/tmp/\u{fffd}spoof"), None);
    }

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ember-history-picker-{label}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(path)
        }

        fn join(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_private(path: &std::path::Path, contents: impl AsRef<[u8]>) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn record(command: &str, cwd: Option<&str>, exit_code: i32) -> CommandHistoryRecord {
        CommandHistoryRecord {
            command: command.to_string(),
            cwd: cwd.map(str::to_string),
            exit_code,
            end_time_ms: None,
        }
    }

    #[test]
    fn sanitized_command_trims_and_rejects_unsafe_text() {
        assert_eq!(sanitized_command("  cargo test  "), Some("cargo test"));
        assert_eq!(sanitized_command(""), None);
        assert_eq!(sanitized_command("   "), None);
        // 多行重建文本（heredoc）无法回填到单行提示符，家族的 review-only
        // 历史格式也拒绝控制字节。
        assert_eq!(sanitized_command("cat <<EOF\nhello\nEOF"), None);
        assert_eq!(sanitized_command("printf \u{7}"), None);
        assert_eq!(sanitized_command("printf safe\u{202e}txt"), None);
        assert_eq!(sanitized_command("printf ok\u{fffd}"), None);
        assert_eq!(sanitized_command("echo\u{00a0}not-a-separator"), None);
        assert_eq!(
            sanitized_command(&"x".repeat(MAX_SHARED_HISTORY_COMMAND_BYTES + 1)),
            None
        );
    }

    #[test]
    fn the_shared_history_command_budget_is_the_core_writers_own() {
        // 上限必须来自共享文件的写入契约，而不是 ember 自己的 OSC 133 重放
        // 预算。之前用 64 KiB：兄弟终端按核心的 256 KiB 合法写入的记录，在
        // ember 这一侧被整条丢掉，既不显示也不参与模糊匹配。
        //
        // 写成字面量而不是它自己的定义式：这个常量*就是*
        // `jterm_core::review_input::MAX_REVIEW_INPUT_BYTES`，两者相比是
        // `assert_eq!(x, x)`，对核心将来采用的任何值都成立。写死数字，核心
        // 挪动预算时这里会红，而不是无声跟随。
        assert_eq!(MAX_SHARED_HISTORY_COMMAND_BYTES, 256 * 1024);
        let over_embers_old_budget = format!("echo {}", "x".repeat(100 * 1024));
        assert!(over_embers_old_budget.len() > 64 * 1024);
        assert_eq!(
            sanitized_command(&over_embers_old_budget),
            Some(over_embers_old_budget.as_str())
        );
        // 显示侧用同一个上限，否则一条可召回的记录会显示成 "(command
        // omitted)"，用户看得到却读不懂。
        assert!(!display_command(&over_embers_old_budget).contains("omitted"));

        let state = HistoryPickerState::new(vec![record(&over_embers_old_budget, None, 0)]);
        assert_eq!(state.entries.len(), 1);
        // 召回那一步在 `app::commands` 里钉住
        // （`a_shared_history_row_the_picker_lists_is_one_the_prompt_will_accept`）：
        // 展示这一条却在回填时条条拒绝，比过去直接不展示更糟。
    }

    #[test]
    fn display_command_truncates_only_over_long_lines() {
        assert_eq!(display_command("cargo test"), "cargo test");
        let long = "x".repeat(500);
        let shown = display_command(&long);
        assert_eq!(shown.chars().count(), 120);
        assert!(shown.ends_with('…'));
        assert_eq!(display_command("safe\u{202e}hidden"), "safe\\u{202E}hidden");
    }

    #[test]
    fn constructor_drops_unsafe_commands_and_untrusted_cwds() {
        let state = HistoryPickerState::new(vec![
            record("echo safe\u{2066}hidden", Some("/tmp"), 0),
            record("cargo test", Some("/tmp/\u{202e}spoof"), 0),
        ]);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].command, "cargo test");
        assert_eq!(state.entries[0].cwd, None);
        assert_eq!(state.selected_command().as_deref(), Some("cargo test"));
    }

    #[test]
    fn empty_query_keeps_newest_first_order() {
        let state = HistoryPickerState::new(vec![
            record("newest", None, 0),
            record("middle", None, 0),
            record("oldest", None, 0),
        ]);
        let commands: Vec<&str> = state
            .filtered()
            .iter()
            .map(|r| r.command.as_str())
            .collect();
        assert_eq!(commands, vec!["newest", "middle", "oldest"]);
    }

    #[test]
    fn fuzzy_query_drops_non_matches_and_ranks_ties_by_recency() {
        let mut state = HistoryPickerState::new(vec![
            record("cargo test", None, 0),
            record("git status", None, 0),
            record("cargo test", None, 1),
        ]);
        state.set_query("cargo");
        let filtered = state.filtered();
        assert_eq!(filtered.len(), 2);
        // 相同 haystack 得分相同；稳定排序保持较新的记录在前。
        assert_eq!(filtered[0].exit_code, 0);
        assert_eq!(filtered[1].exit_code, 1);
    }

    #[test]
    fn cached_history_preserves_selection_and_only_rescores_changed_queries() {
        let mut state = HistoryPickerState::new(vec![
            record("cargo test", None, 0),
            record("cargo build", Some("/work/ember"), 1),
            record("git status", None, 0),
        ]);
        assert_eq!(state.rebuild_count, 1);
        state.set_query("cargo");
        assert_eq!(state.rebuild_count, 2);
        state.select_next();
        assert_eq!(state.selected, 1);
        for _ in 0..20 {
            assert_eq!(state.filtered().len(), 2);
        }
        assert!(state.selected_command().is_some());
        state.set_query("cargo\n");
        assert_eq!(
            state.selected, 1,
            "same normalized query preserves navigation"
        );
        *state.query_buffer_mut() = "cargo".into();
        state.sync_query();
        assert_eq!(state.selected, 1);
        assert_eq!(
            state.rebuild_count, 2,
            "idle reads, navigation and sync do not rescore"
        );
        state.set_query("git");
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_command().as_deref(), Some("git status"));
        assert_eq!(state.rebuild_count, 3);
        state.set_query("git\u{202e}");
        assert!(state.filtered().is_empty());
        assert_eq!(state.selected_command(), None);
    }

    #[test]
    fn history_scroll_intent_is_one_shot_and_pointer_does_not_recenter() {
        let mut state =
            HistoryPickerState::new(vec![record("one", None, 0), record("two", None, 0)]);
        assert!(state.take_scroll_to_selected());
        assert!(!state.take_scroll_to_selected());
        state.select_next();
        assert!(state.take_scroll_to_selected());
        assert!(!state.take_scroll_to_selected());
        state.select_hovered(0);
        assert!(!state.take_scroll_to_selected());
        state.select_hovered(100);
        assert_eq!(state.selected, 0);
        state.set_query("two");
        assert!(state.take_scroll_to_selected());
        state.set_query("two");
        assert!(!state.take_scroll_to_selected());
    }

    #[test]
    fn history_fuzzy_work_budget_keeps_full_text_and_normal_ranking() {
        assert!(!use_linear_history_match(
            &"x".repeat(256),
            &"x".repeat(256)
        ));
        assert!(use_linear_history_match(&"x".repeat(257), &"x".repeat(256)));
        let command = format!(
            "{} target",
            "a".repeat(MAX_SHARED_HISTORY_COMMAND_BYTES - 7)
        );
        let cwd = format!("/{}目录", "d".repeat(MAX_HISTORY_CWD_BYTES - 7));
        let mut state = HistoryPickerState::new(vec![
            record(&command, Some(&cwd), 0),
            record(&command, Some(&cwd), 1),
        ]);
        state.set_query(format!("{} target", "a".repeat(512)));
        assert_eq!(state.filtered().len(), 2);
        assert_eq!(state.filtered()[0].exit_code, 0, "ties retain recency");
        assert_eq!(state.selected_command().as_deref(), Some(command.as_str()));
        state.set_query("target 目录");
        assert_eq!(state.filtered().len(), 2, "matching spans command and cwd");
        state.set_query("TARGET");
        assert!(state.filtered().is_empty(), "smart case is unchanged");
        let mut state = HistoryPickerState::new(vec![
            record("cargo test", None, 0),
            record("cat config", None, 0),
            record("git status", None, 0),
        ]);
        state.set_query("ct");
        let matcher = SkimMatcherV2::default();
        let mut expected: Vec<_> = state
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                matcher
                    .fuzzy_match(&row.command, "ct")
                    .map(|score| (score, i))
            })
            .collect();
        expected.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        assert_eq!(
            state.results,
            expected.into_iter().map(|(_, i)| i).collect::<Vec<_>>()
        );
    }

    #[test]
    fn linear_fallback_changes_ranking_not_match_membership() {
        let exact = SkimMatcherV2::default();
        let linear = SkimMatcherV2::default().element_limit(1);
        let choices = [
            "",
            "cargo test",
            "CARGO test",
            "a___b__c",
            "aaabaaaab",
            "目录/项目",
            "İstanbul Straße",
            "编译🙂终端",
            "foo/bar.rs",
            "foo BAR baz",
            " /é/É/ ",
        ];
        let patterns = [
            "",
            "ct",
            "CT",
            "ab",
            "aaaa",
            "目录",
            "目项",
            "🙂端",
            "İS",
            "st",
            "ß",
            "fb",
            "fB",
            " ",
            "éÉ",
            "unmatched",
        ];
        for choice in choices {
            for pattern in patterns {
                assert_eq!(
                    exact.fuzzy_match(choice, pattern).is_some(),
                    linear.fuzzy_match(choice, pattern).is_some(),
                    "choice {choice:?}, pattern {pattern:?}",
                );
            }
        }
    }

    #[test]
    fn cwd_participates_in_matching() {
        let mut state = HistoryPickerState::new(vec![
            record("make -j8", Some("/home/u/myproj"), 0),
            record("make -j8", Some("/home/u/other"), 0),
        ]);
        state.set_query("myproj");
        let filtered = state.filtered();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].cwd.as_deref(), Some("/home/u/myproj"));
    }

    #[test]
    fn history_query_is_bounded_and_rewritten_queries_do_not_match() {
        let mut state = HistoryPickerState::new(vec![record("cargo test", None, 0)]);
        state.set_query("cargo\n\u{1b}");
        assert_eq!(state.query, "cargo");
        assert_eq!(state.filtered().len(), 1);
        state.set_query("cargo\u{202e}");
        assert_eq!(state.query, "cargo\u{fffd}");
        assert!(state.filtered().is_empty());
        state.set_query(format!("{}z", "x".repeat(MAX_HISTORY_QUERY_BYTES)));
        assert_eq!(state.query.len(), MAX_HISTORY_QUERY_BYTES);
        assert!(!state.query.contains('z'));
    }

    #[test]
    fn results_are_capped_so_navigation_matches_the_drawn_list() {
        let entries = (0..MAX_RESULTS + 5)
            .map(|i| record(&format!("command-{i}"), None, 0))
            .collect();
        let mut state = HistoryPickerState::new(entries);
        assert_eq!(state.filtered().len(), MAX_RESULTS);

        state.select_prev();
        assert_eq!(state.selected, MAX_RESULTS - 1);
        state.select_next();
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_command().as_deref(), Some("command-0"));
    }

    #[test]
    fn load_reads_a_bounded_newest_first_slice() {
        let root = TestDir::new("bounded-load");
        let path = root.join("history.jsonl");
        let mut contents = String::new();
        for i in 0..PICKER_MAX_ENTRIES + 10 {
            contents.push_str(&format!("{{\"command\":\"cmd-{i}\",\"exit_code\":0}}\n"));
        }
        write_private(&path, contents);

        let state = HistoryPickerState::load(&path);

        assert_eq!(state.entries.len(), PICKER_MAX_ENTRIES);
        assert_eq!(
            state.entries.first().map(|r| r.command.as_str()),
            Some(format!("cmd-{}", PICKER_MAX_ENTRIES + 9).as_str())
        );
    }

    #[test]
    fn load_of_a_missing_file_yields_an_empty_picker() {
        let root = TestDir::new("missing-file");
        let path = root.join("history.jsonl");
        let state = HistoryPickerState::load(&path);
        assert!(state.filtered().is_empty());
        assert_eq!(state.selected_command(), None);
    }
}
