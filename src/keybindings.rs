/// 快捷键可配置化系统
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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

    // === 窗口操作 ===
    WindowClose,
    CommandPaletteToggle,
    HelpToggle,

    // === 配置 ===
    ConfigOpen,
    ConfigClose,
    ConfigToggle,
    DebugToggle,

    // === 侧边栏 ===
    SidebarToggle,
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
            Command::WindowClose => write!(f, "window:close"),
            Command::CommandPaletteToggle => write!(f, "command_palette:toggle"),
            Command::HelpToggle => write!(f, "help:toggle"),
            Command::ConfigOpen => write!(f, "config:open"),
            Command::ConfigClose => write!(f, "config:close"),
            Command::ConfigToggle => write!(f, "config:toggle"),
            Command::DebugToggle => write!(f, "debug:toggle"),
            Command::SidebarToggle => write!(f, "sidebar:toggle"),
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
            "window:close" => Ok(Command::WindowClose),
            "command_palette:toggle" => Ok(Command::CommandPaletteToggle),
            "help:toggle" => Ok(Command::HelpToggle),
            "config:open" => Ok(Command::ConfigOpen),
            "config:close" => Ok(Command::ConfigClose),
            "config:toggle" => Ok(Command::ConfigToggle),
            "debug:toggle" => Ok(Command::DebugToggle),
            "sidebar:toggle" => Ok(Command::SidebarToggle),
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

        // 侧边栏
        bindings
            .bindings
            .insert("ctrl+\\".to_string(), "sidebar:toggle".to_string());

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

        // OSC 133 命令跳转：上一/下一个 shell 提示符
        bindings.bindings.insert(
            "ctrl+shift+up".to_string(),
            "terminal:jump_prev_command".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+down".to_string(),
            "terminal:jump_next_command".to_string(),
        );

        bindings
    }

    /// 获取快捷键对应的命令
    pub fn get_command(&self, key_str: &str) -> Option<Command> {
        let normalized = key_str.to_lowercase();
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

    /// 将 "ctrl+shift+t" 美化为 "Ctrl+Shift+T"
    fn prettify_binding(key: &str) -> String {
        key.split('+')
            .map(|tok| {
                let mut chars = tok.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join("+")
    }

    /// 加载配置文件，与默认配置合并
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let mut bindings = Self::default_bindings();

        let path = Self::config_path()?;
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let user_bindings: KeyBindings = toml::from_str(&content)?;
            // 合并用户配置到默认配置，用户配置会覆盖默认值
            for (key, value) in user_bindings.bindings {
                bindings.bindings.insert(key, value);
            }
        }

        Ok(bindings)
    }

    /// 获取配置文件路径
    pub fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Could not determine config directory")?;
        Ok(config_dir.join("jterm2/keybindings.toml"))
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
            Command::CommandPaletteToggle,
            Command::HelpToggle,
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
            ("ctrl+\\", Command::SidebarToggle),
            ("ctrl+shift+e", Command::TerminalSplitVertical),
            ("ctrl+shift+d", Command::TerminalSplitHorizontal),
            ("ctrl+alt+r", Command::SearchReplaceToggle),
            ("ctrl+shift+/", Command::HelpToggle),
            ("shift+insert", Command::EditPaste),
            ("f12", Command::DebugToggle),
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
            "ctrl+shift+b",
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
        assert_eq!(bindings.get_command("ctrl+0"), None);
    }

    #[test]
    fn numeric_tabs_use_browser_semantics_and_leave_ctrl_zero_for_zoom() {
        let bindings = KeyBindings::default_bindings();
        for index in 0..8 {
            assert_eq!(
                bindings.get_command(&format!("ctrl+{}", index + 1)),
                Some(Command::SessionJump(index))
            );
        }
        assert_eq!(bindings.get_command("ctrl+9"), Some(Command::SessionLast));
        assert_eq!(bindings.get_command("ctrl+0"), None);
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
}
