use serde::{Deserialize, Serialize};

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
}

impl SessionsSnapshot {
    /// 从会话快照列表创建
    pub fn from_snapshots(sessions: Vec<SessionSnapshot>, active_index: Option<usize>) -> Self {
        SessionsSnapshot {
            version: 2,
            sessions,
            active_index,
        }
    }

    /// 保存到文件（原子写入 + fsync 持久化）
    pub fn save(&self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        use std::io::Write;
        let json = serde_json::to_string_pretty(self)?;
        let tmp_path = path.with_file_name(
            path.file_name()
                .and_then(|s| s.to_str())
                .map(|name| format!("{}.tmp", name))
                .unwrap_or_else(|| "session_history.json.tmp".to_string()),
        );
        // 写入临时文件并 fsync:rename 只保证元数据原子性,若数据块未落盘,
        // 崩溃/掉电后可能得到一个空或被截断的文件。必须先 sync_all 再 rename。
        {
            let mut f = std::fs::File::create(&tmp_path)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, path).or_else(|_| {
            let _ = std::fs::remove_file(path);
            std::fs::rename(&tmp_path, path)
        })?;
        // fsync 父目录,确保 rename 这条目录项本身也持久化。
        if let Some(parent) = path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        eprintln!("[SessionPersistence] Sessions saved to {}", path.display());
        Ok(())
    }

    /// 从文件加载
    pub fn load(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            return Ok(SessionsSnapshot {
                version: 2,
                sessions: vec![],
                active_index: None,
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

/// 尝试获取实例锁文件。成功返回 Some(File)（持有锁），失败表示已有实例在运行。
pub fn try_acquire_instance_lock() -> Option<std::fs::File> {
    let lock_path = dirs::config_dir()?.join("jterm2").join("instance.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // 尝试以排他锁方式打开文件
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&lock_path)
        .ok()?;

    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // LOCK_EX | LOCK_NB: 非阻塞排他锁
    // SAFETY: flock 对有效的文件描述符是安全的。fd 来自有效的 File 对象，
    // 标志是合法的 flock 常量。File 对象的生命周期确保 fd 在调用期间有效。
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        // 写入 PID 方便调试
        use std::io::Write;
        let mut f = &file;
        let _ = write!(f, "{}", std::process::id());
        Some(file)
    } else {
        None // 已有实例持有锁
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

        let snapshot = SessionsSnapshot::from_snapshots(snapshots, Some(1));
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.sessions[0].cwd, Some("/home/user".to_string()));
        assert_eq!(snapshot.sessions[1].cwd, Some("/tmp".to_string()));
        assert_eq!(snapshot.active_index, Some(1));
    }

    #[test]
    fn test_backward_compat_deserialization() {
        let json =
            r#"{"version":1,"sessions":[{"name":"Session 1","tags":[],"cwd":"/home/user"}]}"#;
        let snapshot: SessionsSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snapshot.sessions[0].session_id, None);
        assert_eq!(snapshot.active_index, None);
    }
}
