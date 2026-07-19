use serde::{Deserialize, Serialize};

fn default_split_ratio() -> f32 {
    0.5
}

/// 持久化布局只引用稳定 session ID，不保存运行期的 session 数组索引。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutSnapshot {
    pub root: LayoutNodeSnapshot,
    #[serde(default)]
    pub focused_session_id: Option<String>,
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
    #[serde(default, deserialize_with = "deserialize_optional_layout")]
    pub layout: Option<LayoutSnapshot>,
}

impl SessionsSnapshot {
    /// 从会话快照列表创建
    pub fn from_snapshots(
        sessions: Vec<SessionSnapshot>,
        active_index: Option<usize>,
        layout: Option<LayoutSnapshot>,
    ) -> Self {
        SessionsSnapshot {
            version: 3,
            sessions,
            active_index,
            layout,
        }
    }

    /// 保存到文件（原子写入 + fsync 持久化）
    pub fn save(&self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_string_pretty(self)?;
        crate::atomic_file::write_atomic(path, json.as_bytes())?;
        eprintln!("[SessionPersistence] Sessions saved to {}", path.display());
        Ok(())
    }

    /// 从文件加载
    pub fn load(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            return Ok(SessionsSnapshot {
                version: 3,
                sessions: vec![],
                active_index: None,
                layout: None,
            });
        }

        let content = std::fs::read_to_string(path)?;
        let snapshot: SessionsSnapshot = serde_json::from_str(&content)?;
        eprintln!(
            "[SessionPersistence] Sessions loaded from {}",
            path.display()
        );
        Ok(snapshot)
    }
}

fn try_acquire_instance_lock_at(
    lock_path: &std::path::Path,
) -> std::io::Result<Option<std::fs::File>> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Do not truncate before flock: a losing second instance must leave the
    // lock owner's diagnostic PID intact.
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(lock_path)?;

    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // LOCK_EX | LOCK_NB: 非阻塞排他锁
    // SAFETY: flock 对有效的文件描述符是安全的。fd 来自有效的 File 对象，
    // 标志是合法的 flock 常量。File 对象的生命周期确保 fd 在调用期间有效。
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        return Err(error);
    }

    // Only the lock owner may replace the diagnostic PID.
    use std::io::{Seek, Write};
    file.set_len(0)?;
    file.rewind()?;
    write!(file, "{}", std::process::id())?;
    file.sync_all()?;
    Ok(Some(file))
}

/// 尝试获取实例锁文件。成功返回 Some(File)（持有锁），失败表示已有实例在运行。
pub fn try_acquire_instance_lock() -> Option<std::fs::File> {
    let lock_path = dirs::config_dir()?.join("jterm2").join("instance.lock");
    match try_acquire_instance_lock_at(&lock_path) {
        Ok(lock) => lock,
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
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_LOCK_TEST: AtomicU64 = AtomicU64::new(0);

    fn lock_test_path() -> std::path::PathBuf {
        let id = NEXT_LOCK_TEST.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "jterm2-instance-lock-test-{}-{id}.lock",
            std::process::id()
        ))
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
        };
        let snapshot = SessionsSnapshot::from_snapshots(snapshots, Some(1), Some(layout.clone()));
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.sessions[0].cwd, Some("/home/user".to_string()));
        assert_eq!(snapshot.sessions[1].cwd, Some("/tmp".to_string()));
        assert_eq!(snapshot.active_index, Some(1));
        assert_eq!(snapshot.version, 3);
        assert_eq!(snapshot.layout, Some(layout.clone()));

        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: SessionsSnapshot = serde_json::from_str(&encoded).unwrap();
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

    #[test]
    fn contending_instance_does_not_truncate_owner_pid() {
        let path = lock_test_path();
        std::fs::write(&path, "stale-and-long-owner-value").unwrap();

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
        std::fs::remove_file(path).unwrap();
    }
}
