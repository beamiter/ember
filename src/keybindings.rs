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
    TerminalSplitHorizontal, // Ctrl+Shift+O
    TerminalClosePane,       // Ctrl+Shift+W
    PaneFocusNext,           // Alt+Tab
    PaneFocusPrev,           // Alt+Shift+Tab

    // === 窗口操作 ===
    WindowClose,

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
            Command::WindowClose => write!(f, "window:close"),
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
            "window:close" => Ok(Command::WindowClose),
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

        // 会话切换（用户可见编号从 1 开始）。Ctrl+0 保留给字号复位，
        // 避免一次按键同时切换会话并重置字号。
        for i in 0..9 {
            bindings
                .bindings
                .insert(format!("ctrl+{}", i + 1), format!("session:jump:{}", i));
        }

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
            "ctrl+shift+r".to_string(),
            "search:replace:toggle".to_string(),
        );

        // 配置操作
        bindings
            .bindings
            .insert("ctrl+shift+,".to_string(), "config:toggle".to_string());
        bindings
            .bindings
            .insert("f12".to_string(), "debug:toggle".to_string());

        // 侧边栏
        bindings
            .bindings
            .insert("ctrl+shift+b".to_string(), "sidebar:toggle".to_string());

        // 终端操作
        bindings
            .bindings
            .insert("ctrl+up".to_string(), "terminal:scroll_up".to_string());
        bindings
            .bindings
            .insert("ctrl+down".to_string(), "terminal:scroll_down".to_string());
        // Terminator-compatible pane/window shortcuts.
        bindings.bindings.insert(
            "ctrl+shift+o".to_string(),
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
        bindings
            .bindings
            .insert("alt+right".to_string(), "pane:focus_next".to_string());
        bindings
            .bindings
            .insert("alt+left".to_string(), "pane:focus_prev".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+q".to_string(), "window:close".to_string());

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
    }

    #[test]
    fn test_default_bindings() {
        let bindings = KeyBindings::default_bindings();
        assert!(bindings.get_command("ctrl+shift+t").is_some());
        assert_eq!(
            bindings.get_command("ctrl+shift+t"),
            Some(Command::SessionNew)
        );
        assert_eq!(
            bindings.get_command("shift+insert"),
            Some(Command::EditPaste)
        );
        assert_eq!(
            bindings.get_command("ctrl+shift+o"),
            Some(Command::TerminalSplitHorizontal)
        );
        assert_eq!(
            bindings.get_command("ctrl+shift+e"),
            Some(Command::TerminalSplitVertical)
        );
        assert_eq!(
            bindings.get_command("ctrl+shift+w"),
            Some(Command::TerminalClosePane)
        );
        assert_eq!(
            bindings.get_command("ctrl+shift+q"),
            Some(Command::WindowClose)
        );
        assert_eq!(bindings.get_command("ctrl+0"), None);
        assert_eq!(
            bindings.get_command("ctrl+1"),
            Some(Command::SessionJump(0))
        );
        assert_eq!(
            bindings.get_command("ctrl+9"),
            Some(Command::SessionJump(8))
        );
        assert_eq!(bindings.get_command("f12"), Some(Command::DebugToggle));
    }

    #[test]
    fn test_command_display() {
        assert_eq!(Command::SessionNew.to_string(), "session:new");
        assert_eq!(Command::SessionJump(3).to_string(), "session:jump:3");
        assert_eq!(Command::DebugToggle.to_string(), "debug:toggle");
    }
}
