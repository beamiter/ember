/// 快捷键可配置化系统
///
/// 组合键字符串的解析/规范化/美化交给家族共享的
/// `jterm_core::keybindings`（四个 jterm 语法的并集，一个 canonical
/// 存储形式）。本文件只保留 ember 的命令词表和绑定表本身。
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// A hand-written binding table should remain tiny; this generous ceiling also
/// bounds allocation and TOML parsing work for a planted configuration file.
const MAX_KEYBINDINGS_BYTES: u64 = 256 * 1024;

/// 所有可用的命令
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Command {
    // === 会话管理 ===
    SessionNew,
    SessionClose,
    SessionNext,
    SessionPrev,
    SessionJump(usize), // 跳转到第 N 个会话 (0-8)
    SessionLast,        // 浏览器语义：Ctrl+9 跳到最后一个会话
    SessionPrevActive,  // 在最近两个会话间快速来回(类似 Vim Ctrl+^)

    // === 编辑操作 ===
    EditCopy,
    EditPaste,

    // === 搜索操作 ===
    SearchOpen,
    SearchClose,
    SearchNext,
    SearchPrev,
    SearchHistoryPrev,
    SearchHistoryNext,
    SearchReplaceToggle,

    // === 终端操作 ===
    TerminalSendSigint, // Ctrl+C
    TerminalSendEof,    // Ctrl+D
    TerminalClear,      // Ctrl+L
    TerminalScrollUp,
    TerminalScrollDown,
    TerminalJumpPrevMark,
    TerminalJumpNextMark,

    // === 命令块（block mode）===
    /// 跳到最早一条 exit != 0 的命令块并选中它。
    BlockJumpFirstFailed,
    #[allow(clippy::enum_variant_names)]
    BlockCopyCommand,
    BlockCopyOutput,
    /// 把选中/最近的命令放回提示符（只填入，绝不执行）。
    #[allow(clippy::enum_variant_names)]
    BlockRecallCommand,
    /// 键盘选块：移到更旧/更新的可选块（与 gutter 点击同一集合）。
    BlockSelectPrev,
    BlockSelectNext,
    /// 选择当前 pane 中的所有已完成块（oldest anchor, newest active）。
    BlockSelectAll,
    /// 按终端顺序把多选范围内的命令回填到提示符，绝不执行。
    BlockReinputSelectedCommands,
    /// 复制整个块的纯文本（命令 + 输出；背景块只复制输出）。
    BlockCopyBlock,
    /// 以 Markdown 文档形式复制块（与 frost 相同的固定格式）。
    BlockCopyMarkdown,
    /// 键盘选块（只在失败块之间跳）：更旧/更新的 FAILED 块。
    BlockJumpPrevFailed,
    BlockJumpNextFailed,
    /// 跨块搜索选择器（命令文本 + 输出行的大小写不敏感子串匹配）。
    BlockSearchToggle,
    BlockToggleBookmark,
    BlockJumpPrevBookmark,
    BlockJumpNextBookmark,

    FontIncrease,
    FontDecrease,
    FontReset,
    OpacityIncrease,
    OpacityDecrease,

    // === 分屏操作 ===
    TerminalSplitVertical,   // Ctrl+Shift+E
    TerminalSplitHorizontal, // Ctrl+Shift+D
    TerminalClosePane,       // Ctrl+Shift+W
    PaneFocusNext,
    PaneFocusPrev,
    PaneFocusLeft,
    PaneFocusRight,
    PaneFocusUp,
    PaneFocusDown,
    PaneResizeLeft,
    PaneResizeRight,
    PaneResizeUp,
    PaneResizeDown,
    PaneZoomToggle,
    PaneEqualize,

    // === 窗口操作 ===
    WindowClose,
    #[allow(clippy::enum_variant_names)]
    CommandPaletteToggle,
    HelpToggle,

    // === 配置 ===
    ConfigOpen,
    ConfigClose,
    ConfigToggle,
    DebugToggle,

    // === 侧边栏 ===
    SidebarToggle,
    AgentToggle,

    // === 配套 shell ===
    /// 在独立会话里安装或更新 jsh。
    JshInstall,

    // === 远程 ===
    /// 打开远程主机选择器（`[[remote_hosts]]` 配置的 ssh/容器目标）。
    RemotePicker,
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::SessionNew => write!(f, "session:new"),
            Command::SessionClose => write!(f, "session:close"),
            Command::SessionNext => write!(f, "session:next"),
            Command::SessionPrev => write!(f, "session:prev"),
            Command::SessionJump(n) => write!(f, "session:jump:{}", n),
            Command::SessionLast => write!(f, "session:last"),
            Command::SessionPrevActive => write!(f, "session:prev_active"),
            Command::EditCopy => write!(f, "edit:copy"),
            Command::EditPaste => write!(f, "edit:paste"),
            Command::SearchOpen => write!(f, "search:open"),
            Command::SearchClose => write!(f, "search:close"),
            Command::SearchNext => write!(f, "search:next"),
            Command::SearchPrev => write!(f, "search:prev"),
            Command::SearchHistoryPrev => write!(f, "search:history:prev"),
            Command::SearchHistoryNext => write!(f, "search:history:next"),
            Command::SearchReplaceToggle => write!(f, "search:replace:toggle"),
            Command::TerminalSendSigint => write!(f, "terminal:send_sigint"),
            Command::TerminalSendEof => write!(f, "terminal:send_eof"),
            Command::TerminalClear => write!(f, "terminal:clear"),
            Command::TerminalScrollUp => write!(f, "terminal:scroll_up"),
            Command::TerminalScrollDown => write!(f, "terminal:scroll_down"),
            Command::TerminalJumpPrevMark => write!(f, "terminal:jump_prev_command"),
            Command::TerminalJumpNextMark => write!(f, "terminal:jump_next_command"),
            Command::BlockJumpFirstFailed => write!(f, "block:jump_first_failed"),
            Command::BlockCopyCommand => write!(f, "block:copy_command"),
            Command::BlockCopyOutput => write!(f, "block:copy_output"),
            Command::BlockRecallCommand => write!(f, "block:recall_command"),
            Command::BlockSelectPrev => write!(f, "block:select_prev"),
            Command::BlockSelectNext => write!(f, "block:select_next"),
            Command::BlockSelectAll => write!(f, "block:select_all"),
            Command::BlockReinputSelectedCommands => {
                write!(f, "block:reinput_selected_commands")
            }
            Command::BlockCopyBlock => write!(f, "block:copy_block"),
            Command::BlockCopyMarkdown => write!(f, "block:copy_markdown"),
            Command::BlockJumpPrevFailed => write!(f, "block:jump_prev_failed"),
            Command::BlockJumpNextFailed => write!(f, "block:jump_next_failed"),
            Command::BlockSearchToggle => write!(f, "block:search"),
            Command::BlockToggleBookmark => write!(f, "block:toggle_bookmark"),
            Command::BlockJumpPrevBookmark => write!(f, "block:jump_prev_bookmark"),
            Command::BlockJumpNextBookmark => write!(f, "block:jump_next_bookmark"),
            Command::FontIncrease => write!(f, "font:increase"),
            Command::FontDecrease => write!(f, "font:decrease"),
            Command::FontReset => write!(f, "font:reset"),
            Command::OpacityIncrease => write!(f, "opacity:increase"),
            Command::OpacityDecrease => write!(f, "opacity:decrease"),
            Command::TerminalSplitVertical => write!(f, "terminal:split_vertical"),
            Command::TerminalSplitHorizontal => write!(f, "terminal:split_horizontal"),
            Command::TerminalClosePane => write!(f, "terminal:close_pane"),
            Command::PaneFocusNext => write!(f, "pane:focus_next"),
            Command::PaneFocusPrev => write!(f, "pane:focus_prev"),
            Command::PaneFocusLeft => write!(f, "pane:focus_left"),
            Command::PaneFocusRight => write!(f, "pane:focus_right"),
            Command::PaneFocusUp => write!(f, "pane:focus_up"),
            Command::PaneFocusDown => write!(f, "pane:focus_down"),
            Command::PaneResizeLeft => write!(f, "pane:resize_left"),
            Command::PaneResizeRight => write!(f, "pane:resize_right"),
            Command::PaneResizeUp => write!(f, "pane:resize_up"),
            Command::PaneResizeDown => write!(f, "pane:resize_down"),
            Command::PaneZoomToggle => write!(f, "pane:zoom_toggle"),
            Command::PaneEqualize => write!(f, "pane:equalize"),
            Command::WindowClose => write!(f, "window:close"),
            Command::CommandPaletteToggle => write!(f, "command_palette:toggle"),
            Command::HelpToggle => write!(f, "help:toggle"),
            Command::ConfigOpen => write!(f, "config:open"),
            Command::ConfigClose => write!(f, "config:close"),
            Command::ConfigToggle => write!(f, "config:toggle"),
            Command::DebugToggle => write!(f, "debug:toggle"),
            Command::SidebarToggle => write!(f, "sidebar:toggle"),
            Command::AgentToggle => write!(f, "agent:toggle"),
            Command::JshInstall => write!(f, "jsh:install"),
            Command::RemotePicker => write!(f, "remote:picker"),
        }
    }
}

impl std::str::FromStr for Command {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "session:new" => Ok(Command::SessionNew),
            "session:close" => Ok(Command::SessionClose),
            "session:next" => Ok(Command::SessionNext),
            "session:prev" => Ok(Command::SessionPrev),
            "session:last" => Ok(Command::SessionLast),
            "session:prev_active" => Ok(Command::SessionPrevActive),
            "edit:copy" => Ok(Command::EditCopy),
            "edit:paste" => Ok(Command::EditPaste),
            "search:open" => Ok(Command::SearchOpen),
            "search:close" => Ok(Command::SearchClose),
            "search:next" => Ok(Command::SearchNext),
            "search:prev" => Ok(Command::SearchPrev),
            "search:history:prev" => Ok(Command::SearchHistoryPrev),
            "search:history:next" => Ok(Command::SearchHistoryNext),
            "search:replace:toggle" => Ok(Command::SearchReplaceToggle),
            "terminal:send_sigint" => Ok(Command::TerminalSendSigint),
            "terminal:send_eof" => Ok(Command::TerminalSendEof),
            "terminal:clear" => Ok(Command::TerminalClear),
            "terminal:scroll_up" => Ok(Command::TerminalScrollUp),
            "terminal:scroll_down" => Ok(Command::TerminalScrollDown),
            "terminal:jump_prev_command" => Ok(Command::TerminalJumpPrevMark),
            "terminal:jump_next_command" => Ok(Command::TerminalJumpNextMark),
            "block:jump_first_failed" => Ok(Command::BlockJumpFirstFailed),
            "block:copy_command" => Ok(Command::BlockCopyCommand),
            "block:copy_output" => Ok(Command::BlockCopyOutput),
            "block:recall_command" => Ok(Command::BlockRecallCommand),
            "block:select_prev" => Ok(Command::BlockSelectPrev),
            "block:select_next" => Ok(Command::BlockSelectNext),
            "block:select_all" => Ok(Command::BlockSelectAll),
            "block:reinput_selected_commands" => Ok(Command::BlockReinputSelectedCommands),
            "block:copy_block" => Ok(Command::BlockCopyBlock),
            "block:copy_markdown" => Ok(Command::BlockCopyMarkdown),
            "block:jump_prev_failed" => Ok(Command::BlockJumpPrevFailed),
            "block:jump_next_failed" => Ok(Command::BlockJumpNextFailed),
            "block:search" => Ok(Command::BlockSearchToggle),
            "block:toggle_bookmark" => Ok(Command::BlockToggleBookmark),
            "block:jump_prev_bookmark" => Ok(Command::BlockJumpPrevBookmark),
            "block:jump_next_bookmark" => Ok(Command::BlockJumpNextBookmark),
            "font:increase" => Ok(Command::FontIncrease),
            "font:decrease" => Ok(Command::FontDecrease),
            "font:reset" => Ok(Command::FontReset),
            "opacity:increase" => Ok(Command::OpacityIncrease),
            "opacity:decrease" => Ok(Command::OpacityDecrease),
            "terminal:split_vertical" => Ok(Command::TerminalSplitVertical),
            "terminal:split_horizontal" => Ok(Command::TerminalSplitHorizontal),
            "terminal:close_pane" => Ok(Command::TerminalClosePane),
            "pane:focus_next" => Ok(Command::PaneFocusNext),
            "pane:focus_prev" => Ok(Command::PaneFocusPrev),
            "pane:focus_left" => Ok(Command::PaneFocusLeft),
            "pane:focus_right" => Ok(Command::PaneFocusRight),
            "pane:focus_up" => Ok(Command::PaneFocusUp),
            "pane:focus_down" => Ok(Command::PaneFocusDown),
            "pane:resize_left" => Ok(Command::PaneResizeLeft),
            "pane:resize_right" => Ok(Command::PaneResizeRight),
            "pane:resize_up" => Ok(Command::PaneResizeUp),
            "pane:resize_down" => Ok(Command::PaneResizeDown),
            "pane:zoom_toggle" => Ok(Command::PaneZoomToggle),
            "pane:equalize" => Ok(Command::PaneEqualize),
            "window:close" => Ok(Command::WindowClose),
            "command_palette:toggle" => Ok(Command::CommandPaletteToggle),
            "help:toggle" => Ok(Command::HelpToggle),
            "config:open" => Ok(Command::ConfigOpen),
            "config:close" => Ok(Command::ConfigClose),
            "config:toggle" => Ok(Command::ConfigToggle),
            "debug:toggle" => Ok(Command::DebugToggle),
            "sidebar:toggle" => Ok(Command::SidebarToggle),
            "agent:toggle" => Ok(Command::AgentToggle),
            "jsh:install" => Ok(Command::JshInstall),
            "remote:picker" => Ok(Command::RemotePicker),
            s if s.starts_with("session:jump:") => {
                let num_str = &s[13..];
                let num = num_str
                    .parse::<usize>()
                    .map_err(|_| format!("Invalid session number: {}", num_str))?;
                if num < 9 {
                    Ok(Command::SessionJump(num))
                } else {
                    Err(format!("Session number out of range: {}", num))
                }
            }
            _ => Err(format!("Unknown command: {}", s)),
        }
    }
}

/// 快捷键绑定集合
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyBindings {
    #[serde(flatten)]
    pub bindings: HashMap<String, String>, // "ctrl+shift+a" => "command:name"
}

impl KeyBindings {
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }

    /// 加载默认快捷键
    pub fn default_bindings() -> Self {
        let mut bindings = Self::new();

        // 会话管理
        bindings
            .bindings
            .insert("ctrl+shift+t".to_string(), "session:new".to_string());
        bindings
            .bindings
            .insert("ctrl+d".to_string(), "terminal:send_eof".to_string());
        bindings
            .bindings
            .insert("ctrl+tab".to_string(), "session:next".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+tab".to_string(), "session:prev".to_string());
        bindings
            .bindings
            .insert("ctrl+pagedown".to_string(), "session:next".to_string());
        bindings
            .bindings
            .insert("ctrl+pageup".to_string(), "session:prev".to_string());
        bindings
            .bindings
            .insert("ctrl+`".to_string(), "session:prev_active".to_string());

        // 浏览器式会话切换：Ctrl+1..8 对应前 8 个，Ctrl+9 总是最后一个。
        // Ctrl+0 保留给字号复位，避免同一按键双触发。
        for i in 0..8 {
            bindings
                .bindings
                .insert(format!("ctrl+{}", i + 1), format!("session:jump:{}", i));
        }
        bindings
            .bindings
            .insert("ctrl+9".to_string(), "session:last".to_string());

        // 编辑操作
        bindings
            .bindings
            .insert("ctrl+shift+c".to_string(), "edit:copy".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+v".to_string(), "edit:paste".to_string());
        bindings
            .bindings
            .insert("shift+insert".to_string(), "edit:paste".to_string());

        // 搜索操作
        bindings
            .bindings
            .insert("ctrl+shift+f".to_string(), "search:open".to_string());
        bindings.bindings.insert(
            "ctrl+alt+r".to_string(),
            "search:replace:toggle".to_string(),
        );

        // 配置操作
        bindings
            .bindings
            .insert("ctrl+shift+o".to_string(), "config:toggle".to_string());
        bindings
            .bindings
            .insert("f12".to_string(), "debug:toggle".to_string());

        // 全局界面
        bindings.bindings.insert(
            "ctrl+shift+p".to_string(),
            "command_palette:toggle".to_string(),
        );
        bindings
            .bindings
            .insert("ctrl+shift+/".to_string(), "help:toggle".to_string());

        // 侧边栏。存储用共享语法的 canonical 拼写 `backslash`：用户配置里
        // "ctrl+\\" 与 "ctrl+backslash" 都解析到同一 chord，TOML 无需转义。
        bindings
            .bindings
            .insert("ctrl+backslash".to_string(), "sidebar:toggle".to_string());

        // AI agent 面板
        bindings
            .bindings
            .insert("ctrl+shift+alt+a".to_string(), "agent:toggle".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+s".to_string(), "remote:picker".to_string());

        // 终端操作
        bindings
            .bindings
            .insert("ctrl+up".to_string(), "terminal:scroll_up".to_string());
        bindings
            .bindings
            .insert("ctrl+down".to_string(), "terminal:scroll_down".to_string());
        // 统一的分屏与物理方向窗格操作。
        bindings.bindings.insert(
            "ctrl+shift+d".to_string(),
            "terminal:split_horizontal".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+e".to_string(),
            "terminal:split_vertical".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+w".to_string(),
            "terminal:close_pane".to_string(),
        );
        for (key, command) in [
            ("left", "pane:focus_left"),
            ("right", "pane:focus_right"),
            ("up", "pane:focus_up"),
            ("down", "pane:focus_down"),
        ] {
            bindings
                .bindings
                .insert(format!("ctrl+alt+{key}"), command.to_string());
        }
        for (key, command) in [
            ("left", "pane:resize_left"),
            ("right", "pane:resize_right"),
            ("up", "pane:resize_up"),
            ("down", "pane:resize_down"),
        ] {
            // `build_keybinding_string` 的内部次序为 Ctrl, Shift, Alt。
            bindings
                .bindings
                .insert(format!("ctrl+shift+alt+{key}"), command.to_string());
        }
        bindings.bindings.insert(
            "ctrl+shift+return".to_string(),
            "pane:zoom_toggle".to_string(),
        );
        // 家族契约的 zoom 键位（jterm_core DEFAULT_CHORDS）。保留上面的
        // ctrl+shift+return 以兼容既有肌肉记忆。
        bindings
            .bindings
            .insert("ctrl+shift+z".to_string(), "pane:zoom_toggle".to_string());

        // OSC 133 命令跳转：上一/下一个 shell 提示符
        bindings.bindings.insert(
            "ctrl+shift+up".to_string(),
            "terminal:jump_prev_command".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+down".to_string(),
            "terminal:jump_next_command".to_string(),
        );
        // 跳到最早的失败命令块,与 anvil/forge 的 filter_failed_blocks
        // 同键位。其余 block:* 命令默认不绑定,走命令面板——包括
        // block:select_prev/next 与 block:jump_prev/next_failed:它们想要
        // 的 ctrl+alt+up/down 和 ctrl+alt+left/right 已被 pane:focus_*
        // 占用。
        bindings.bindings.insert(
            "ctrl+shift+x".to_string(),
            "block:jump_first_failed".to_string(),
        );
        // 跨块搜索,与 anvil 的 cross-block search 同键位(ember 的默认
        // 表里 ctrl+shift+g 空闲)。
        bindings
            .bindings
            .insert("ctrl+shift+g".to_string(), "block:search".to_string());
        // Warp/anvil/forge 的批量 block 工作流。Agent 移到
        // Ctrl+Shift+Alt+A，为 Select all blocks 让出家族统一键位。
        bindings
            .bindings
            .insert("ctrl+shift+a".to_string(), "block:select_all".to_string());
        bindings.bindings.insert(
            "ctrl+shift+i".to_string(),
            "block:reinput_selected_commands".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+b".to_string(),
            "block:toggle_bookmark".to_string(),
        );
        bindings
            .bindings
            .insert("ctrl+,".to_string(), "block:jump_prev_bookmark".to_string());
        bindings
            .bindings
            .insert("ctrl+.".to_string(), "block:jump_next_bookmark".to_string());
        // Keep font zoom in the same configurable command path as every other
        // keyboard shortcut. Different keyboard layouts can report `+` either
        // with or without Shift, while `Ctrl+=` is the conventional spelling.
        for chord in ["ctrl+=", "ctrl++", "ctrl+shift++"] {
            bindings
                .bindings
                .insert(chord.to_string(), "font:increase".to_string());
        }
        bindings
            .bindings
            .insert("ctrl+-".to_string(), "font:decrease".to_string());
        bindings
            .bindings
            .insert("ctrl+0".to_string(), "font:reset".to_string());

        // 窗口透明度实时调节，与 anvil/forge 相同的键位。
        bindings
            .bindings
            .insert("ctrl+alt+=".to_string(), "opacity:increase".to_string());
        bindings
            .bindings
            .insert("ctrl+alt+-".to_string(), "opacity:decrease".to_string());

        bindings
    }

    /// 获取快捷键对应的命令
    pub fn get_command(&self, key_str: &str) -> Option<Command> {
        let normalized = Self::normalize_binding(key_str).ok()?;
        self.bindings
            .get(&normalized)
            .and_then(|cmd_str| cmd_str.parse::<Command>().ok())
    }

    /// 反向查找：给定命令名，返回所有绑定的人类可读组合键（如 "Ctrl+Shift+T"）
    pub fn pretty_bindings_for(&self, command: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .bindings
            .iter()
            .filter(|(_, cmd)| cmd.as_str() == command)
            .map(|(key, _)| Self::prettify_binding(key))
            .collect();
        out.sort();
        out
    }

    /// 将 "ctrl+shift+t" 美化为 "Ctrl+Shift+T"（共享的 display 形式：
    /// `return` 显示为 Enter、`backslash` 显示为 `\`）。无法解析的字符串
    /// 原样返回，保证帮助面板不因单个坏键位而崩。
    fn prettify_binding(key: &str) -> String {
        match jterm_core::keybindings::parse(key) {
            Ok(chord) => chord.display(),
            Err(_) => key.to_string(),
        }
    }

    /// 加载配置文件，与默认配置合并
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let path = Self::config_path()?;
        Self::load_path(&path)
    }

    /// Load one explicit path through ember's descriptor-based persistence
    /// boundary. Keeping this separate from environment-based path discovery
    /// makes the security contract directly testable without mutating HOME.
    fn load_path(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut bindings = Self::default_bindings();
        let revision = crate::persistence_file::read_revision(path, MAX_KEYBINDINGS_BYTES)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("cannot read keybindings file {}: {error}", path.display()),
                )
            })?;
        let Some(bytes) = revision.bytes() else {
            return Ok(bindings);
        };
        let content = std::str::from_utf8(bytes).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("keybindings file {} is not valid UTF-8", path.display()),
            )
        })?;
        let user_bindings: KeyBindings = toml::from_str(content).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cannot parse keybindings file {}: {error}", path.display()),
            )
        })?;
        for warning in bindings.merge_user_bindings(user_bindings) {
            eprintln!("[Keybindings] WARNING: {warning}");
        }

        Ok(bindings)
    }

    /// Merge hand-edited overrides without letting one bad entry discard every
    /// valid customization. Keys are normalized to the exact modifier order
    /// emitted by the input router; `none` removes a default binding.
    fn merge_user_bindings(&mut self, user: KeyBindings) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut entries: Vec<_> = user.bindings.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        for (raw_key, raw_command) in entries {
            let key = match Self::normalize_binding(&raw_key) {
                Ok(key) => key,
                Err(error) => {
                    warnings.push(format!("ignoring binding '{raw_key}': {error}"));
                    continue;
                }
            };
            let command = raw_command.trim().to_ascii_lowercase();
            if jterm_core::keybindings::is_unbind_token(&command) {
                self.bindings.remove(&key);
                continue;
            }
            if let Err(error) = command.parse::<Command>() {
                warnings.push(format!(
                    "ignoring binding '{raw_key}' = '{raw_command}': {error}"
                ));
                continue;
            }
            self.bindings.insert(key, command);
        }
        warnings
    }

    /// 组合键规范化：交给 `jterm_core::keybindings::parse`，返回共享的
    /// canonical 存储形式（小写、ctrl+shift+alt+super 次序、`\` 折叠为
    /// `backslash`）。相比旧的本地 allowlist，语法是四个 jterm 的并集：
    /// `space` 可绑定、f13–f24 与 X11 风格符号名（`minus`、`grave`…）
    /// 也能解析——这是有意的家族标准化。
    fn normalize_binding(binding: &str) -> Result<String, String> {
        jterm_core::keybindings::parse(binding)
            .map(|chord| chord.canonical())
            .map_err(|error| error.to_string())
    }

    /// 获取配置文件路径
    pub fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Could not determine config directory")?;
        Ok(config_dir.join("ember/keybindings.toml"))
    }
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self::default_bindings()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ember-keybindings-{label}-{}-{id}",
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

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_private(path: &Path, contents: impl AsRef<[u8]>) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn path_loader_defaults_when_missing_and_keeps_valid_forward_compatible_entries() {
        let root = TestDir::new("missing-valid");
        let path = root.join("keybindings.toml");

        let missing = KeyBindings::load_path(&path).unwrap();
        assert_eq!(
            missing.get_command("ctrl+shift+t"),
            Some(Command::SessionNew)
        );

        write_private(
            &path,
            concat!(
                "\"ctrl+shift+t\" = \"session:close\"\n",
                "\"ctrl+shift+y\" = \"future:command\"\n"
            ),
        );
        let loaded = KeyBindings::load_path(&path).unwrap();
        assert_eq!(
            loaded.get_command("ctrl+shift+t"),
            Some(Command::SessionClose)
        );
        assert_eq!(loaded.get_command("ctrl+shift+y"), None);
    }

    #[test]
    fn keybindings_size_limit_is_inclusive_and_checked_before_toml_parse() {
        let root = TestDir::new("size-limit");
        let path = root.join("keybindings.toml");
        let mut exact = b"\"ctrl+shift+t\" = \"session:close\"\n".to_vec();
        exact.resize(MAX_KEYBINDINGS_BYTES as usize, b' ');
        write_private(&path, &exact);

        assert_eq!(
            KeyBindings::load_path(&path)
                .unwrap()
                .get_command("ctrl+shift+t"),
            Some(Command::SessionClose)
        );

        // The first byte beyond the cap is invalid UTF-8: size rejection must
        // win before either text decoding or TOML parsing sees it.
        exact.push(0xff);
        write_private(&path, exact);
        let error = KeyBindings::load_path(&path).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::FileTooLarge)
        );
        let message = error.to_string();
        assert!(
            message.contains(path.to_string_lossy().as_ref()),
            "{message}"
        );
        assert!(message.contains("262144-byte limit"), "{message}");

        write_private(&path, [0xff]);
        let error = KeyBindings::load_path(&path).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::InvalidData)
        );
        assert!(
            error.to_string().contains(path.to_string_lossy().as_ref()),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_keybinding_entries_are_rejected_and_fifo_open_is_bounded() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        use std::time::Duration;

        let root = TestDir::new("unsafe-entries");
        let target = root.join("target.toml");
        write_private(&target, b"\"ctrl+shift+t\" = \"session:close\"\n");

        let linked = root.join("symlink.toml");
        symlink(&target, &linked).unwrap();
        let error = KeyBindings::load_path(&linked).unwrap_err().to_string();
        assert!(error.contains(linked.to_string_lossy().as_ref()), "{error}");

        let hard_linked = root.join("hard-link.toml");
        std::fs::hard_link(&target, &hard_linked).unwrap();
        let error = KeyBindings::load_path(&hard_linked).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::PermissionDenied)
        );
        assert!(error.to_string().contains("hard link"), "{error}");

        let writable = root.join("writable.toml");
        std::fs::write(&writable, b"\"ctrl+shift+t\" = \"session:close\"\n").unwrap();
        std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o666)).unwrap();
        let error = KeyBindings::load_path(&writable).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::PermissionDenied)
        );

        let fifo = root.join("fifo.toml");
        let encoded = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: encoded is a live NUL-terminated path for this call.
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        let fifo_for_reader = fifo.clone();
        let reader = std::thread::spawn(move || {
            let outcome = KeyBindings::load_path(&fifo_for_reader)
                .map(|_| ())
                .map_err(|error| error.to_string());
            sender.send(outcome).unwrap();
        });
        let outcome = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("opening a keybindings FIFO must never wait for a writer");
        assert!(outcome.is_err());
        reader.join().unwrap();

        let device_error = KeyBindings::load_path(Path::new("/dev/null")).unwrap_err();
        assert_eq!(
            device_error
                .downcast_ref::<io::Error>()
                .map(io::Error::kind),
            Some(io::ErrorKind::InvalidInput)
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"\"ctrl+shift+t\" = \"session:close\"\n"
        );
    }

    #[test]
    fn test_command_parse() {
        let cmd: Command = "session:new".parse().unwrap();
        assert_eq!(cmd, Command::SessionNew);

        let cmd: Command = "session:jump:5".parse().unwrap();
        assert_eq!(cmd, Command::SessionJump(5));

        for command in [
            Command::SessionLast,
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
            Command::FontIncrease,
            Command::FontDecrease,
            Command::FontReset,
            Command::OpacityIncrease,
            Command::OpacityDecrease,
            Command::CommandPaletteToggle,
            Command::HelpToggle,
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
            Command::BlockJumpPrevBookmark,
            Command::BlockJumpNextBookmark,
        ] {
            assert_eq!(command.to_string().parse::<Command>().unwrap(), command);
        }
    }

    #[test]
    fn common_default_bindings_follow_the_shared_contract() {
        let bindings = KeyBindings::default_bindings();

        let expected = [
            ("ctrl+shift+t", Command::SessionNew),
            ("ctrl+shift+w", Command::TerminalClosePane),
            ("ctrl+shift+c", Command::EditCopy),
            ("ctrl+shift+v", Command::EditPaste),
            ("ctrl+shift+f", Command::SearchOpen),
            ("ctrl+shift+p", Command::CommandPaletteToggle),
            ("ctrl+tab", Command::SessionNext),
            ("ctrl+shift+tab", Command::SessionPrev),
            ("ctrl+pagedown", Command::SessionNext),
            ("ctrl+pageup", Command::SessionPrev),
            ("ctrl+shift+o", Command::ConfigToggle),
            ("ctrl+backslash", Command::SidebarToggle),
            ("ctrl+shift+alt+a", Command::AgentToggle),
            ("ctrl+shift+a", Command::BlockSelectAll),
            ("ctrl+shift+i", Command::BlockReinputSelectedCommands),
            ("ctrl+shift+b", Command::BlockToggleBookmark),
            ("ctrl+,", Command::BlockJumpPrevBookmark),
            ("ctrl+.", Command::BlockJumpNextBookmark),
            ("ctrl+shift+e", Command::TerminalSplitVertical),
            ("ctrl+shift+d", Command::TerminalSplitHorizontal),
            ("ctrl+shift+return", Command::PaneZoomToggle),
            ("ctrl+shift+z", Command::PaneZoomToggle),
            ("ctrl+alt+r", Command::SearchReplaceToggle),
            ("ctrl+shift+/", Command::HelpToggle),
            ("shift+insert", Command::EditPaste),
            ("ctrl+=", Command::FontIncrease),
            ("ctrl+-", Command::FontDecrease),
            ("ctrl+0", Command::FontReset),
            ("ctrl+alt+=", Command::OpacityIncrease),
            ("ctrl+alt+-", Command::OpacityDecrease),
            ("f12", Command::DebugToggle),
            ("ctrl+shift+x", Command::BlockJumpFirstFailed),
            ("ctrl+shift+g", Command::BlockSearchToggle),
        ];
        for (key, command) in expected {
            assert_eq!(
                bindings.get_command(key),
                Some(command.clone()),
                "unexpected command for {key}"
            );
            assert_eq!(
                bindings
                    .bindings
                    .iter()
                    .filter(|(bound_key, _)| bound_key.as_str() == key)
                    .count(),
                1,
                "duplicate default chord {key}"
            );
        }

        for removed in [
            "ctrl+shift+,",
            "ctrl+shift+r",
            "ctrl+shift+q",
            "alt+left",
            "alt+right",
        ] {
            assert_eq!(
                bindings.get_command(removed),
                None,
                "stale binding {removed}"
            );
        }
        assert_eq!(bindings.get_command("ctrl+0"), Some(Command::FontReset));

        // 侧边栏存储为 canonical 的 "ctrl+backslash"，但字面反斜杠拼写
        // （既有用户配置的写法）必须命中同一绑定。
        assert_eq!(
            bindings.get_command("ctrl+\\"),
            Some(Command::SidebarToggle)
        );
    }

    /// ember 对家族默认键位契约每一行的本地命令。ember 目前实现了
    /// `DEFAULT_CHORDS` 的全部行（含 sidebar），因此没有跳过项；若
    /// jterm_core 新增了 ember 尚未实现的行，穷举 match 会编译失败，
    /// 迫使在这里显式选择：给出映射，或返回 `None` 并留注释说明跳过。
    fn local_command_for(action: jterm_core::keybindings::CommonAction) -> Option<Command> {
        use jterm_core::keybindings::CommonAction as A;
        Some(match action {
            A::NewTab => Command::SessionNew,
            A::ClosePaneOrTab => Command::TerminalClosePane,
            A::Copy => Command::EditCopy,
            A::Paste => Command::EditPaste,
            A::NextTab => Command::SessionNext,
            A::PrevTab => Command::SessionPrev,
            A::NextTabPage => Command::SessionNext,
            A::PrevTabPage => Command::SessionPrev,
            A::QuickSwitch(n) => Command::SessionJump(usize::from(n) - 1),
            A::LastTab => Command::SessionLast,
            A::FontIncrease => Command::FontIncrease,
            A::FontDecrease => Command::FontDecrease,
            A::FontReset => Command::FontReset,
            A::Search => Command::SearchOpen,
            A::CommandPalette => Command::CommandPaletteToggle,
            A::Settings => Command::ConfigToggle,
            A::Sidebar => Command::SidebarToggle,
            A::DebugPanel => Command::DebugToggle,
            A::ScrollUp => Command::TerminalScrollUp,
            A::ScrollDown => Command::TerminalScrollDown,
            A::PaneFocusLeft => Command::PaneFocusLeft,
            A::PaneFocusRight => Command::PaneFocusRight,
            A::PaneFocusUp => Command::PaneFocusUp,
            A::PaneFocusDown => Command::PaneFocusDown,
            A::PaneResizeLeft => Command::PaneResizeLeft,
            A::PaneResizeRight => Command::PaneResizeRight,
            A::PaneResizeUp => Command::PaneResizeUp,
            A::PaneResizeDown => Command::PaneResizeDown,
            A::SplitSideBySide => Command::TerminalSplitVertical,
            A::SplitStacked => Command::TerminalSplitHorizontal,
            A::PaneZoom => Command::PaneZoomToggle,
        })
    }

    #[test]
    fn default_bindings_implement_every_row_of_the_family_chord_contract() {
        let bindings = KeyBindings::default_bindings();
        for (action, chord) in jterm_core::keybindings::DEFAULT_CHORDS {
            let Some(command) = local_command_for(*action) else {
                continue; // 显式跳过：ember 未实现的契约行（目前没有）。
            };
            // 契约行必须以 canonical 拼写直接存在于绑定表中（map 键即
            // canonical 形式），并且经 get_command 的规范化路径可解析。
            assert_eq!(
                bindings.bindings.get(*chord),
                Some(&command.to_string()),
                "family contract row {action:?} must be stored under canonical chord {chord:?}"
            );
            assert_eq!(
                bindings.get_command(chord),
                Some(command),
                "family contract row {action:?} ({chord}) must resolve via get_command"
            );
        }
    }

    #[test]
    fn default_binding_keys_are_core_canonical_and_commands_parse() {
        let bindings = KeyBindings::default_bindings();
        for (key, command) in &bindings.bindings {
            assert_eq!(
                KeyBindings::normalize_binding(key).as_deref(),
                Ok(key.as_str()),
                "default chord {key:?} must be a canonical fixed point, or runtime lookups miss it"
            );
            assert!(
                command.parse::<Command>().is_ok(),
                "default command {command:?} must parse"
            );
        }
    }

    #[test]
    fn numeric_tabs_use_browser_semantics_and_ctrl_zero_resets_zoom() {
        let bindings = KeyBindings::default_bindings();
        for index in 0..8 {
            assert_eq!(
                bindings.get_command(&format!("ctrl+{}", index + 1)),
                Some(Command::SessionJump(index))
            );
        }
        assert_eq!(bindings.get_command("ctrl+9"), Some(Command::SessionLast));
        assert_eq!(bindings.get_command("ctrl+0"), Some(Command::FontReset));
    }

    #[test]
    fn directional_pane_bindings_cover_focus_and_resize() {
        let bindings = KeyBindings::default_bindings();
        for (key, focus, resize) in [
            ("left", Command::PaneFocusLeft, Command::PaneResizeLeft),
            ("right", Command::PaneFocusRight, Command::PaneResizeRight),
            ("up", Command::PaneFocusUp, Command::PaneResizeUp),
            ("down", Command::PaneFocusDown, Command::PaneResizeDown),
        ] {
            assert_eq!(
                bindings.get_command(&format!("ctrl+alt+{key}")),
                Some(focus)
            );
            assert_eq!(
                bindings.get_command(&format!("ctrl+shift+alt+{key}")),
                Some(resize)
            );
        }
    }

    #[test]
    fn user_bindings_are_canonicalized_validated_and_can_unbind_defaults() {
        let mut bindings = KeyBindings::default_bindings();
        let user = KeyBindings {
            bindings: HashMap::from([
                (
                    " Alt + Control + X ".to_string(),
                    " SESSION:NEW ".to_string(),
                ),
                ("ctrl+shift+t".to_string(), "none".to_string()),
                // 家族共享的解绑词表（is_unbind_token）：false/disabled/
                // unbind 与 none 等价。
                ("ctrl+shift+w".to_string(), "false".to_string()),
                ("ctrl+shift+y".to_string(), "not:a:command".to_string()),
            ]),
        };

        let warnings = bindings.merge_user_bindings(user);

        assert_eq!(
            bindings.get_command("ctrl+alt+x"),
            Some(Command::SessionNew)
        );
        assert_eq!(bindings.get_command("ctrl+shift+t"), None);
        assert_eq!(bindings.get_command("ctrl+shift+w"), None);
        assert_eq!(bindings.get_command("ctrl+shift+y"), None);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn binding_normalization_handles_aliases_and_literal_plus() {
        assert_eq!(
            KeyBindings::normalize_binding("Shift+Ctrl+Enter").as_deref(),
            Ok("ctrl+shift+return")
        );
        assert_eq!(
            KeyBindings::normalize_binding("ctrl++").as_deref(),
            Ok("ctrl++")
        );
        // 两种反斜杠拼写折叠到同一 canonical 存储形式。
        assert_eq!(
            KeyBindings::normalize_binding("Ctrl+\\").as_deref(),
            Ok("ctrl+backslash")
        );
        assert_eq!(
            KeyBindings::normalize_binding("ctrl+backslash").as_deref(),
            Ok("ctrl+backslash")
        );
        // 共享语法有意放宽的点：space 可绑定（旧 allowlist 拒绝它）。
        assert_eq!(
            KeyBindings::normalize_binding("ctrl+space").as_deref(),
            Ok("ctrl+space")
        );
        assert!(KeyBindings::normalize_binding("ctrl+ctrl+x").is_err());
        assert!(KeyBindings::normalize_binding("ctrl+bogus").is_err());
    }

    #[test]
    fn prettify_uses_the_family_display_form() {
        assert_eq!(
            KeyBindings::prettify_binding("ctrl+shift+t"),
            "Ctrl+Shift+T"
        );
        assert_eq!(
            KeyBindings::prettify_binding("ctrl+shift+return"),
            "Ctrl+Shift+Enter"
        );
        assert_eq!(KeyBindings::prettify_binding("ctrl+backslash"), "Ctrl+\\");
        assert_eq!(KeyBindings::prettify_binding("ctrl+pageup"), "Ctrl+PageUp");
        assert_eq!(
            KeyBindings::prettify_binding("ctrl+shift+/"),
            "Ctrl+Shift+/"
        );
        // 解析失败时原样返回，帮助面板不因坏键位崩掉。
        assert_eq!(KeyBindings::prettify_binding("ctrl+bogus"), "ctrl+bogus");
    }
}
