use fuzzy_matcher::skim::SkimMatcherV2;
/// 命令调色板模块
use fuzzy_matcher::FuzzyMatcher;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PALETTE_OPENING: AtomicU64 = AtomicU64::new(1);

fn allocate_palette_opening(counter: &AtomicU64) -> u64 {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .expect("palette opening identity exhausted");
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return current,
            Err(observed) => current = observed,
        }
    }
}

/// Ask-AI request budget, enforced fail-closed at the palette entry (forge's
/// `MAX_AI_QUERY_BYTES`). The number is core's own `MAX_USER_PROMPT_BYTES`
/// (jterm_core/src/ai/mod.rs:41), which is private: past it `sample_output`
/// elides the middle of the request, so an unbounded request reaches the
/// model with a hole in it and the review card presents the resulting command
/// as an ordinary suggestion. Refusing is the only honest answer.
pub const MAX_AI_QUERY_BYTES: usize = 64 * 1024;
/// One-line overlay query budget, shared with the workflow and history
/// pickers so a paste cannot grow the field and the fuzzy haystack without bound.
pub(crate) const MAX_PALETTE_QUERY_BYTES: usize = jterm_core::workflows::MAX_PICKER_QUERY_BYTES;

fn palette_query_is_unsafe(query: &str) -> bool {
    query.contains('\u{fffd}') || jterm_core::review_input::contains_visual_spoofing(query)
}

fn bound_palette_query(query: impl Into<String>, max_bytes: usize) -> String {
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
    if query.len() > max_bytes {
        let mut end = max_bytes;
        while end > 0 && !query.is_char_boundary(end) {
            end -= 1;
        }
        query.truncate(end);
    }
    query
}

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
    opening: u64,
    /// 所有可用的命令
    all_commands: Vec<CommandInfo>,
    pub(crate) organism_target: Option<String>,
    pub(crate) query_widget_id: Option<egui::Id>,
    /// 搜索结果（包含匹配分数）
    search_results: Vec<(CommandInfo, i64)>,
    /// 搜索输入
    pub search_query: String,
    /// An oversized Ask-AI edit remains invalid even though its display is bounded.
    ask_ai_input_too_large: bool,
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
            opening: 0,
            all_commands,
            organism_target: None,
            query_widget_id: None,
            search_results: Vec::new(),
            search_query: String::new(),
            ask_ai_input_too_large: false,
            selected_index: 0,
            is_open: false,
            needs_focus: false,
            recent_commands: VecDeque::new(),
            max_recent: 10,
            matcher,
        }
    }

    /// Stable for one opening; no command or user query appears in this key.
    pub fn window_id(&self) -> egui::Id {
        egui::Id::new(("command_palette", self.opening))
    }

    /// Gesture identity follows the complete action, not its visible row index.
    pub fn command_row_id(&self, command: &crate::keybindings::Command) -> egui::Id {
        self.window_id().with(("command", command))
    }

    /// A changed request cannot inherit an earlier Ask-AI button press.
    pub fn ask_ai_row_id(&self, request: &str) -> egui::Id {
        self.window_id().with(("ask_ai", request))
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
            CommandInfo::new(
                "Search Command History",
                CommandCategory::Search,
                "Fuzzy-search persisted shell commands; Enter fills the prompt without running it",
                crate::keybindings::Command::HistoryPickerToggle,
            ),
            CommandInfo::new(
                "Open Workflows",
                CommandCategory::Search,
                "Fuzzy-search parameterized command templates; Enter fills the prompt without running it",
                crate::keybindings::Command::WorkflowPickerToggle,
            ),
            // === 终端操作 ===
            CommandInfo::new(
                "Scroll Up",
                CommandCategory::Terminal,
                "Scroll terminal output up; with Ctrl, start selecting command blocks from the newest one",
                crate::keybindings::Command::TerminalScrollUp,
            ),
            CommandInfo::new(
                "Scroll Down",
                CommandCategory::Terminal,
                "Scroll terminal output down; with Ctrl, move or leave the block selection",
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
                "Select Previous Block",
                CommandCategory::Terminal,
                "Move the block selection to the next older command block",
                crate::keybindings::Command::BlockSelectPrev,
            ),
            CommandInfo::new(
                "Select Next Block",
                CommandCategory::Terminal,
                "Move the block selection to the next newer command block",
                crate::keybindings::Command::BlockSelectNext,
            ),
            CommandInfo::new(
                "Select All Blocks",
                CommandCategory::Terminal,
                "Select every completed command block in the active pane",
                crate::keybindings::Command::BlockSelectAll,
            ),
            CommandInfo::new(
                "Reinput Selected Commands",
                CommandCategory::Terminal,
                "Insert selected block commands at the prompt in terminal order without running them",
                crate::keybindings::Command::BlockReinputSelectedCommands,
            ),
            CommandInfo::new(
                "Copy Block",
                CommandCategory::Terminal,
                "Copy the selected (or most recent) block's command and output as plain text",
                crate::keybindings::Command::BlockCopyBlock,
            ),
            CommandInfo::new(
                "Copy Block as Markdown",
                CommandCategory::Terminal,
                "Copy the selected (or most recent) block as a Markdown document",
                crate::keybindings::Command::BlockCopyMarkdown,
            ),
            CommandInfo::new(
                "Jump to Previous Failed Block",
                CommandCategory::Terminal,
                "Select and reveal the nearest failed block older than the selection",
                crate::keybindings::Command::BlockJumpPrevFailed,
            ),
            CommandInfo::new(
                "Jump to Next Failed Block",
                CommandCategory::Terminal,
                "Select and reveal the nearest failed block newer than the selection",
                crate::keybindings::Command::BlockJumpNextFailed,
            ),
            CommandInfo::new(
                "Search Blocks",
                CommandCategory::Terminal,
                "Search every command block's command and output in this session",
                crate::keybindings::Command::BlockSearchToggle,
            ),
            CommandInfo::new(
                "Toggle Block Bookmark",
                CommandCategory::Terminal,
                "Add or remove a bookmark on the active completed block",
                crate::keybindings::Command::BlockToggleBookmark,
            ),
            CommandInfo::new(
                "Toggle Selected Block Collapse",
                CommandCategory::Terminal,
                "Collapse or expand output of the explicitly selected block in the active pane",
                crate::keybindings::Command::BlockToggleCollapse,
            ),
            CommandInfo::new(
                "Collapse Selected Block Outputs",
                CommandCategory::Terminal,
                "Collapse exact retained output for all selected completed blocks in the active pane",
                crate::keybindings::Command::BlockCollapseSelected,
            ),
            CommandInfo::new(
                "Expand Selected Block Outputs",
                CommandCategory::Terminal,
                "Expand output for all selected completed blocks in the active pane",
                crate::keybindings::Command::BlockExpandSelected,
            ),
            CommandInfo::new(
                "Previous Block Bookmark",
                CommandCategory::Terminal,
                "Jump to the previous bookmarked block, wrapping at the start",
                crate::keybindings::Command::BlockJumpPrevBookmark,
            ),
            CommandInfo::new(
                "Next Block Bookmark",
                CommandCategory::Terminal,
                "Jump to the next bookmarked block, wrapping at the end",
                crate::keybindings::Command::BlockJumpNextBookmark,
            ),
            CommandInfo::new(
                "Clear Blocks",
                CommandCategory::Terminal,
                "Remove every completed command block in the active pane (undoable)",
                crate::keybindings::Command::BlockClear,
            ),
            CommandInfo::new(
                "Undo Clear Blocks",
                CommandCategory::Terminal,
                "Restore the blocks removed by the most recent Clear Blocks",
                crate::keybindings::Command::BlockUndoClear,
            ),
            CommandInfo::new(
                "Compare Selected Block Outputs",
                CommandCategory::Terminal,
                "Compare captured output of two completed commands in terminal order",
                crate::keybindings::Command::BlockCompareSelected,
            ),
            CommandInfo::new(
                "Export Selected Blocks as Markdown",
                CommandCategory::Terminal,
                "Write only selected command blocks and captured output to a private Markdown file",
                crate::keybindings::Command::BlockExportSelectedMarkdown,
            ),
            CommandInfo::new(
                "Export Selected Blocks as JSON",
                CommandCategory::Terminal,
                "Write only selected command blocks and captured output to a private JSON file",
                crate::keybindings::Command::BlockExportSelectedJson,
            ),
            CommandInfo::new(
                "Export Session Blocks as Markdown",
                CommandCategory::Terminal,
                "Write retained finalized blocks to a private Markdown file",
                crate::keybindings::Command::BlockExportSessionMarkdown,
            ),
            CommandInfo::new(
                "Export Session Blocks as JSON",
                CommandCategory::Terminal,
                "Write retained finalized blocks to a private JSON file",
                crate::keybindings::Command::BlockExportSessionJson,
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
                "Swap Panes",
                CommandCategory::Terminal,
                "Exchange the focused pane with the next one",
                crate::keybindings::Command::PaneSwap,
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
                "Toggle AI Chats",
                CommandCategory::Window,
                "Open/close the persistent AI chats library (read-only conversations; nothing runs)",
                crate::keybindings::Command::AiChatToggle,
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
            CommandInfo::new(
                "ASCII Organism: Say hello",
                CommandCategory::Window,
                "Greet the local companion in Full or Calm mode; shared 8-second cooldown",
                crate::keybindings::Command::OrganismHello,
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
            CommandInfo::new(
                "Vim9 IDE: Open Project",
                CommandCategory::Session,
                "Open a real Vim9 editor with native buffers, code actions, project search and build diagnostics",
                crate::keybindings::Command::Vim9Open,
            ),
        ];

        for (label, description, command) in [
            (
                "Quick Open",
                "Find a file in the active Vim project",
                crate::keybindings::Command::Vim9QuickOpen,
            ),
            (
                "Go to Line",
                "Jump to a line or line:column in the active Vim buffer",
                crate::keybindings::Command::Vim9Goto,
            ),
            (
                "Navigate Back",
                "Return to the previous location in the active Vim window's jump list",
                crate::keybindings::Command::Vim9NavigateBack,
            ),
            (
                "Navigate Forward",
                "Move forward in the active Vim window's jump list",
                crate::keybindings::Command::Vim9NavigateForward,
            ),
            (
                "Save",
                "Write the current Vim buffer",
                crate::keybindings::Command::Vim9Save,
            ),
            (
                "Save All",
                "Write all modified Vim buffers",
                crate::keybindings::Command::Vim9SaveAll,
            ),
            (
                "Project Search",
                "Focus the active editor's project search field",
                crate::keybindings::Command::Vim9Search,
            ),
            (
                "Cancel Project Search",
                "Stop the active Vim project's running search",
                crate::keybindings::Command::Vim9CancelSearch,
            ),
            (
                "Build Command",
                "Focus the build command field without running a command",
                crate::keybindings::Command::Vim9Build,
            ),
            (
                "Previous Buffer",
                "Select the previous listed Vim buffer",
                crate::keybindings::Command::Vim9PreviousBuffer,
            ),
            (
                "Next Buffer",
                "Select the next listed Vim buffer",
                crate::keybindings::Command::Vim9NextBuffer,
            ),
            (
                "Close Buffer",
                "Close the current Vim buffer with modified-buffer protection",
                crate::keybindings::Command::Vim9CloseBuffer,
            ),
            (
                "Next Problem",
                "Go to the next quickfix entry",
                crate::keybindings::Command::Vim9NextProblem,
            ),
            (
                "Previous Problem",
                "Go to the previous quickfix entry",
                crate::keybindings::Command::Vim9PreviousProblem,
            ),
        ] {
            commands.push(CommandInfo::new(
                &format!("Vim9 IDE: {label}"),
                CommandCategory::Session,
                description,
                command,
            ));
        }

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
        self.opening = allocate_palette_opening(&NEXT_PALETTE_OPENING);
        self.organism_target = None;
        self.query_widget_id = None;
        self.is_open = true;
        self.needs_focus = true;
        self.search_query.clear();
        self.ask_ai_input_too_large = false;
        self.selected_index = 0;
        self.update_search_results();
    }

    /// Ask-AI palette mode (anvil/forge's `?` prefix). When the raw query
    /// starts with `?`, the remainder is a natural-language command request
    /// for the review-only AI command suggestion — never a command to run.
    /// Returns `Some` only for a non-empty request within the request budget.
    ///
    /// The leading trim matches anvil/forge, which parse `raw.trim_start()`
    /// before looking for the `?`: a space typed (or left behind by a
    /// backspace) in front of the prefix must not silently drop the user back
    /// into ordinary command matching.
    pub fn ask_ai_request(&self) -> Option<String> {
        if self.ask_ai_input_too_large {
            return None;
        }
        let rest = self.search_query.trim_start().strip_prefix('?')?;
        let rest = rest.trim();
        if rest.is_empty()
            || rest.len() > MAX_AI_QUERY_BYTES
            || rest.contains('\u{fffd}')
            || jterm_core::review_input::contains_visual_spoofing(rest)
        {
            None
        } else {
            Some(rest.to_string())
        }
    }

    /// True as soon as the query starts with `?`, even before the request is
    /// non-empty, so the overlay can switch to the Ask-AI affordance.
    pub fn ask_ai_mode(&self) -> bool {
        self.search_query.trim_start().starts_with('?')
    }

    /// An Ask-AI request past the request budget. The affordance fails closed
    /// (no row to accept) and the overlay says why, rather than sending a
    /// request whose middle core would elide — forge's behaviour.
    pub fn ask_ai_request_too_large(&self) -> bool {
        self.ask_ai_input_too_large
            || self
                .search_query
                .trim_start()
                .strip_prefix('?')
                .is_some_and(|rest| rest.trim().len() > MAX_AI_QUERY_BYTES)
    }

    /// 关闭调色板
    pub fn close(&mut self) {
        self.is_open = false;
    }

    /// Ordinary searches use the picker budget. Ask-AI validates the original
    /// request before bounding its display; truncation must never turn an
    /// oversized instruction into an accepted request with its tail missing.
    pub fn set_query(&mut self, query: impl Into<String>) {
        let query = query.into();
        self.ask_ai_input_too_large = false;
        self.search_query = if let Some(request) = query.trim_start().strip_prefix('?') {
            let request = request.trim();
            self.ask_ai_input_too_large = request.len() > MAX_AI_QUERY_BYTES;
            if self.ask_ai_input_too_large {
                let preview: String = request.chars().take(MAX_PALETTE_QUERY_BYTES).collect();
                bound_palette_query(format!("? {preview}"), MAX_PALETTE_QUERY_BYTES)
            } else {
                // Sanitizing a rejected Unicode scalar may expand it to U+FFFD.
                // Retain that marker, rather than truncating it off and accepting
                // a rewritten prefix. The request validator still enforces 64 KiB.
                bound_palette_query(format!("? {request}"), MAX_AI_QUERY_BYTES * 3 + 2)
            }
        } else {
            bound_palette_query(query, MAX_PALETTE_QUERY_BYTES)
        };
        self.selected_index = 0;
        self.update_search_results();
    }

    /// 更新搜索结果
    pub fn update_search_results(&mut self) {
        self.search_results.clear();
        self.selected_index = 0;

        // `?` switches to Ask-AI mode: the fixed command list is replaced by
        // the single AI row, mirroring anvil's palette mode switch.
        if self.ask_ai_mode() {
            return;
        }

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
                    .take(5)
                    .any(|recent| recent == &cmd.command)
            });
            other_commands.sort_by_key(|cmd| cmd.category);
            for cmd in other_commands {
                self.search_results.push((cmd, 50));
            }
        } else {
            if palette_query_is_unsafe(&self.search_query) {
                return;
            }
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

    fn gesture_frame(ctx: &egui::Context, id: egui::Id, events: Vec<egui::Event>) -> bool {
        let mut clicked = false;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, 240.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                let rect =
                    egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(200.0, 30.0));
                clicked = ui.interact(rect, id, egui::Sense::click()).clicked();
            },
        );
        output.textures_delta.clear();
        clicked
    }

    fn release_on_replacement(pressed_id: egui::Id, released_id: egui::Id) -> bool {
        let ctx = egui::Context::default();
        let pos = egui::pos2(30.0, 30.0);
        gesture_frame(&ctx, pressed_id, vec![]);
        gesture_frame(&ctx, pressed_id, vec![egui::Event::PointerMoved(pos)]);
        assert!(!gesture_frame(
            &ctx,
            pressed_id,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }]
        ));
        // The filter replaces the row while the pointer is still held.
        gesture_frame(&ctx, released_id, vec![]);
        gesture_frame(
            &ctx,
            released_id,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        )
    }

    #[test]
    fn palette_pointer_release_cannot_dispatch_a_replacement_command() {
        use crate::keybindings::Command;
        let mut palette = CommandPalette::new();
        palette.open();
        palette.set_query("new tab");
        let pressed = palette.command_row_id(&Command::SessionNew);
        palette.set_query("close tab");
        let replacement = palette.command_row_id(&Command::SessionClose);
        assert!(!release_on_replacement(pressed, replacement));
        // Same-action clicks remain valid even when the query text changes.
        assert!(release_on_replacement(
            pressed,
            palette.command_row_id(&Command::SessionNew)
        ));
    }

    #[test]
    fn palette_pointer_release_cannot_change_ai_request_or_opening() {
        let mut palette = CommandPalette::new();
        palette.open();
        let pressed = palette.ask_ai_row_id("explain listing files");
        assert!(!release_on_replacement(
            pressed,
            palette.ask_ai_row_id("explain removing files")
        ));
        assert!(release_on_replacement(
            pressed,
            palette.ask_ai_row_id("explain listing files")
        ));
        let old_window = palette.window_id();
        palette.close();
        palette.open();
        assert_ne!(old_window, palette.window_id());
        assert!(!release_on_replacement(
            pressed,
            palette.ask_ai_row_id("explain listing files")
        ));
    }

    #[test]
    fn palette_opening_is_retained_while_editing_and_never_wraps() {
        let mut palette = CommandPalette::new();
        palette.open();
        let opening = palette.window_id();
        palette.set_query("session");
        palette.select_next();
        assert_eq!(palette.window_id(), opening);
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_palette_opening(&counter), u64::MAX - 1);
        assert!(std::panic::catch_unwind(|| allocate_palette_opening(&counter)).is_err());
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn palette_rendering_uses_opening_and_exact_action_gesture_keys() {
        let source = include_str!("app/rendering.rs");
        let palette = source
            .split("egui::Window::new(\"Command Palette\")")
            .nth(1)
            .unwrap()
            .split("// 历史命令选择器")
            .next()
            .unwrap();
        assert!(palette.contains(".id(self.command_palette.window_id())"));
        assert!(palette.contains("self.command_palette.command_row_id(&cmd_info.command)"));
        assert!(palette.contains("self.command_palette.ask_ai_row_id(&request)"));
        assert!(!palette.contains(".with(\"palette_click\")"));
        assert!(!palette.contains(".with(\"palette_ask_ai\")"));
    }

    #[test]
    fn organism_palette_target_and_query_identity_belong_to_one_opening() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.organism_target = Some("original".to_owned());
        let id = egui::Id::new("actual query");
        palette.query_widget_id = Some(id);
        palette.set_query("ASCII Organism: Say hello");
        assert_eq!(palette.organism_target.as_deref(), Some("original"));
        assert_eq!(palette.query_widget_id, Some(id));
        assert!(palette
            .all_commands()
            .iter()
            .any(|entry| entry.command == crate::keybindings::Command::OrganismHello));
        palette.close();
        palette.open();
        assert!(palette.organism_target.is_none());
        assert!(palette.query_widget_id.is_none());
    }

    #[test]
    fn palette_hello_surrenders_only_its_exact_query_focus() {
        let ctx = egui::Context::default();
        let query = egui::Id::new("palette query");
        let other = egui::Id::new("other editor");
        ctx.memory_mut(|memory| memory.request_focus(other));
        ctx.memory_mut(|memory| memory.surrender_focus(query));
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(other));
        ctx.memory_mut(|memory| memory.request_focus(query));
        ctx.memory_mut(|memory| memory.surrender_focus(query));
        assert_eq!(ctx.memory(|memory| memory.focused()), None);
    }

    #[test]
    fn selected_block_collapse_is_discoverable_by_output_action() {
        let mut palette = CommandPalette::new();
        palette.search_query = "collapse".to_owned();
        palette.update_search_results();
        assert!(palette.search_results.iter().any(|(entry, _)| {
            entry.command == crate::keybindings::Command::BlockToggleCollapse
                && entry.name == "Toggle Selected Block Collapse"
        }));
    }

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
    fn palette_query_is_bounded_and_rewritten_queries_do_not_match() {
        let mut palette = CommandPalette::new();
        palette.set_query("session\n\u{1b}");
        assert_eq!(palette.search_query, "session");
        assert!(!palette.search_results.is_empty());
        palette.set_query("session\u{202e}");
        assert_eq!(palette.search_query, "session\u{fffd}");
        assert!(palette.search_results.is_empty());
        palette.set_query(format!("{}z", "x".repeat(MAX_PALETTE_QUERY_BYTES)));
        assert_eq!(palette.search_query.len(), MAX_PALETTE_QUERY_BYTES);
        assert!(!palette.search_query.contains('z'));
        palette.set_query("? ok\u{202e}request");
        assert!(palette.ask_ai_mode());
        assert_eq!(palette.ask_ai_request(), None);
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
    fn empty_query_keeps_recent_commands_outside_the_promoted_five() {
        for recent_count in [6, 10] {
            let mut palette = CommandPalette::new();
            let recent: Vec<_> = palette
                .all_commands()
                .iter()
                .take(recent_count)
                .map(|entry| entry.command.clone())
                .collect();
            for command in &recent {
                palette.execute_command(command.clone());
            }
            palette.open();

            assert_eq!(palette.get_results().len(), palette.all_commands().len());
            for command in palette.all_commands() {
                assert_eq!(
                    palette
                        .get_results()
                        .iter()
                        .filter(|(entry, _)| entry.command == command.command)
                        .count(),
                    1,
                    "every command remains discoverable exactly once: {}",
                    command.command
                );
            }
            let promoted: Vec<_> = palette
                .get_results()
                .iter()
                .take(5)
                .map(|(entry, _)| entry.command.clone())
                .collect();
            assert_eq!(
                promoted,
                recent.into_iter().rev().take(5).collect::<Vec<_>>()
            );
        }
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
    fn ask_ai_prefix_switches_mode_and_extracts_the_request() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert!(!palette.ask_ai_mode());
        assert_eq!(palette.ask_ai_request(), None);

        // A bare `?` is already the mode but not yet a request.
        palette.search_query = "?".to_string();
        palette.update_search_results();
        assert!(palette.ask_ai_mode());
        assert_eq!(palette.ask_ai_request(), None);
        assert!(palette.get_results().is_empty());

        palette.search_query = "?  undo the last git commit  ".to_string();
        palette.update_search_results();
        assert_eq!(
            palette.ask_ai_request().as_deref(),
            Some("undo the last git commit")
        );
        // The fixed command list is replaced by the AI affordance row.
        assert!(palette.get_results().is_empty());

        // Ordinary queries and mid-string question marks never enter AI mode.
        palette.search_query = "what does ? do".to_string();
        palette.update_search_results();
        assert!(!palette.ask_ai_mode());
        assert_eq!(palette.ask_ai_request(), None);

        // A leading space before the `?` is still Ask-AI (anvil/forge trim the
        // raw query first). Without the trim the user silently gets the
        // ordinary command list and Enter runs whatever row is highlighted.
        palette.search_query = "  ? find large files".to_string();
        palette.update_search_results();
        assert!(palette.ask_ai_mode());
        assert_eq!(
            palette.ask_ai_request().as_deref(),
            Some("find large files")
        );
        assert!(palette.get_results().is_empty());
    }

    #[test]
    fn ask_ai_fails_closed_on_an_oversized_request() {
        // Past the budget core elides the middle of the request and drafts a
        // command from an instruction with a hole in it. Refuse instead.
        let mut palette = CommandPalette::new();
        palette.open();
        let huge = "x".repeat(MAX_AI_QUERY_BYTES + 1);
        palette.search_query = format!("? {huge}");
        palette.update_search_results();
        assert!(palette.ask_ai_mode());
        assert!(palette.ask_ai_request_too_large());
        assert_eq!(palette.ask_ai_request(), None, "no row may be accepted");

        palette.search_query = format!("? {}", "x".repeat(MAX_AI_QUERY_BYTES));
        palette.update_search_results();
        assert!(!palette.ask_ai_request_too_large());
        assert!(
            palette.ask_ai_request().is_some(),
            "the limit itself passes"
        );
    }

    #[test]
    fn ask_ai_setter_preserves_valid_requests_and_rejects_oversized_originals() {
        let mut palette = CommandPalette::new();
        for request in [
            format!("{} KEEP_THIS_TAIL", "x".repeat(MAX_PALETTE_QUERY_BYTES)),
            "x".repeat(MAX_AI_QUERY_BYTES),
            format!("{}x", "界".repeat(MAX_AI_QUERY_BYTES / 3)),
        ] {
            palette.set_query(format!("  ? {request}  "));
            assert_eq!(palette.ask_ai_request().as_deref(), Some(request.as_str()));
            assert!(!palette.ask_ai_request_too_large());
        }
        for request in [
            "x".repeat(MAX_AI_QUERY_BYTES + 1),
            "界".repeat(MAX_AI_QUERY_BYTES / 3 + 1),
        ] {
            palette.set_query(format!("? {request} KEEP_THIS_TAIL"));
            assert!(palette.ask_ai_mode());
            assert!(palette.ask_ai_request_too_large());
            assert_eq!(palette.ask_ai_request(), None);
            assert!(palette.get_results().is_empty());
            assert!(palette.search_query.len() <= MAX_PALETTE_QUERY_BYTES);
        }
        palette.set_query("? valid again");
        assert_eq!(palette.ask_ai_request().as_deref(), Some("valid again"));
        assert!(!palette.ask_ai_request_too_large());
        palette.set_query(format!("? {}", "x".repeat(MAX_AI_QUERY_BYTES + 1)));
        palette.open();
        assert!(!palette.ask_ai_request_too_large());
        palette.set_query("x".repeat(MAX_PALETTE_QUERY_BYTES + 1));
        assert_eq!(palette.search_query.len(), MAX_PALETTE_QUERY_BYTES);
        assert!(!palette.ask_ai_mode());
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
            Command::HistoryPickerToggle,
            Command::WorkflowPickerToggle,
            Command::OrganismHello,
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
            Command::BlockSelectPrev,
            Command::BlockSelectNext,
            Command::BlockSelectAll,
            Command::BlockReinputSelectedCommands,
            Command::BlockCopyBlock,
            Command::BlockCopyMarkdown,
            Command::BlockJumpPrevFailed,
            Command::BlockJumpNextFailed,
            Command::BlockSearchToggle,
            Command::BlockToggleBookmark,
            Command::BlockToggleCollapse,
            Command::BlockCompareSelected,
            Command::BlockExportSelectedMarkdown,
            Command::BlockExportSelectedJson,
            Command::BlockJumpPrevBookmark,
            Command::BlockJumpNextBookmark,
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
            Command::AiChatToggle,
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
