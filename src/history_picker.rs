//! 历史命令选择器：跨重启持久化命令的模糊搜索浮层（Ctrl+Shift+H 打开）。
//!
//! 记录与检索都建立在家族共享的 `jterm_core::command_history` JSONL 索引上
//! （与 anvil/forge/frost 同名配置键、同文件格式），因此几个兄弟终端可以
//! 指向同一份历史文件。Enter 只把选中的命令回填到活动 pane 的提示符，
//! 从不执行。
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use jterm_core::command_history::{self, CommandHistoryRecord};

/// 打开选择器时加载的最大条数。与 forge/frost 的历史面板一致：交互检索只
/// 需要一个近期工作集，`read_recent` 本身也把读取限制在有界的文件尾部。
pub const PICKER_MAX_ENTRIES: usize = 2_000;

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

/// 把一条 OSC 133 重建的命令行修剪并校验为可持久化文本。返回 `None` 表示
/// 不应写入历史：空白命令，或含换行/控制字符的重建文本（例如 heredoc 的
/// 多行命令）——家族的 review-only 历史格式拒绝控制字符，这类文本也无法
/// 安全地回填到提示符。
pub fn sanitized_command(command: &str) -> Option<&str> {
    let trimmed = command.trim_matches(' ');
    crate::review_text::validate_single_line(trimmed, crate::review_text::MAX_HISTORY_COMMAND_BYTES)
        .ok()
}

pub fn sanitized_cwd(cwd: &str) -> Option<&str> {
    if cwd.len() > MAX_HISTORY_CWD_BYTES
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
    if command.len() > crate::review_text::MAX_HISTORY_COMMAND_BYTES {
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
    pub query: String,
    /// 当前过滤结果中的高亮位置。
    pub selected: usize,
    /// 是否需要聚焦搜索框（egui 文本框在浮层打开后的第一帧取焦）。
    pub needs_focus: bool,
    entries: Vec<CommandHistoryRecord>,
    matcher: SkimMatcherV2,
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
        Self {
            query: String::new(),
            selected: 0,
            needs_focus: true,
            entries,
            matcher: SkimMatcherV2::default(),
        }
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
        if self.query.is_empty() {
            return self.entries.iter().take(MAX_RESULTS).collect();
        }
        let mut scored: Vec<(i64, &CommandHistoryRecord)> = self
            .entries
            .iter()
            .filter_map(|record| {
                let haystack = match record.cwd.as_deref() {
                    Some(cwd) => format!("{} {cwd}", record.command),
                    None => record.command.clone(),
                };
                self.matcher
                    .fuzzy_match(&haystack, &self.query)
                    .map(|score| (score, record))
            })
            .collect();
        scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(_, record)| record)
            .collect()
    }

    /// 高亮项下移（在过滤结果中循环）。
    pub fn select_next(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = (self.selected + 1) % len;
        }
    }

    /// 高亮项上移（在过滤结果中循环）。
    pub fn select_prev(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = if self.selected == 0 {
                len - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// 当前高亮的命令文本（按过滤结果中的位置）。
    pub fn selected_command(&self) -> Option<String> {
        self.filtered()
            .get(self.selected)
            .and_then(|record| sanitized_command(&record.command))
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
        assert_eq!(sanitized_command("echo\u{00a0}not-a-separator"), None);
        assert_eq!(
            sanitized_command(&"x".repeat(crate::review_text::MAX_HISTORY_COMMAND_BYTES + 1)),
            None
        );
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
        let state = HistoryPickerState {
            query: "cargo".to_string(),
            ..HistoryPickerState::new(vec![
                record("cargo test", None, 0),
                record("git status", None, 0),
                record("cargo test", None, 1),
            ])
        };
        let filtered = state.filtered();
        assert_eq!(filtered.len(), 2);
        // 相同 haystack 得分相同；稳定排序保持较新的记录在前。
        assert_eq!(filtered[0].exit_code, 0);
        assert_eq!(filtered[1].exit_code, 1);
    }

    #[test]
    fn cwd_participates_in_matching() {
        let state = HistoryPickerState {
            query: "myproj".to_string(),
            ..HistoryPickerState::new(vec![
                record("make -j8", Some("/home/u/myproj"), 0),
                record("make -j8", Some("/home/u/other"), 0),
            ])
        };
        let filtered = state.filtered();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].cwd.as_deref(), Some("/home/u/myproj"));
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
