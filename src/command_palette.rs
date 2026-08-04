use fuzzy_matcher::skim::SkimMatcherV2;
/// 命令调色板模块
use fuzzy_matcher::FuzzyMatcher;
use std::collections::VecDeque;

/// 命令类别
#[derive(Clone, Debug, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommandCategory {
    Session,
    Edit,
    Search,
    Terminal,
    Window,
    Config,
}

impl std::fmt::Display for CommandCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandCategory::Session => write!(f, "Session"),
            CommandCategory::Edit => write!(f, "Edit"),
            CommandCategory::Search => write!(f, "Search"),
            CommandCategory::Terminal => write!(f, "Terminal"),
            CommandCategory::Window => write!(f, "Window"),
            CommandCategory::Config => write!(f, "Config"),
        }
    }
}

/// 单个命令的信息
#[derive(Clone, Debug)]
pub struct CommandInfo {
    /// 命令名称（显示用）
    pub name: String,
    /// 命令类别
    pub category: CommandCategory,
    /// 命令描述
    pub description: String,
    /// 实际执行的命令
    pub command: crate::keybindings::Command,
}

impl CommandInfo {
    pub fn new(
        name: &str,
        category: CommandCategory,
        description: &str,
        command: crate::keybindings::Command,
    ) -> Self {
        Self {
            name: name.to_string(),
            category,
            description: description.to_string(),
            command,
        }
    }
}

/// 命令调色板
pub struct CommandPalette {
    /// 所有可用的命令
    all_commands: Vec<CommandInfo>,
    /// 搜索结果（包含匹配分数）
    search_results: Vec<(CommandInfo, i64)>,
    /// 搜索输入
    pub search_query: String,
    /// 当前选中的命令索引
    pub selected_index: usize,
    /// 是否打开
    pub is_open: bool,
    /// 是否需要聚焦搜索框
    pub needs_focus: bool,
    /// 最近使用的命令
    recent_commands: VecDeque<crate::keybindings::Command>,
    /// 最大保存的最近使用命令数
    max_recent: usize,
    /// 模糊匹配器
    matcher: SkimMatcherV2,
}

impl CommandPalette {
    pub fn new() -> Self {
        let matcher = SkimMatcherV2::default();
        let all_commands = Self::build_commands();

        Self {
            all_commands,
            search_results: Vec::new(),
            search_query: String::new(),
            selected_index: 0,
            is_open: false,
            needs_focus: false,
            recent_commands: VecDeque::new(),
            max_recent: 10,
            matcher,
        }
    }

    /// 所有可用命令（供帮助面板等枚举使用）
    pub fn all_commands(&self) -> &[CommandInfo] {
        &self.all_commands
    }

    /// 构建所有可用命令列表
    fn build_commands() -> Vec<CommandInfo> {
        let mut commands = vec![
            // === 会话管理 ===
            CommandInfo::new(
                "New Tab",
                CommandCategory::Session,
                "Open a new tab with a fresh terminal session",
                crate::keybindings::Command::SessionNew,
            ),
            CommandInfo::new(
                "Close Tab",
                CommandCategory::Session,
                "Close the current tab and every pane in it",
                crate::keybindings::Command::SessionClose,
            ),
            CommandInfo::new(
                "Next Tab",
                CommandCategory::Session,
                "Switch to the next tab",
                crate::keybindings::Command::SessionNext,
            ),
            CommandInfo::new(
                "Previous Tab",
                CommandCategory::Session,
                "Switch to the previous tab",
                crate::keybindings::Command::SessionPrev,
            ),
            CommandInfo::new(
                "Last Tab",
                CommandCategory::Session,
                "Switch to the last tab",
                crate::keybindings::Command::SessionLast,
            ),
            CommandInfo::new(
                "Last Active Session",
                CommandCategory::Session,
                "Switch back to the most recently focused pane, in whichever tab it lives",
                crate::keybindings::Command::SessionPrevActive,
            ),
            // === 编辑操作 ===
            CommandInfo::new(
                "Copy",
                CommandCategory::Edit,
                "Copy selected text to clipboard",
                crate::keybindings::Command::EditCopy,
            ),
            CommandInfo::new(
                "Paste",
                CommandCategory::Edit,
                "Paste from clipboard",
                crate::keybindings::Command::EditPaste,
            ),
            // === 搜索操作 ===
            CommandInfo::new(
                "Open Search",
                CommandCategory::Search,
                "Open the search panel",
                crate::keybindings::Command::SearchOpen,
            ),
            CommandInfo::new(
                "Close Search",
                CommandCategory::Search,
                "Close the search panel",
                crate::keybindings::Command::SearchClose,
            ),
            CommandInfo::new(
                "Search Next",
                CommandCategory::Search,
                "Jump to next search match",
                crate::keybindings::Command::SearchNext,
            ),
            CommandInfo::new(
                "Search Previous",
                CommandCategory::Search,
                "Jump to previous search match",
                crate::keybindings::Command::SearchPrev,
            ),
            CommandInfo::new(
                "Previous Search History",
                CommandCategory::Search,
                "Load the previous search query",
                crate::keybindings::Command::SearchHistoryPrev,
            ),
            CommandInfo::new(
                "Next Search History",
                CommandCategory::Search,
                "Load the next search query",
                crate::keybindings::Command::SearchHistoryNext,
            ),
            CommandInfo::new(
                "Find and Replace",
                CommandCategory::Search,
                "Open find-and-replace on the selection",
                crate::keybindings::Command::SearchReplaceToggle,
            ),
            // === 终端操作 ===
            CommandInfo::new(
                "Scroll Up",
                CommandCategory::Terminal,
                "Scroll terminal output up",
                crate::keybindings::Command::TerminalScrollUp,
            ),
            CommandInfo::new(
                "Scroll Down",
                CommandCategory::Terminal,
                "Scroll terminal output down",
                crate::keybindings::Command::TerminalScrollDown,
            ),
            CommandInfo::new(
                "Clear Screen",
                CommandCategory::Terminal,
                "Clear the terminal screen",
                crate::keybindings::Command::TerminalClear,
            ),
            CommandInfo::new(
                "Send Interrupt",
                CommandCategory::Terminal,
                "Send Ctrl+C to the terminal",
                crate::keybindings::Command::TerminalSendSigint,
            ),
            CommandInfo::new(
                "Send End of File",
                CommandCategory::Terminal,
                "Send Ctrl+D to the terminal",
                crate::keybindings::Command::TerminalSendEof,
            ),
            CommandInfo::new(
                "Previous Command Mark",
                CommandCategory::Terminal,
                "Jump to the previous shell command mark",
                crate::keybindings::Command::TerminalJumpPrevMark,
            ),
            CommandInfo::new(
                "Next Command Mark",
                CommandCategory::Terminal,
                "Jump to the next shell command mark",
                crate::keybindings::Command::TerminalJumpNextMark,
            ),
            // === 命令块（block mode）===
            CommandInfo::new(
                "Jump to First Failed Block",
                CommandCategory::Terminal,
                "Select and reveal the oldest command that exited nonzero",
                crate::keybindings::Command::BlockJumpFirstFailed,
            ),
            CommandInfo::new(
                "Copy Block Command",
                CommandCategory::Terminal,
                "Copy the selected (or most recent) command block's command text",
                crate::keybindings::Command::BlockCopyCommand,
            ),
            CommandInfo::new(
                "Copy Block Output",
                CommandCategory::Terminal,
                "Copy the selected (or most recent) command block's output",
                crate::keybindings::Command::BlockCopyOutput,
            ),
            CommandInfo::new(
                "Recall Block Command",
                CommandCategory::Terminal,
                "Insert the selected (or latest) command at the prompt without running it",
                crate::keybindings::Command::BlockRecallCommand,
            ),
            CommandInfo::new(
                "Split Vertically",
                CommandCategory::Terminal,
                "Create a terminal pane side by side",
                crate::keybindings::Command::TerminalSplitVertical,
            ),
            CommandInfo::new(
                "Split Horizontally",
                CommandCategory::Terminal,
                "Create a terminal pane above or below",
                crate::keybindings::Command::TerminalSplitHorizontal,
            ),
            CommandInfo::new(
                "Close Pane",
                CommandCategory::Terminal,
                "Close the focused pane and its shell session",
                crate::keybindings::Command::TerminalClosePane,
            ),
            CommandInfo::new(
                "Focus Next Pane",
                CommandCategory::Terminal,
                "Move keyboard focus to the next pane",
                crate::keybindings::Command::PaneFocusNext,
            ),
            CommandInfo::new(
                "Focus Previous Pane",
                CommandCategory::Terminal,
                "Move keyboard focus to the previous pane",
                crate::keybindings::Command::PaneFocusPrev,
            ),
            CommandInfo::new(
                "Focus Pane Left",
                CommandCategory::Terminal,
                "Focus the pane physically to the left",
                crate::keybindings::Command::PaneFocusLeft,
            ),
            CommandInfo::new(
                "Focus Pane Right",
                CommandCategory::Terminal,
                "Focus the pane physically to the right",
                crate::keybindings::Command::PaneFocusRight,
            ),
            CommandInfo::new(
                "Focus Pane Up",
                CommandCategory::Terminal,
                "Focus the pane physically above",
                crate::keybindings::Command::PaneFocusUp,
            ),
            CommandInfo::new(
                "Focus Pane Down",
                CommandCategory::Terminal,
                "Focus the pane physically below",
                crate::keybindings::Command::PaneFocusDown,
            ),
            CommandInfo::new(
                "Resize Divider Left",
                CommandCategory::Terminal,
                "Move a vertical pane divider left",
                crate::keybindings::Command::PaneResizeLeft,
            ),
            CommandInfo::new(
                "Resize Divider Right",
                CommandCategory::Terminal,
                "Move a vertical pane divider right",
                crate::keybindings::Command::PaneResizeRight,
            ),
            CommandInfo::new(
                "Resize Divider Up",
                CommandCategory::Terminal,
                "Move a horizontal pane divider up",
                crate::keybindings::Command::PaneResizeUp,
            ),
            CommandInfo::new(
                "Resize Divider Down",
                CommandCategory::Terminal,
                "Move a horizontal pane divider down",
                crate::keybindings::Command::PaneResizeDown,
            ),
            CommandInfo::new(
                "Toggle Pane Zoom",
                CommandCategory::Terminal,
                "Temporarily maximize or restore the focused pane",
                crate::keybindings::Command::PaneZoomToggle,
            ),
            CommandInfo::new(
                "Equalize Panes",
                CommandCategory::Terminal,
                "Reset every pane divider to an even split",
                crate::keybindings::Command::PaneEqualize,
            ),
            CommandInfo::new(
                "Increase Font Size",
                CommandCategory::Terminal,
                "Make terminal text larger",
                crate::keybindings::Command::FontIncrease,
            ),
            CommandInfo::new(
                "Decrease Font Size",
                CommandCategory::Terminal,
                "Make terminal text smaller",
                crate::keybindings::Command::FontDecrease,
            ),
            CommandInfo::new(
                "Reset Font Size",
                CommandCategory::Terminal,
                "Restore the default terminal text size",
                crate::keybindings::Command::FontReset,
            ),
            // === 窗口操作 ===
            CommandInfo::new(
                "Increase Opacity",
                CommandCategory::Window,
                "Make the window background more opaque",
                crate::keybindings::Command::OpacityIncrease,
            ),
            CommandInfo::new(
                "Decrease Opacity",
                CommandCategory::Window,
                "Make the window background more transparent",
                crate::keybindings::Command::OpacityDecrease,
            ),
            CommandInfo::new(
                "Close Window",
                CommandCategory::Window,
                "Close the entire application",
                crate::keybindings::Command::WindowClose,
            ),
            CommandInfo::new(
                "Toggle Sidebar",
                CommandCategory::Window,
                "Show/hide the file tree sidebar",
                crate::keybindings::Command::SidebarToggle,
            ),
            CommandInfo::new(
                "Toggle AI Agent",
                CommandCategory::Window,
                "Open/close the AI agent panel (per-command approval)",
                crate::keybindings::Command::AgentToggle,
            ),
            CommandInfo::new(
                "Connect to remote host",
                CommandCategory::Session,
                "Open a [[remote_hosts]] destination (ssh or container) in a new session",
                crate::keybindings::Command::RemotePicker,
            ),
            CommandInfo::new(
                "Install or update jsh",
                CommandCategory::Terminal,
                "Install jterm's companion shell, or update the installed one, in a new session",
                crate::keybindings::Command::JshInstall,
            ),
            CommandInfo::new(
                "Toggle Command Palette",
                CommandCategory::Window,
                "Open or close the command palette",
                crate::keybindings::Command::CommandPaletteToggle,
            ),
            CommandInfo::new(
                "Keyboard Shortcuts",
                CommandCategory::Window,
                "Open or close the keyboard shortcut reference",
                crate::keybindings::Command::HelpToggle,
            ),
            // === 配置 ===
            CommandInfo::new(
                "Open Settings",
                CommandCategory::Config,
                "Open the settings panel",
                crate::keybindings::Command::ConfigOpen,
            ),
            CommandInfo::new(
                "Close Settings",
                CommandCategory::Config,
                "Close the settings panel",
                crate::keybindings::Command::ConfigClose,
            ),
            CommandInfo::new(
                "Toggle Settings",
                CommandCategory::Config,
                "Open or close the settings panel",
                crate::keybindings::Command::ConfigToggle,
            ),
            CommandInfo::new(
                "Toggle Debug Overlay",
                CommandCategory::Config,
                "Show or hide renderer diagnostics",
                crate::keybindings::Command::DebugToggle,
            ),
        ];

        for index in 0..8 {
            commands.push(CommandInfo::new(
                &format!("Go to Session {}", index + 1),
                CommandCategory::Session,
                "Switch directly to a numbered session",
                crate::keybindings::Command::SessionJump(index),
            ));
        }

        commands
    }

    /// 打开调色板
    pub fn open(&mut self) {
        self.is_open = true;
        self.needs_focus = true;
        self.search_query.clear();
        self.selected_index = 0;
        self.update_search_results();
    }

    /// 关闭调色板
    pub fn close(&mut self) {
        self.is_open = false;
    }

    /// 更新搜索结果
    pub fn update_search_results(&mut self) {
        self.search_results.clear();
        self.selected_index = 0;

        if self.search_query.is_empty() {
            // 如果没有搜索词，优先显示最近使用的命令
            if !self.recent_commands.is_empty() {
                // 最近使用的命令
                for recent_cmd in self.recent_commands.iter().take(5) {
                    if let Some(cmd_info) =
                        self.all_commands.iter().find(|c| c.command == *recent_cmd)
                    {
                        self.search_results.push((cmd_info.clone(), 100));
                    }
                }
            }

            // 然后显示所有其他命令（按分类排序）
            let mut other_commands = self.all_commands.clone();
            other_commands.retain(|cmd| {
                !self
                    .recent_commands
                    .iter()
                    .any(|recent| recent == &cmd.command)
            });
            other_commands.sort_by_key(|cmd| cmd.category);
            for cmd in other_commands {
                self.search_results.push((cmd, 50));
            }
        } else {
            // 使用模糊匹配
            for cmd in &self.all_commands {
                let search_str = format!("{} {}", cmd.name, cmd.description);
                if let Some(score) = self.matcher.fuzzy_match(&search_str, &self.search_query) {
                    self.search_results.push((cmd.clone(), score));
                }
            }

            // 按分数从高到低排序
            self.search_results
                .sort_by_key(|result| std::cmp::Reverse(result.1));
        }
    }

    /// 选择下一个命令
    pub fn select_next(&mut self) {
        if !self.search_results.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.search_results.len();
        }
    }

    /// 选择上一个命令
    pub fn select_prev(&mut self) {
        if !self.search_results.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.search_results.len() - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    /// 获取当前选中的命令
    pub fn get_selected_command(&self) -> Option<crate::keybindings::Command> {
        self.search_results
            .get(self.selected_index)
            .map(|(cmd_info, _)| cmd_info.command.clone())
    }

    /// 执行命令（记录到最近使用）
    pub fn execute_command(&mut self, command: crate::keybindings::Command) {
        self.recent_commands.retain(|cmd| cmd != &command);
        self.recent_commands.push_front(command);
        while self.recent_commands.len() > self.max_recent {
            self.recent_commands.pop_back();
        }
    }

    /// 获取所有搜索结果（用于 UI 显示）
    pub fn get_results(&self) -> &[(CommandInfo, i64)] {
        &self.search_results
    }

    /// 持久化用:取出最近命令快照(最近的在前)。
    pub fn recent_commands_snapshot(&self) -> Vec<crate::keybindings::Command> {
        self.recent_commands.iter().cloned().collect()
    }

    /// 持久化恢复:从磁盘读出的列表(最近的在前)灌回内部队列,超长则截断。
    /// 校验上限避免被手改后的脏数据撑大内存。
    pub fn restore_recent_commands(&mut self, list: Vec<crate::keybindings::Command>) {
        self.recent_commands.clear();
        for cmd in list.into_iter().take(self.max_recent) {
            self.recent_commands.push_back(cmd);
        }
    }
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_palette_open() {
        let mut palette = CommandPalette::new();
        assert!(!palette.is_open);
        palette.open();
        assert!(palette.is_open);
    }

    #[test]
    fn test_command_palette_search() {
        let mut palette = CommandPalette::new();
        palette.search_query = "session".to_string();
        palette.update_search_results();
        assert!(!palette.search_results.is_empty());
        // 应该找到关于 session 的命令
        assert!(palette
            .search_results
            .iter()
            .any(|(cmd, _)| { cmd.name.to_lowercase().contains("session") }));
    }

    #[test]
    fn test_recent_commands() {
        let mut palette = CommandPalette::new();
        palette.execute_command(crate::keybindings::Command::SessionNew);
        palette.execute_command(crate::keybindings::Command::EditCopy);

        assert_eq!(palette.recent_commands.len(), 2);
        assert_eq!(
            palette.recent_commands[0],
            crate::keybindings::Command::EditCopy
        );
    }

    #[test]
    fn test_selection_navigation() {
        let mut palette = CommandPalette::new();
        palette.open();

        let initial_idx = palette.selected_index;
        palette.select_next();
        assert!(palette.selected_index >= initial_idx || palette.search_results.len() <= 1);
    }

    #[test]
    fn every_command_variant_is_discoverable() {
        use crate::keybindings::Command;

        let palette = CommandPalette::new();
        let represented = |command: &Command| {
            palette
                .all_commands()
                .iter()
                .any(|entry| &entry.command == command)
        };
        let fixed_commands = [
            Command::SessionNew,
            Command::SessionClose,
            Command::SessionNext,
            Command::SessionPrev,
            Command::SessionLast,
            Command::SessionPrevActive,
            Command::EditCopy,
            Command::EditPaste,
            Command::SearchOpen,
            Command::SearchClose,
            Command::SearchNext,
            Command::SearchPrev,
            Command::SearchHistoryPrev,
            Command::SearchHistoryNext,
            Command::SearchReplaceToggle,
            Command::TerminalSendSigint,
            Command::TerminalSendEof,
            Command::TerminalClear,
            Command::TerminalScrollUp,
            Command::TerminalScrollDown,
            Command::TerminalJumpPrevMark,
            Command::TerminalJumpNextMark,
            Command::BlockJumpFirstFailed,
            Command::BlockCopyCommand,
            Command::BlockCopyOutput,
            Command::BlockRecallCommand,
            Command::FontIncrease,
            Command::FontDecrease,
            Command::FontReset,
            Command::OpacityIncrease,
            Command::OpacityDecrease,
            Command::TerminalSplitVertical,
            Command::TerminalSplitHorizontal,
            Command::TerminalClosePane,
            Command::PaneFocusNext,
            Command::PaneFocusPrev,
            Command::PaneFocusLeft,
            Command::PaneFocusRight,
            Command::PaneFocusUp,
            Command::PaneFocusDown,
            Command::PaneResizeLeft,
            Command::PaneResizeRight,
            Command::PaneResizeUp,
            Command::PaneResizeDown,
            Command::PaneZoomToggle,
            Command::PaneEqualize,
            Command::WindowClose,
            Command::CommandPaletteToggle,
            Command::HelpToggle,
            Command::ConfigOpen,
            Command::ConfigClose,
            Command::ConfigToggle,
            Command::DebugToggle,
            Command::SidebarToggle,
            Command::AgentToggle,
            Command::JshInstall,
        ];

        for command in fixed_commands {
            assert!(represented(&command), "missing palette entry for {command}");
        }
        for index in 0..8 {
            assert!(represented(&Command::SessionJump(index)));
        }
    }
}
