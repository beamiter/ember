use crate::shell::ShellSession;
use crate::terminal::TerminalState;
use parking_lot::Mutex as ParkingMutex;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// UI-side retry storage is separate from the bounded PTY writer, but must be
/// bounded as well: a stopped child plus key repeat must not grow memory
/// forever. Clipboard payloads that cannot fit remain in the paste-confirm
/// flow instead of entering this queue.
pub const PENDING_INPUT_BYTE_CAP: usize = 8 * 1024 * 1024;

fn append_bounded_input(buffer: &mut Vec<u8>, input: &[u8], cap: usize) -> bool {
    let Some(total) = buffer.len().checked_add(input.len()) else {
        return false;
    };
    if total > cap {
        return false;
    }
    buffer.extend_from_slice(input);
    true
}

/// Generate a unique session ID for rsh session persistence.
pub fn generate_session_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{}", std::process::id(), ts)
}

/// Session metadata - 会话元数据
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    pub name: String,
    pub tags: Vec<String>,
    pub session_id: String,
    pub last_active: Instant,
    /// 后台会话在用户未查看期间是否有新输出。切换到该会话时清零。
    /// 用作 tab 上的活动指示点,避免用户在多 tab 间反复切换确认。
    pub unseen_output: bool,
    /// 用户通过双击 tab 显式重命名后保留的标题。Some 时覆盖 CWD-derived
    /// 标题(仍由 session_cwd_title 读取);None 表示沿用默认推导。空字符串
    /// 视同 None,由提交逻辑负责规范化,避免 UI 显示空标签。
    pub custom_name: Option<String>,
}

impl SessionMetadata {
    pub fn new(name: String, tags: Vec<String>) -> Self {
        SessionMetadata {
            name,
            tags,
            session_id: generate_session_id(),
            last_active: Instant::now(),
            unseen_output: false,
            custom_name: None,
        }
    }

    pub fn default_name(index: usize) -> String {
        format!("Session {}", index + 1)
    }

    pub fn update_last_active(&mut self) {
        self.last_active = Instant::now();
    }
}

/// Session - 完整的会话，包含终端状态和 Shell 会话
pub struct Session {
    pub metadata: SessionMetadata,
    pub terminal: Arc<ParkingMutex<TerminalState>>,
    pub shell: ShellSession,
    /// User input accepted by the UI but not yet admitted to this session's
    /// bounded PTY write queue. Keeping the retry buffer on the session is a
    /// correctness boundary: switching tabs while the writer is backpressured
    /// must never deliver bytes to a different shell.
    pub pending_input: Vec<u8>,
    /// PTY output that exceeded the per-frame parsing budget.
    ///
    /// This must live on the session rather than on `TerminalApp`: a tab switch
    /// can happen between frames, and feeding the old tab's remaining ANSI
    /// stream into the newly active terminal corrupts both its screen and its
    /// terminal modes.
    pub pending_output: Vec<u8>,
}

impl Session {
    /// Append bytes to this session's strict input FIFO without crossing its
    /// memory cap. `false` guarantees that no byte was appended.
    pub fn queue_input(&mut self, input: &[u8]) -> bool {
        append_bounded_input(&mut self.pending_input, input, PENDING_INPUT_BYTE_CAP)
    }

    pub fn new(
        name: String,
        tags: Vec<String>,
        terminal: Arc<ParkingMutex<TerminalState>>,
        shell: ShellSession,
    ) -> Self {
        Session {
            metadata: SessionMetadata::new(name, tags),
            terminal,
            shell,
            pending_input: Vec::new(),
            pending_output: Vec::new(),
        }
    }

    pub fn with_default_name(
        index: usize,
        terminal: Arc<ParkingMutex<TerminalState>>,
        shell: ShellSession,
    ) -> Self {
        let name = SessionMetadata::default_name(index);
        Session::new(name, Vec::new(), terminal, shell)
    }

    /// 获取 shell 子进程的 PID
    pub fn get_shell_pid(&self) -> i32 {
        self.shell.get_child_pid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_metadata() {
        let metadata = SessionMetadata::new("Test".to_string(), vec!["tag1".to_string()]);
        assert_eq!(metadata.name, "Test");
        assert_eq!(metadata.tags.len(), 1);
        assert_eq!(metadata.tags[0], "tag1");
    }

    #[test]
    fn test_default_name() {
        assert_eq!(SessionMetadata::default_name(0), "Session 1");
        assert_eq!(SessionMetadata::default_name(5), "Session 6");
    }

    #[test]
    fn pending_input_cap_is_atomic() {
        let mut pending = b"old".to_vec();
        assert!(append_bounded_input(&mut pending, b"12", 5));
        assert_eq!(pending, b"old12");
        assert!(!append_bounded_input(&mut pending, b"x", 5));
        assert_eq!(pending, b"old12");
    }
}
