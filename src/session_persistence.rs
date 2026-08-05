use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicI32, Ordering};

pub const MAX_SESSION_SNAPSHOT_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_RESTORED_SESSIONS: usize = 64;
const MAX_SESSION_NAME_BYTES: usize = 256;
const MAX_SESSION_TAGS: usize = 32;
const MAX_SESSION_TAG_BYTES: usize = 128;
const MAX_SESSION_CWD_BYTES: usize = 4096;
const MAX_RESTORED_LAYOUT_DEPTH: usize = 64;
const MAX_RESTORED_LAYOUT_NODES: usize = MAX_RESTORED_SESSIONS * 2 - 1;
const INSTANCE_LOCK_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_millis(100);
const INSTANCE_LOCK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2);
const NO_INSTANCE_LOCK_FD: i32 = -1;
static INSTANCE_LOCK_FD: AtomicI32 = AtomicI32::new(NO_INSTANCE_LOCK_FD);

fn default_split_ratio() -> f32 {
    0.5
}

/// 持久化布局只引用稳定 session ID，不保存运行期的 session 数组索引。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutSnapshot {
    pub root: LayoutNodeSnapshot,
    #[serde(default)]
    pub focused_session_id: Option<String>,
    /// 固定的 tab 重启后仍固定。旧快照没有这个字段，恢复为未固定。
    #[serde(default)]
    pub pinned: bool,
    /// 同上，用于「重要」标记（多选模型）。
    #[serde(default)]
    pub marked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutNodeSnapshot {
    Pane {
        session_id: String,
    },
    Split {
        horizontal: bool,
        #[serde(default = "default_split_ratio")]
        ratio: f32,
        first: Box<LayoutNodeSnapshot>,
        second: Box<LayoutNodeSnapshot>,
    },
}

/// 布局损坏不应连带丢失整个 session 列表。先读为 Value，再单独尝试解析；
/// 失败时退化成 `None`，启动端会恢复为单 pane。
fn deserialize_optional_layout<'de, D>(deserializer: D) -> Result<Option<LayoutSnapshot>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// 同上，用于 per-tab 布局列表：整段解析失败退回空列表，启动端会回落到
/// `layout` 字段（旧快照）或单 tab。
fn deserialize_tab_layouts<'de, D>(deserializer: D) -> Result<Vec<LayoutSnapshot>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// 会话持久化数据结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub name: String,
    pub tags: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    /// 用户在 tab 上双击重命名后的显示名;Some 时覆盖 CWD-derived 标题。
    #[serde(default)]
    pub custom_name: Option<String>,
}

/// 会话列表快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsSnapshot {
    pub version: u32,
    pub sessions: Vec<SessionSnapshot>,
    #[serde(default)]
    pub active_index: Option<usize>,
    /// 兼容字段：v3 及更早只有一棵全局布局树。写入时填当前 tab 的布局，
    /// 让旧版本仍能打开新快照；读取时只在 `tabs` 缺失时用于迁移。
    #[serde(default, deserialize_with = "deserialize_optional_layout")]
    pub layout: Option<LayoutSnapshot>,
    /// v4：每个 tab 一棵布局树。窗格归 tab 所有，因此布局也必须按 tab 存。
    #[serde(default, deserialize_with = "deserialize_tab_layouts")]
    pub tabs: Vec<LayoutSnapshot>,
    #[serde(default)]
    pub active_tab: Option<usize>,
}

impl SessionsSnapshot {
    /// 从会话快照列表创建
    pub fn from_snapshots(
        sessions: Vec<SessionSnapshot>,
        active_index: Option<usize>,
        tabs: Vec<LayoutSnapshot>,
        active_tab: Option<usize>,
    ) -> Self {
        // `layout` 保留给旧版本读取：给它当前 tab 的布局，旧版本至少能开出
        // 用户最后看到的那组窗格，而不是一个空布局。
        let layout = active_tab
            .and_then(|idx| tabs.get(idx))
            .or_else(|| tabs.first())
            .cloned();
        SessionsSnapshot {
            version: 4,
            sessions,
            active_index,
            layout,
            tabs,
            active_tab,
        }
    }

    /// 保存到文件（原子写入 + fsync 持久化）
    pub fn save(&self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        // Apply the same resource contract on write and read. Runtime metadata
        // can originate in text fields, so writing it verbatim could otherwise
        // create a snapshot that this application rejects on its next launch.
        let mut bounded = self.clone();
        bounded.version = 4;
        let warnings = bounded.sanitize();
        for warning in warnings {
            eprintln!("[SessionPersistence] WARNING while saving: {warning}");
        }
        let json = serde_json::to_vec_pretty(&bounded)?;
        if json.len() as u64 > MAX_SESSION_SNAPSHOT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                format!(
                    "bounded session snapshot is {} bytes; limit is {}",
                    json.len(),
                    MAX_SESSION_SNAPSHOT_BYTES
                ),
            )
            .into());
        }
        crate::persistence_file::write_atomic(path, &json)?;
        eprintln!("[SessionPersistence] Sessions saved to {}", path.display());
        Ok(())
    }

    /// 从文件加载，并报告为了保证启动资源有界而执行的修复。
    pub fn load_with_warnings(
        path: &std::path::Path,
    ) -> Result<(Self, Vec<String>), Box<dyn std::error::Error>> {
        // The local boundary deliberately fronts the pinned core revision: it
        // opens with O_NOFOLLOW/O_NONBLOCK, validates owner/link count through
        // the descriptor, and keeps the total read under the hard limit.
        let content = match crate::persistence_file::read_bounded(path, MAX_SESSION_SNAPSHOT_BYTES)
        {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((
                    SessionsSnapshot {
                        version: 4,
                        sessions: vec![],
                        active_index: None,
                        layout: None,
                        tabs: Vec::new(),
                        active_tab: None,
                    },
                    Vec::new(),
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let mut snapshot: SessionsSnapshot = serde_json::from_str(&content)?;
        if !(1..=4).contains(&snapshot.version) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unsupported session snapshot version {}", snapshot.version),
            )
            .into());
        }
        let warnings = snapshot.sanitize();
        eprintln!(
            "[SessionPersistence] Sessions loaded from {}",
            path.display()
        );
        Ok((snapshot, warnings))
    }

    /// Compatibility wrapper for callers that do not need sanitization notes.
    #[allow(dead_code)]
    pub fn load(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        Self::load_with_warnings(path).map(|(snapshot, _warnings)| snapshot)
    }

    fn sanitize(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.sessions.len() > MAX_RESTORED_SESSIONS {
            warnings.push(format!(
                "restored only the first {MAX_RESTORED_SESSIONS} of {} sessions",
                self.sessions.len()
            ));
            self.sessions.truncate(MAX_RESTORED_SESSIONS);
        }

        let mut repaired_fields = 0usize;
        for session in &mut self.sessions {
            repaired_fields += usize::from(sanitize_display_text(
                &mut session.name,
                MAX_SESSION_NAME_BYTES,
            ));
            if session.name.is_empty() {
                session.name = "Session".to_string();
                repaired_fields += 1;
            }
            if session.tags.len() > MAX_SESSION_TAGS {
                session.tags.truncate(MAX_SESSION_TAGS);
                repaired_fields += 1;
            }
            for tag in &mut session.tags {
                repaired_fields += usize::from(sanitize_display_text(tag, MAX_SESSION_TAG_BYTES));
            }
            if session
                .cwd
                .as_ref()
                .is_some_and(|cwd| cwd.len() > MAX_SESSION_CWD_BYTES || cwd.as_bytes().contains(&0))
            {
                session.cwd = None;
                repaired_fields += 1;
            }
            if let Some(name) = session.custom_name.as_mut() {
                repaired_fields += usize::from(sanitize_display_text(name, MAX_SESSION_NAME_BYTES));
                if name.is_empty() {
                    session.custom_name = None;
                    repaired_fields += 1;
                }
            }
            if session
                .session_id
                .as_deref()
                .is_some_and(|id| !crate::session::is_valid_jsh_session_id(id))
            {
                session.session_id = None;
                repaired_fields += 1;
            }
        }
        if repaired_fields > 0 {
            warnings.push(format!(
                "repaired {repaired_fields} oversized or invalid session fields"
            ));
        }

        if self
            .active_index
            .is_some_and(|index| index >= self.sessions.len())
        {
            self.active_index = self.sessions.len().checked_sub(1);
            warnings.push("active session index was outside the restored list".to_string());
        }

        let allowed_session_ids = self
            .sessions
            .iter()
            .filter_map(|session| session.session_id.as_ref())
            .cloned()
            .collect::<HashSet<_>>();
        // 一个会话至多出现在一个窗格里，而这个约束跨 tab 成立——否则两个
        // tab 会争夺同一个 PTY。`used_session_ids` 因此在所有 tab 间共享。
        let mut used_session_ids = HashSet::new();
        let mut node_count = 0usize;
        let mut layout_repaired = false;

        let raw_tabs = std::mem::take(&mut self.tabs);
        let migrating = raw_tabs.is_empty();
        let raw_tabs = if migrating {
            self.layout.take().into_iter().collect()
        } else {
            raw_tabs
        };

        for tab in raw_tabs {
            let root = sanitize_layout_node(
                tab.root,
                &allowed_session_ids,
                &mut used_session_ids,
                &mut node_count,
                0,
                &mut layout_repaired,
            );
            match root {
                Some(root) => {
                    let focused_session_id = tab.focused_session_id.filter(|session_id| {
                        let keep = used_session_ids.contains(session_id);
                        layout_repaired |= !keep;
                        keep
                    });
                    self.tabs.push(LayoutSnapshot {
                        root,
                        focused_session_id,
                        pinned: tab.pinned,
                        marked: tab.marked,
                    });
                }
                // 空 tab 不是可渲染的状态，整个丢掉；它的会话会在启动时
                // 作为孤儿各自获得一个新 tab。
                None => layout_repaired = true,
            }
        }

        if self
            .active_tab
            .is_some_and(|index| index >= self.tabs.len())
        {
            self.active_tab = self.tabs.len().checked_sub(1);
            layout_repaired = true;
        }
        self.layout = self
            .active_tab
            .and_then(|idx| self.tabs.get(idx))
            .or_else(|| self.tabs.first())
            .cloned();

        if layout_repaired {
            warnings.push("repaired an invalid or oversized pane layout".to_string());
        }
        warnings
    }
}

fn sanitize_layout_node(
    node: LayoutNodeSnapshot,
    allowed_session_ids: &HashSet<String>,
    used_session_ids: &mut HashSet<String>,
    node_count: &mut usize,
    depth: usize,
    repaired: &mut bool,
) -> Option<LayoutNodeSnapshot> {
    if depth > MAX_RESTORED_LAYOUT_DEPTH || *node_count >= MAX_RESTORED_LAYOUT_NODES {
        *repaired = true;
        return None;
    }
    *node_count += 1;

    match node {
        LayoutNodeSnapshot::Pane { session_id } => {
            if allowed_session_ids.contains(&session_id)
                && used_session_ids.insert(session_id.clone())
            {
                Some(LayoutNodeSnapshot::Pane { session_id })
            } else {
                *repaired = true;
                None
            }
        }
        LayoutNodeSnapshot::Split {
            horizontal,
            ratio,
            first,
            second,
        } => {
            let first = sanitize_layout_node(
                *first,
                allowed_session_ids,
                used_session_ids,
                node_count,
                depth + 1,
                repaired,
            );
            let second = sanitize_layout_node(
                *second,
                allowed_session_ids,
                used_session_ids,
                node_count,
                depth + 1,
                repaired,
            );
            match (first, second) {
                (Some(first), Some(second)) => {
                    let normalized_ratio = if ratio.is_finite() {
                        ratio.clamp(0.1, 0.9)
                    } else {
                        0.5
                    };
                    *repaired |= normalized_ratio != ratio;
                    Some(LayoutNodeSnapshot::Split {
                        horizontal,
                        ratio: normalized_ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    })
                }
                (Some(remaining), None) | (None, Some(remaining)) => {
                    *repaired = true;
                    Some(remaining)
                }
                (None, None) => {
                    *repaired = true;
                    None
                }
            }
        }
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) -> bool {
    if value.len() <= max_bytes {
        return false;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    true
}

fn is_bidi_display_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn sanitize_display_text(value: &mut String, max_bytes: usize) -> bool {
    let original = value.clone();
    value.retain(|ch| !ch.is_control() && !is_bidi_display_control(ch));
    *value = value.trim().to_string();
    truncate_utf8(value, max_bytes);
    *value != original
}

pub fn bounded_session_name(value: &str) -> String {
    let mut value = value.to_string();
    sanitize_display_text(&mut value, MAX_SESSION_NAME_BYTES);
    value
}

fn validate_instance_lock_file(file: &std::fs::File) -> std::io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "instance lock is not a regular file",
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.nlink() != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "instance lock must have exactly one hard link",
            ));
        }
        // SAFETY: geteuid has no preconditions and only reads process state.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "instance lock is not owned by the current user",
            ));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }

    Ok(())
}

/// Owns the primary-instance lock and publishes its descriptor so a freshly
/// forked PTY child can close its inherited copy before doing any other work.
/// `FD_CLOEXEC` alone is insufficient because `flock` remains held between
/// `fork` and `execve`, and can survive the parent if that child stalls.
pub struct InstanceLock {
    file: std::fs::File,
}

impl InstanceLock {
    fn register(file: std::fs::File) -> std::io::Result<Self> {
        use std::os::unix::io::AsRawFd;

        let fd = file.as_raw_fd();
        INSTANCE_LOCK_FD
            .compare_exchange(NO_INSTANCE_LOCK_FD, fd, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "an instance lock descriptor is already registered",
                )
            })?;
        Ok(Self { file })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;

        let fd = self.file.as_raw_fd();
        let _ = INSTANCE_LOCK_FD.compare_exchange(
            fd,
            NO_INSTANCE_LOCK_FD,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// Capture before `fork`; the child must only use the returned integer with
/// async-signal-safe `close(2)`.
pub(crate) fn inherited_instance_lock_fd() -> libc::c_int {
    INSTANCE_LOCK_FD.load(Ordering::Acquire)
}

fn try_acquire_instance_lock_at(
    lock_path: &std::path::Path,
) -> std::io::Result<Option<std::fs::File>> {
    crate::persistence_file::ensure_parent(lock_path)?;

    // Do not truncate before flock: a losing second instance must leave the
    // lock owner's diagnostic PID intact.
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let mut file = options.open(lock_path)?;
    validate_instance_lock_file(&file)?;

    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // Retain a small bounded retry for transient duplicated descriptors. PTY
    // children explicitly close the registered descriptor immediately after
    // fork, so application-spawned children cannot extend the lock lifetime.
    let retry_deadline = std::time::Instant::now() + INSTANCE_LOCK_RETRY_WINDOW;
    loop {
        // LOCK_EX | LOCK_NB: 非阻塞排他锁
        // SAFETY: flock 对有效的文件描述符是安全的。fd 来自有效的 File 对象，
        // 标志是合法的 flock 常量。File 对象的生命周期确保 fd 在调用期间有效。
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if ret == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error);
        }
        let now = std::time::Instant::now();
        if now >= retry_deadline {
            return Ok(None);
        }
        std::thread::sleep(
            INSTANCE_LOCK_RETRY_INTERVAL.min(retry_deadline.saturating_duration_since(now)),
        );
    }

    // Re-check after acquiring the lock and immediately before mutation. This
    // rejects a regular file that gained a second directory entry between open
    // and flock instead of truncating another path's contents through a hard
    // link. O_NOFOLLOW above handles symbolic links at open time.
    validate_instance_lock_file(&file)?;

    // Only the lock owner may replace the diagnostic PID.
    use std::io::{Seek, Write};
    file.set_len(0)?;
    file.rewind()?;
    write!(file, "{}", std::process::id())?;
    file.sync_all()?;
    Ok(Some(file))
}

/// 尝试获取实例锁文件。成功返回持锁守卫，失败表示已有实例在运行。
pub fn try_acquire_instance_lock() -> Option<InstanceLock> {
    let lock_path = dirs::config_dir()?.join("ember").join("instance.lock");
    match try_acquire_instance_lock_at(&lock_path) {
        Ok(Some(file)) => match InstanceLock::register(file) {
            Ok(lock) => Some(lock),
            Err(error) => {
                eprintln!(
                    "[SessionPersistence] Failed to register instance lock {}: {}",
                    lock_path.display(),
                    error
                );
                None
            }
        },
        Ok(None) => None,
        Err(error) => {
            eprintln!(
                "[SessionPersistence] Failed to acquire instance lock {}: {}",
                lock_path.display(),
                error
            );
            None
        }
    }
}

/// 确保会话历史目录存在
pub fn ensure_session_history_dir(
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    crate::persistence_file::ensure_parent(path).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_LOCK_TEST: AtomicU64 = AtomicU64::new(0);

    fn write_private(path: &std::path::Path, contents: impl AsRef<[u8]>) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let id = NEXT_LOCK_TEST.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ember-instance-lock-test-{}-{label}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    struct ForkChild(libc::pid_t);

    #[cfg(unix)]
    impl Drop for ForkChild {
        fn drop(&mut self) {
            // SAFETY: the stored PID was returned by fork in this test. SIGKILL
            // guarantees a child blocked in pause exits, and waitpid reaps it.
            unsafe {
                let _ = libc::kill(self.0, libc::SIGKILL);
                let mut status = 0;
                while libc::waitpid(self.0, &mut status, 0) < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                }
            }
        }
    }

    #[test]
    fn test_snapshot_conversion() {
        let snapshots = vec![
            SessionSnapshot {
                name: "Session 1".to_string(),
                tags: vec!["dev".to_string()],
                cwd: Some("/home/user".to_string()),
                session_id: Some("123-456".to_string()),
                custom_name: None,
            },
            SessionSnapshot {
                name: "Session 2".to_string(),
                tags: vec!["test".to_string()],
                cwd: Some("/tmp".to_string()),
                session_id: None,
                custom_name: None,
            },
        ];

        let layout = LayoutSnapshot {
            root: LayoutNodeSnapshot::Split {
                horizontal: false,
                ratio: 0.6,
                first: Box::new(LayoutNodeSnapshot::Pane {
                    session_id: "123-456".to_string(),
                }),
                second: Box::new(LayoutNodeSnapshot::Pane {
                    session_id: "second-session".to_string(),
                }),
            },
            focused_session_id: Some("second-session".to_string()),
            pinned: false,
            marked: false,
        };
        let snapshot =
            SessionsSnapshot::from_snapshots(snapshots, Some(1), vec![layout.clone()], Some(0));
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.sessions[0].cwd, Some("/home/user".to_string()));
        assert_eq!(snapshot.sessions[1].cwd, Some("/tmp".to_string()));
        assert_eq!(snapshot.active_index, Some(1));
        assert_eq!(snapshot.version, 4);
        assert_eq!(snapshot.tabs, vec![layout.clone()]);
        // 旧字段仍然写出当前 tab 的布局，供旧版本读取。
        assert_eq!(snapshot.layout, Some(layout.clone()));

        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: SessionsSnapshot = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.tabs, vec![layout.clone()]);
        assert_eq!(decoded.active_tab, Some(0));
        assert_eq!(decoded.layout, Some(layout));
    }

    #[test]
    fn test_backward_compat_deserialization() {
        let json =
            r#"{"version":1,"sessions":[{"name":"Session 1","tags":[],"cwd":"/home/user"}]}"#;
        let snapshot: SessionsSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snapshot.sessions[0].session_id, None);
        assert_eq!(snapshot.active_index, None);
        assert_eq!(snapshot.layout, None);
    }

    #[test]
    fn malformed_layout_does_not_prevent_session_restore() {
        let json = r#"{
            "version": 3,
            "sessions": [{"name": "Session 1", "tags": []}],
            "active_index": 0,
            "layout": {"root": {"kind": "unknown"}}
        }"#;
        let snapshot: SessionsSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.active_index, Some(0));
        assert_eq!(snapshot.layout, None);
    }

    /// v3 快照只有一棵全局布局树。它描述的是用户最后看到的那组窗格,所以
    /// 迁移时应该原样变成第一个 tab,而不是散成一堆单窗格 tab。
    #[test]
    fn a_v3_snapshot_migrates_its_single_layout_into_the_first_tab() {
        let json = r#"{
            "version": 3,
            "sessions": [
                {"name": "a", "tags": [], "session_id": "session-a"},
                {"name": "b", "tags": [], "session_id": "session-b"}
            ],
            "active_index": 1,
            "layout": {
                "root": {
                    "kind": "split",
                    "horizontal": true,
                    "ratio": 0.5,
                    "first": {"kind": "pane", "session_id": "session-a"},
                    "second": {"kind": "pane", "session_id": "session-b"}
                },
                "focused_session_id": "session-b"
            }
        }"#;
        let mut snapshot: SessionsSnapshot = serde_json::from_str(json).unwrap();
        assert!(snapshot.tabs.is_empty());

        let warnings = snapshot.sanitize();

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(
            snapshot.tabs[0].focused_session_id.as_deref(),
            Some("session-b")
        );
        assert!(matches!(
            snapshot.tabs[0].root,
            LayoutNodeSnapshot::Split { .. }
        ));
    }

    /// 同一个会话不能同时出现在两个 tab 的窗格里,否则两个 tab 会争夺同一个
    /// PTY——去重必须跨 tab 生效,而不只是在单棵树内。
    #[test]
    fn a_session_cannot_appear_in_two_tabs() {
        let pane = |id: &str| LayoutSnapshot {
            root: LayoutNodeSnapshot::Pane {
                session_id: id.to_string(),
            },
            focused_session_id: Some(id.to_string()),
            pinned: false,
            marked: false,
        };
        let mut snapshot = SessionsSnapshot::from_snapshots(
            vec![SessionSnapshot {
                name: "a".to_string(),
                tags: vec![],
                cwd: None,
                session_id: Some("session-a".to_string()),
                custom_name: None,
            }],
            Some(0),
            vec![pane("session-a"), pane("session-a")],
            Some(0),
        );

        let warnings = snapshot.sanitize();

        assert_eq!(snapshot.tabs.len(), 1);
        assert!(!warnings.is_empty());
    }

    #[test]
    fn an_out_of_range_active_tab_is_clamped() {
        let mut snapshot = SessionsSnapshot::from_snapshots(
            vec![SessionSnapshot {
                name: "a".to_string(),
                tags: vec![],
                cwd: None,
                session_id: Some("session-a".to_string()),
                custom_name: None,
            }],
            Some(0),
            vec![LayoutSnapshot {
                root: LayoutNodeSnapshot::Pane {
                    session_id: "session-a".to_string(),
                },
                focused_session_id: None,
                pinned: false,
                marked: false,
            }],
            Some(9),
        );

        snapshot.sanitize();

        assert_eq!(snapshot.active_tab, Some(0));
    }

    #[test]
    fn restored_sessions_and_fields_are_bounded_before_spawn() {
        let session = SessionSnapshot {
            name: "雪".repeat(200),
            tags: (0..(MAX_SESSION_TAGS + 10))
                .map(|_| "x".repeat(MAX_SESSION_TAG_BYTES + 10))
                .collect(),
            cwd: Some("/".repeat(MAX_SESSION_CWD_BYTES + 10)),
            session_id: Some("../invalid".to_string()),
            custom_name: Some("n".repeat(MAX_SESSION_NAME_BYTES + 10)),
        };
        let mut snapshot = SessionsSnapshot::from_snapshots(
            vec![session; MAX_RESTORED_SESSIONS + 10],
            Some(usize::MAX),
            Vec::new(),
            None,
        );

        let warnings = snapshot.sanitize();

        assert_eq!(snapshot.sessions.len(), MAX_RESTORED_SESSIONS);
        assert_eq!(
            snapshot.active_index,
            Some(MAX_RESTORED_SESSIONS.saturating_sub(1))
        );
        assert!(warnings.len() >= 2);
        for session in snapshot.sessions {
            assert!(session.name.len() <= MAX_SESSION_NAME_BYTES);
            assert!(session.name.is_char_boundary(session.name.len()));
            assert_eq!(session.tags.len(), MAX_SESSION_TAGS);
            assert!(session
                .tags
                .iter()
                .all(|tag| tag.len() <= MAX_SESSION_TAG_BYTES));
            assert!(session.cwd.is_none());
            assert!(session.session_id.is_none());
            assert!(session
                .custom_name
                .as_ref()
                .is_some_and(|name| name.len() <= MAX_SESSION_NAME_BYTES));
        }
    }

    #[test]
    fn save_produces_a_snapshot_accepted_by_the_bounded_loader() {
        let root = TestDir::new("bounded-save-round-trip");
        let path = root.0.join("sessions.json");
        let huge_name = "x".repeat(MAX_SESSION_SNAPSHOT_BYTES as usize + 1024);
        let mut sessions = vec![SessionSnapshot {
            name: huge_name.clone(),
            tags: vec![],
            cwd: Some("/tmp".to_string()),
            session_id: Some("session-0".to_string()),
            custom_name: Some(huge_name),
        }];
        sessions.extend((1..MAX_RESTORED_SESSIONS + 8).map(|index| SessionSnapshot {
            name: format!("Session {index}"),
            tags: vec![],
            cwd: Some("/tmp".to_string()),
            session_id: Some(format!("session-{index}")),
            custom_name: None,
        }));
        let snapshot = SessionsSnapshot::from_snapshots(
            sessions,
            Some(usize::MAX),
            vec![LayoutSnapshot {
                root: LayoutNodeSnapshot::Split {
                    horizontal: false,
                    ratio: f32::NAN,
                    first: Box::new(LayoutNodeSnapshot::Pane {
                        session_id: "session-0".to_string(),
                    }),
                    second: Box::new(LayoutNodeSnapshot::Pane {
                        session_id: "session-0".to_string(),
                    }),
                },
                focused_session_id: Some("session-0".to_string()),
                pinned: false,
                marked: false,
            }],
            Some(0),
        );

        snapshot.save(&path).unwrap();

        assert!(std::fs::metadata(&path).unwrap().len() <= MAX_SESSION_SNAPSHOT_BYTES);
        let (loaded, warnings) = SessionsSnapshot::load_with_warnings(&path).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(loaded.sessions.len(), MAX_RESTORED_SESSIONS);
        assert_eq!(loaded.sessions[0].name.len(), MAX_SESSION_NAME_BYTES);
        assert_eq!(
            loaded.sessions[0].custom_name.as_ref().unwrap().len(),
            MAX_SESSION_NAME_BYTES
        );
        assert_eq!(
            loaded.active_index,
            Some(MAX_RESTORED_SESSIONS.saturating_sub(1))
        );
        assert!(matches!(
            loaded.layout.unwrap().root,
            LayoutNodeSnapshot::Pane { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_save_preserves_a_shared_parent_and_refuses_a_linked_parent() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = TestDir::new("configured-parent");
        let shared = root.0.join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        let snapshot = SessionsSnapshot::from_snapshots(Vec::new(), None, Vec::new(), None);

        snapshot.save(&shared.join("sessions.json")).unwrap();
        assert_eq!(
            std::fs::metadata(&shared).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let victim = root.0.join("victim");
        let linked_parent = root.0.join("linked-parent");
        std::fs::create_dir(&victim).unwrap();
        symlink(&victim, &linked_parent).unwrap();
        assert!(ensure_session_history_dir(&linked_parent.join("sessions.json")).is_err());
        assert!(snapshot.save(&linked_parent.join("sessions.json")).is_err());
        assert!(!victim.join("sessions.json").exists());
    }

    #[test]
    fn load_with_warnings_sanitizes_the_real_file_path() {
        let root = TestDir::new("bounded-load-integration");
        let path = root.0.join("sessions.json");
        let session = SessionSnapshot {
            name: "雪".repeat(200),
            tags: vec!["x".repeat(MAX_SESSION_TAG_BYTES + 1); MAX_SESSION_TAGS + 1],
            cwd: Some("/".repeat(MAX_SESSION_CWD_BYTES + 1)),
            session_id: Some("../invalid".to_string()),
            custom_name: Some("n".repeat(MAX_SESSION_NAME_BYTES + 1)),
        };
        let snapshot = SessionsSnapshot::from_snapshots(
            vec![session; MAX_RESTORED_SESSIONS + 1],
            Some(usize::MAX),
            Vec::new(),
            None,
        );
        write_private(&path, serde_json::to_vec(&snapshot).unwrap());

        let (loaded, warnings) = SessionsSnapshot::load_with_warnings(&path).unwrap();

        assert!(!warnings.is_empty());
        assert_eq!(loaded.sessions.len(), MAX_RESTORED_SESSIONS);
        assert_eq!(
            loaded.active_index,
            Some(MAX_RESTORED_SESSIONS.saturating_sub(1))
        );
        assert!(loaded.sessions.iter().all(|session| {
            session.name.len() <= MAX_SESSION_NAME_BYTES
                && session.tags.len() <= MAX_SESSION_TAGS
                && session.session_id.is_none()
        }));
    }

    #[test]
    fn bounded_session_names_keep_utf8_boundaries() {
        let bounded = bounded_session_name(&format!("  {}  ", "雪".repeat(200)));
        assert_eq!(bounded, "雪".repeat(85));
        assert_eq!(bounded.len(), 255);
        assert_eq!(bounded_session_name("  short name  "), "short name");
        assert_eq!(
            bounded_session_name("safe\n\u{202e}spoof\u{7f}"),
            "safespoof"
        );
    }

    #[test]
    fn future_snapshot_versions_are_rejected_before_restore() {
        let root = TestDir::new("future-version");
        let path = root.0.join("sessions.json");
        write_private(
            &path,
            br#"{"version":99,"sessions":[{"name":"future","tags":[]}]}"#,
        );

        let error = SessionsSnapshot::load(&path).unwrap_err().to_string();
        assert!(
            error.contains("unsupported session snapshot version 99"),
            "{error}"
        );
        assert!(path.exists());
    }

    #[test]
    fn oversized_snapshot_is_rejected_before_reading_it() {
        let root = TestDir::new("oversized-snapshot");
        let path = root.0.join("sessions.json");
        write_private(&path, vec![b' '; MAX_SESSION_SNAPSHOT_BYTES as usize + 1]);

        let error = SessionsSnapshot::load(&path).unwrap_err().to_string();
        assert!(error.contains("limit"), "{error}");
    }

    /// The seam, not the mechanism (core owns and tests the naming): what this
    /// pins is that ember's loader rejects a corrupt snapshot and that the
    /// bytes survive being moved aside, in that order — startup writes fresh
    /// state right after, and the evidence must already be out of its way.
    #[test]
    fn malformed_snapshot_is_quarantined_without_losing_its_bytes() {
        let root = TestDir::new("corrupt-snapshot");
        let path = root.0.join("sessions.json");
        let original = b"{ definitely not valid JSON";
        write_private(&path, original);

        assert!(SessionsSnapshot::load(&path).is_err());
        let backup = jterm_core::snapshot_file::quarantine_corrupt(&path).unwrap();

        assert!(!path.exists());
        assert_eq!(std::fs::read(backup).unwrap(), original);
    }

    /// A snapshot path that is not a regular file must be rejected instead of
    /// read: an attacker-planted fifo at a configured `session_history_file`
    /// used to hang the whole startup inside `open`.
    #[cfg(unix)]
    #[test]
    fn a_fifo_at_the_snapshot_path_is_refused_rather_than_opened() {
        let root = TestDir::new("fifo-snapshot");
        let path = root.0.join("sessions.json");
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `name` is a NUL-terminated path that outlives the call.
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
            return; // Some sandboxes forbid mkfifo; nothing to assert then.
        }

        let error = SessionsSnapshot::load(&path).unwrap_err().to_string();
        assert!(error.contains("not a regular file"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_hardlink_snapshots_are_not_restored() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("linked-snapshot");
        let target = root.0.join("target.json");
        let symlink_path = root.0.join("symlink.json");
        let hardlink_path = root.0.join("hardlink.json");
        let snapshot = SessionsSnapshot::from_snapshots(Vec::new(), None, Vec::new(), None);
        write_private(&target, serde_json::to_vec(&snapshot).unwrap());

        symlink(&target, &symlink_path).unwrap();
        assert!(SessionsSnapshot::load(&symlink_path).is_err());

        std::fs::hard_link(&target, &hardlink_path).unwrap();
        let error = SessionsSnapshot::load(&hardlink_path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("hard link"), "{error}");
    }

    #[test]
    fn contending_instance_does_not_truncate_owner_pid() {
        let root = TestDir::new("contending");
        let path = root.0.join("instance.lock");
        write_private(&path, "stale-and-long-owner-value");

        let owner = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("first caller should acquire the lock");
        let expected_pid = std::process::id().to_string();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected_pid);

        let contender = try_acquire_instance_lock_at(&path).unwrap();
        assert!(contender.is_none());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );

        drop(owner);
        let replacement = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("lock should be available after its owner drops");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );
        drop(replacement);
    }

    #[test]
    fn transient_inherited_lock_is_retried_until_exec_style_release() {
        let root = TestDir::new("transient-inheritance");
        let path = root.0.join("instance.lock");
        let owner = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("first caller should acquire the lock");
        let inherited = owner.try_clone().unwrap();
        drop(owner);
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            drop(inherited);
        });

        let replacement = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("a transient inherited descriptor should be retried");

        releaser.join().unwrap();
        drop(replacement);
    }

    #[cfg(unix)]
    #[test]
    fn forked_child_can_close_its_registered_instance_lock_copy() {
        let root = TestDir::new("fork-inheritance");
        let path = root.0.join("instance.lock");
        let file = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("first caller should acquire the lock");
        let owner = InstanceLock::register(file).unwrap();
        let inherited_fd = inherited_instance_lock_fd();
        assert!(inherited_fd >= 0);

        let mut ready_pipe = [-1; 2];
        // SAFETY: ready_pipe points to two writable integers.
        assert_eq!(unsafe { libc::pipe(ready_pipe.as_mut_ptr()) }, 0);
        // SAFETY: both branches below restrict their post-fork work to raw
        // async-signal-safe syscalls until the child exits.
        let child_pid = unsafe { libc::fork() };
        assert!(child_pid >= 0, "fork failed");
        if child_pid == 0 {
            // SAFETY: these descriptors are inherited from the parent and the
            // child never returns into Rust or runs destructors.
            unsafe {
                libc::close(ready_pipe[0]);
                libc::close(inherited_fd);
                let marker = [1u8];
                libc::write(
                    ready_pipe[1],
                    marker.as_ptr().cast::<libc::c_void>(),
                    marker.len(),
                );
                loop {
                    libc::pause();
                }
            }
        }
        let _child = ForkChild(child_pid);

        // SAFETY: the parent owns both pipe descriptors after a successful
        // fork and waits for the child's one-byte close acknowledgement.
        unsafe {
            libc::close(ready_pipe[1]);
            let mut marker = [0u8];
            let read = loop {
                let result = libc::read(
                    ready_pipe[0],
                    marker.as_mut_ptr().cast::<libc::c_void>(),
                    marker.len(),
                );
                if result < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                break result;
            };
            libc::close(ready_pipe[0]);
            assert_eq!(read, 1);
        }

        drop(owner);
        assert_eq!(
            inherited_instance_lock_fd(),
            NO_INSTANCE_LOCK_FD,
            "dropping the parent guard must clear the published descriptor"
        );
        let replacement = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("the live forked child must not retain the lock");
        drop(replacement);
    }

    #[cfg(unix)]
    #[test]
    fn instance_lock_symlink_never_changes_its_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = TestDir::new("symlink");
        let target = root.0.join("do-not-touch");
        let lock_path = root.0.join("instance.lock");
        write_private(&target, "sentinel contents");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&target, &lock_path).unwrap();

        assert!(try_acquire_instance_lock_at(&lock_path).is_err());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "sentinel contents"
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[cfg(unix)]
    #[test]
    fn instance_lock_hard_link_never_changes_its_target() {
        let root = TestDir::new("hard-link");
        let target = root.0.join("do-not-touch");
        let lock_path = root.0.join("instance.lock");
        write_private(&target, "sentinel contents");
        std::fs::hard_link(&target, &lock_path).unwrap();

        assert!(try_acquire_instance_lock_at(&lock_path).is_err());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "sentinel contents"
        );
    }

    #[cfg(unix)]
    #[test]
    fn instance_lock_fifo_is_rejected_without_blocking() {
        use std::ffi::CString;

        let root = TestDir::new("fifo");
        let lock_path = root.0.join("instance.lock");
        let encoded = CString::new(lock_path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: encoded is a live NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);

        let started = std::time::Instant::now();
        assert!(try_acquire_instance_lock_at(&lock_path).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
