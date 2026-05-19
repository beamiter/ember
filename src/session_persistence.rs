use serde::{Deserialize, Serialize};
use std::path::Path;

/// 会话持久化数据结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub name: String,
    pub tags: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub restorable_commands: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
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

    /// 保存到文件（原子写入）
    pub fn save(&self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_string_pretty(self)?;
        let tmp_path = path.with_file_name(
            path.file_name()
                .and_then(|s| s.to_str())
                .map(|name| format!("{}.tmp", name))
                .unwrap_or_else(|| "session_history.json.tmp".to_string()),
        );
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, path).or_else(|_| {
            let _ = std::fs::remove_file(path);
            std::fs::rename(&tmp_path, path)
        })?;
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

// ============================================================
// Process tree introspection for detecting restorable commands
// ============================================================

/// Read /proc/<pid>/cmdline and return the argv as a Vec<String>.
pub fn read_proc_cmdline(pid: i32) -> Option<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let args: Vec<String> = bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).to_string())
        .collect();
    if args.is_empty() {
        None
    } else {
        Some(args)
    }
}

/// Read the parent PID from /proc/<pid>/stat.
pub fn read_ppid(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Format: "<pid> (<comm>) <state> <ppid> ..."
    // comm may contain spaces/parens, so find the last ')' first.
    let after_comm = stat.rsplit_once(')')?.1;
    let mut fields = after_comm.split_whitespace();
    fields.next(); // state
    fields.next()?.parse::<i32>().ok()
}

/// Check if an argv matches a known restorable command pattern.
/// Returns the command string to replay, or None.
pub fn match_restorable_command(args: &[String]) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    let bin = Path::new(&args[0])
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    match bin.as_str() {
        "nix" => {
            // e.g. nix develop, nix develop /path/to/flake
            if args.len() >= 2 && args[1] == "develop" {
                Some(args.join(" "))
            } else {
                None
            }
        }
        "bash" | "zsh" | "fish" => {
            // nix develop execs into: bash --rcfile /tmp/nix-shell.XXXXX
            // Detect this pattern and restore as "nix develop" using the CWD's flake.
            for arg in &args[1..] {
                if arg.starts_with("/tmp/nix-shell.") || arg.starts_with("/tmp/nix-shell-") {
                    return Some("nix develop".to_string());
                }
            }
            None
        }
        "ssh" | "mosh" => Some(args.join(" ")),
        "docker" | "podman" => {
            if args.len() >= 2
                && (args[1] == "exec"
                    || (args[1] == "compose" && args.len() >= 3 && args[2] == "exec"))
            {
                Some(args.join(" "))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Detect restorable interactive commands running in a terminal by inspecting the
/// foreground process group and walking up the process tree to the shell.
pub fn get_restorable_commands(shell_pid: i32, master_fd: i32) -> Option<String> {
    // SAFETY: tcgetpgrp 获取终端的前台进程组 ID。master_fd 是有效的 PTY master fd。
    // 即使 fd 无效，tcgetpgrp 也只会返回 -1，不会导致未定义行为。
    let fg_pgid = unsafe { libc::tcgetpgrp(master_fd) };
    if fg_pgid <= 0 || fg_pgid == shell_pid {
        return None; // shell itself is foreground — nothing to restore
    }

    // Walk from the foreground process up to the shell, checking each level.
    let mut pid = fg_pgid;
    let mut visited = 0;
    while pid != shell_pid && pid > 1 && visited < 16 {
        if let Some(args) = read_proc_cmdline(pid) {
            if let Some(cmd) = match_restorable_command(&args) {
                return Some(cmd);
            }
        }
        pid = match read_ppid(pid) {
            Some(ppid) => ppid,
            None => break,
        };
        visited += 1;
    }
    None
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
                restorable_commands: Some("nix develop".to_string()),
                session_id: Some("123-456".to_string()),
            },
            SessionSnapshot {
                name: "Session 2".to_string(),
                tags: vec!["test".to_string()],
                cwd: Some("/tmp".to_string()),
                restorable_commands: None,
                session_id: None,
            },
        ];

        let snapshot = SessionsSnapshot::from_snapshots(snapshots, Some(1));
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.sessions[0].cwd, Some("/home/user".to_string()));
        assert_eq!(
            snapshot.sessions[0].restorable_commands,
            Some("nix develop".to_string())
        );
        assert_eq!(snapshot.sessions[1].cwd, Some("/tmp".to_string()));
        assert_eq!(snapshot.active_index, Some(1));
    }

    #[test]
    fn test_match_restorable_nix_develop() {
        let args = vec!["nix".to_string(), "develop".to_string()];
        assert_eq!(
            match_restorable_command(&args),
            Some("nix develop".to_string())
        );

        let args = vec![
            "nix".to_string(),
            "develop".to_string(),
            "/path/to/flake".to_string(),
        ];
        assert_eq!(
            match_restorable_command(&args),
            Some("nix develop /path/to/flake".to_string())
        );

        let args = vec!["nix".to_string(), "build".to_string()];
        assert_eq!(match_restorable_command(&args), None);
    }

    #[test]
    fn test_match_restorable_nix_shell_pattern() {
        let args = vec![
            "bash".to_string(),
            "--rcfile".to_string(),
            "/tmp/nix-shell.abcdef".to_string(),
        ];
        assert_eq!(
            match_restorable_command(&args),
            Some("nix develop".to_string())
        );
    }

    #[test]
    fn test_match_restorable_ssh() {
        let args = vec!["ssh".to_string(), "user@host".to_string()];
        assert_eq!(
            match_restorable_command(&args),
            Some("ssh user@host".to_string())
        );

        let args = vec!["mosh".to_string(), "user@host".to_string()];
        assert_eq!(
            match_restorable_command(&args),
            Some("mosh user@host".to_string())
        );
    }

    #[test]
    fn test_match_restorable_docker() {
        let args = vec![
            "docker".to_string(),
            "exec".to_string(),
            "-it".to_string(),
            "container".to_string(),
            "bash".to_string(),
        ];
        assert_eq!(
            match_restorable_command(&args),
            Some("docker exec -it container bash".to_string())
        );

        let args = vec!["docker".to_string(), "run".to_string()];
        assert_eq!(match_restorable_command(&args), None);
    }

    #[test]
    fn test_match_restorable_empty() {
        let args: Vec<String> = vec![];
        assert_eq!(match_restorable_command(&args), None);
    }

    #[test]
    fn test_backward_compat_deserialization() {
        // Old format without new fields
        let json =
            r#"{"version":1,"sessions":[{"name":"Session 1","tags":[],"cwd":"/home/user"}]}"#;
        let snapshot: SessionsSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snapshot.sessions[0].restorable_commands, None);
        assert_eq!(snapshot.sessions[0].session_id, None);
        assert_eq!(snapshot.active_index, None);
    }
}
