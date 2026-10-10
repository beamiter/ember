use crate::shell::ShellSession;
use crate::terminal::{ProjectionPolicy, ProjectionViewState, TerminalState};
use parking_lot::Mutex as ParkingMutex;
use std::sync::atomic::{AtomicU64, Ordering};
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

fn shell_owns_foreground_group(shell_pid: i32, foreground_pgid: Option<i32>) -> bool {
    foreground_pgid == Some(shell_pid)
}

static NEXT_SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn session_id_from_parts(pid: u32, timestamp: u128, sequence: u64) -> String {
    format!("{pid}-{timestamp}-{sequence}")
}

fn session_id_at(pid: u32, timestamp: u128, counter: &AtomicU64) -> String {
    // Relaxed ordering is sufficient: the counter allocates identities, not
    // shared state. Fail rather than reuse a sequence after u64 exhaustion.
    let mut sequence = counter.load(Ordering::Relaxed);
    loop {
        let next = sequence.checked_add(1).expect("session ID sequence exhausted");
        match counter.compare_exchange_weak(sequence, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(current) => sequence = current,
        }
    }
    session_id_from_parts(pid, timestamp, sequence)
}

/// Generate a session ID without relying on wall-clock resolution/monotonicity.
pub fn generate_session_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    session_id_at(std::process::id(), ts, &NEXT_SESSION_SEQUENCE)
}

/// Match jsh's `--session` and execution-journal grammar. Persisted metadata
/// is user-editable, so callers must validate restored IDs before putting one
/// on an argv or using it as a cross-process routing key.
pub fn is_valid_jsh_session_id(id: &str) -> bool {
    jterm_core::execution_journal::is_valid_jsh_session_id(id)
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
    /// Last time this pane posted a BEL desktop toast. Shared
    /// `bell_should_notify` spacing is per pane, not global.
    #[allow(dead_code)] // binary app reads this when draining BEL
    pub last_bell_toast: Option<std::time::Instant>,
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
            last_bell_toast: None,
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
    fn concurrent_session_sequences_are_unique() {
        let counter = AtomicU64::new(0);
        let ids = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    let counter = &counter;
                    scope.spawn(move || {
                        (0..128)
                            .map(|_| session_id_at(7, 42, counter))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|worker| worker.join().expect("ID worker completed"))
                .collect::<std::collections::HashSet<_>>()
        });
        assert_eq!(ids.len(), 1024);
        assert_eq!(counter.load(Ordering::Relaxed), 1024);
    }

    #[test]
    fn generated_identity_does_not_alias_when_clock_repeats_or_moves_backwards() {
        let counter = AtomicU64::new(0);
        let first = session_id_at(123, 900, &counter);
        let repeated_clock = session_id_at(123, 900, &counter);
        let backwards_clock = session_id_at(123, 0, &counter);
        assert_ne!(first, repeated_clock);
        assert_ne!(first, backwards_clock);
        assert_ne!(repeated_clock, backwards_clock);
        for id in [
            first,
            repeated_clock,
            backwards_clock,
            session_id_from_parts(u32::MAX, u128::MAX, u64::MAX),
        ] {
            assert!(is_valid_jsh_session_id(&id));
            assert!(id.len() <= jterm_core::execution_journal::MAX_JSH_SESSION_ID_BYTES);
        }
    }

    #[test]
    fn final_available_sequence_advances_to_exhaustion_without_wrapping() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(
            session_id_at(7, 42, &counter),
            session_id_from_parts(7, 42, u64::MAX - 1)
        );
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    #[should_panic(expected = "session ID sequence exhausted")]
    fn exhausted_sequence_does_not_wrap_to_an_existing_identity() {
        let counter = AtomicU64::new(u64::MAX);
        let _ = session_id_at(123, 900, &counter);
    }

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
        let max = jterm_core::execution_journal::MAX_JSH_SESSION_ID_BYTES;
        assert!(is_valid_jsh_session_id(&"s".repeat(max)));
        assert!(!is_valid_jsh_session_id(&"s".repeat(max + 1)));
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
