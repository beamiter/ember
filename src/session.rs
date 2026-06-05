use crate::shell::ShellSession;
use crate::terminal::TerminalState;
use parking_lot::Mutex as ParkingMutex;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
}

impl SessionMetadata {
    pub fn new(name: String, tags: Vec<String>) -> Self {
        SessionMetadata {
            name,
            tags,
            session_id: generate_session_id(),
            last_active: Instant::now(),
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
}

impl Session {
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
}
