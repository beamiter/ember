use crate::shell::ShellSession;
use crate::terminal::{ProjectionPolicy, ProjectionViewState, TerminalState};
use parking_lot::Mutex as ParkingMutex;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// UI-side retry storage is separate from the bounded PTY writer, but must be
/// bounded as well: a stopped child plus key repeat must not grow memory
/// forever. Clipboard payloads that cannot fit remain in the paste-confirm
/// flow instead of entering this queue.
pub const PENDING_INPUT_BYTE_CAP: usize = 8 * 1024 * 1024;
const MAX_JSH_SESSION_ID_BYTES: usize = 128;

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

fn shell_owns_foreground_group(shell_pid: i32, foreground_pgid: Option<i32>) -> bool {
    foreground_pgid == Some(shell_pid)
}

/// Generate a unique session ID for jsh session persistence.
pub fn generate_session_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{}", std::process::id(), ts)
}

/// Match jsh's `--session` and execution-journal grammar. Persisted metadata
/// is user-editable, so callers must validate restored IDs before putting one
/// on an argv or using it as a cross-process routing key.
pub fn is_valid_jsh_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_JSH_SESSION_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
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
    #[allow(dead_code)]
    pub fn new(name: String, tags: Vec<String>) -> Self {
        Self::with_session_id(name, tags, generate_session_id())
    }

    /// Build metadata around the ID that was assigned before the shell was
    /// spawned. jsh receives the same value through `--session`, so terminal
    /// routing, shell snapshots, and the execution journal share one stable
    /// identity from the first byte of PTY output onward.
    pub fn with_session_id(name: String, tags: Vec<String>, session_id: String) -> Self {
        debug_assert!(is_valid_jsh_session_id(&session_id));
        SessionMetadata {
            name,
            tags,
            session_id,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPurpose {
    /// Ordinary interactive shell; safe to restore from cwd metadata.
    Interactive,
    /// Exact-argv helper or Agent CLI. Its argv/lifecycle owner is not part of
    /// the ordinary shell snapshot, so restoring it as a shell would be false.
    EphemeralCommand,
    /// An Agent CLI whose child has exited. Keep its terminal buffer available
    /// for explicit human review, but never poll or restore it as a live shell.
    RetainedCommand,
}

/// Session - 完整的会话，包含终端状态和 Shell 会话
pub struct Session {
    pub metadata: SessionMetadata,
    pub terminal: Arc<ParkingMutex<TerminalState>>,
    pub shell: ShellSession,
    pub purpose: SessionPurpose,
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
    /// Session-owned view policy. Renderers are reused across tabs/panes and
    /// must never carry collapse state or projected scroll between sessions.
    pub projection_policy: ProjectionPolicy,
    pub projection_view_state: ProjectionViewState,
    /// Last policy/provenance generations whose requested collapses were
    /// checked for permanent eviction. Rendering can then keep the P1
    /// fail-closed cleanup off the steady-state frame path.
    #[allow(dead_code)]
    // Used by the binary app; the library Session type is built separately.
    pub(crate) collapse_availability_cache: Option<(u64, u64)>,
}

impl Session {
    /// Append bytes to this session's strict input FIFO without crossing its
    /// memory cap. `false` guarantees that no byte was appended.
    pub fn queue_input(&mut self, input: &[u8]) -> bool {
        if self.purpose == SessionPurpose::RetainedCommand {
            return false;
        }
        let queued = append_bounded_input(&mut self.pending_input, input, PENDING_INPUT_BYTE_CAP);
        if queued {
            self.terminal.lock().note_user_input(input);
        }
        queued
    }

    /// Queue an already-armed Agent command without marking it as unrelated
    /// local input. Callers must arm the terminal generation first.
    pub fn queue_agent_input(&mut self, input: &[u8]) -> bool {
        if self.purpose == SessionPurpose::RetainedCommand {
            return false;
        }
        append_bounded_input(&mut self.pending_input, input, PENDING_INPUT_BYTE_CAP)
    }

    /// An OSC 133 prompt marker is necessary but not sufficient for Agent
    /// approval: a foreground editor/test can emit the same bytes. Require the
    /// interactive shell's own process group to own the controlling terminal
    /// immediately before arming and queueing the reviewed command.
    pub fn shell_owns_foreground_pty(&self) -> bool {
        let shell_pid = self.get_shell_pid();
        shell_owns_foreground_group(
            shell_pid,
            jterm_core::process::foreground_pgid_via_stat(shell_pid),
        )
    }

    #[allow(dead_code)]
    pub fn new(
        name: String,
        tags: Vec<String>,
        terminal: Arc<ParkingMutex<TerminalState>>,
        shell: ShellSession,
    ) -> Self {
        Self::new_with_session_id(name, tags, terminal, shell, generate_session_id())
    }

    pub fn new_with_session_id(
        name: String,
        tags: Vec<String>,
        terminal: Arc<ParkingMutex<TerminalState>>,
        shell: ShellSession,
        session_id: String,
    ) -> Self {
        Session {
            metadata: SessionMetadata::with_session_id(name, tags, session_id),
            terminal,
            shell,
            purpose: SessionPurpose::Interactive,
            pending_input: Vec::new(),
            pending_output: Vec::new(),
            projection_policy: ProjectionPolicy::new(),
            projection_view_state: ProjectionViewState::new(),
            collapse_availability_cache: None,
        }
    }

    #[allow(dead_code)]
    pub fn with_default_name(
        index: usize,
        terminal: Arc<ParkingMutex<TerminalState>>,
        shell: ShellSession,
    ) -> Self {
        let name = SessionMetadata::default_name(index);
        Session::new(name, Vec::new(), terminal, shell)
    }

    pub fn with_default_name_and_session_id(
        index: usize,
        terminal: Arc<ParkingMutex<TerminalState>>,
        shell: ShellSession,
        session_id: String,
    ) -> Self {
        let name = SessionMetadata::default_name(index);
        Session::new_with_session_id(name, Vec::new(), terminal, shell, session_id)
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
    fn explicit_session_id_is_preserved() {
        let metadata = SessionMetadata::with_session_id(
            "Test".to_string(),
            Vec::new(),
            "stable-session".to_string(),
        );
        assert_eq!(metadata.session_id, "stable-session");
    }

    #[test]
    fn jsh_session_id_grammar_rejects_argv_unsafe_metadata() {
        for valid in ["123-456", "tab_1", "ABC"] {
            assert!(is_valid_jsh_session_id(valid), "{valid}");
        }
        for invalid in ["", "has.dot", "has space", "../escape", "雪"] {
            assert!(!is_valid_jsh_session_id(invalid), "{invalid}");
        }
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

    #[test]
    fn agent_requires_the_shell_process_group_in_the_foreground() {
        assert!(shell_owns_foreground_group(100, Some(100)));
        assert!(!shell_owns_foreground_group(100, Some(200)));
        assert!(!shell_owns_foreground_group(100, None));
    }
}
