//! 命令面板/搜索历史的轻量持久化。
//!
//! 与 session_persistence 分文件存放(`ui_history.json`),避免每次保存
//! 都重写完整的会话快照。结构内置版本号,后续扩展字段(例如 paste 历史)
//! 时可通过 `#[serde(default)]` 平滑兼容。

use crate::keybindings::Command;
use crate::search::SearchHistoryEntry;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySnapshot {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub recent_commands: Vec<Command>,
    #[serde(default)]
    pub search_history: Vec<SearchHistoryEntry>,
}

/// Upper bound for `ui_history.json`. Recent commands are palette entries and
/// search history is a handful of user queries, so real files are kilobytes;
/// this only exists so a runaway or hostile file cannot be read into memory in
/// full before anything gets to reject it. Same contract as the session
/// snapshot, two orders of magnitude of headroom.
const MAX_HISTORY_SNAPSHOT_BYTES: u64 = 4 * 1024 * 1024;

fn default_version() -> u32 {
    1
}

impl Default for HistorySnapshot {
    fn default() -> Self {
        Self {
            version: 1,
            recent_commands: Vec::new(),
            search_history: Vec::new(),
        }
    }
}

impl HistorySnapshot {
    /// 加载持久化历史;文件不存在或解析失败时返回 Default。
    /// 解析失败只打印日志,不让旧/坏数据阻止应用启动。
    pub fn load(path: &std::path::Path) -> Self {
        match crate::persistence_file::read_bounded(path, MAX_HISTORY_SNAPSHOT_BYTES) {
            Ok(content) => match serde_json::from_str::<Self>(&content) {
                Ok(snap) => snap,
                Err(e) => {
                    eprintln!(
                        "[HistoryPersistence] Failed to parse {}: {} (using defaults)",
                        path.display(),
                        e
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                eprintln!(
                    "[HistoryPersistence] Failed to read {}: {} (using defaults)",
                    path.display(),
                    e
                );
                Self::default()
            }
        }
    }

    /// 原子写入(临时文件 + fsync + rename),失败返回 Err 由调用方决定如何提示。
    pub fn save(&self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_string_pretty(self)?;
        if json.len() as u64 > MAX_HISTORY_SNAPSHOT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                format!(
                    "serialized UI history is {} bytes; limit is {MAX_HISTORY_SNAPSHOT_BYTES}",
                    json.len()
                ),
            )
            .into());
        }
        crate::persistence_file::write_atomic(path, json.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "jterm2-history-test-{label}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
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

    /// A history file over the bound must be rejected by the *reader*, not left
    /// to fail as a parse error.
    ///
    /// The payload is therefore perfectly valid JSON: garbage over the limit
    /// would fall back to defaults either way, so it would prove nothing about
    /// the bound. Over the limit the snapshot must not load; the same shape just
    /// under it must.
    #[test]
    fn oversized_history_is_rejected_on_read_and_write() {
        let root = TestDir::new("oversized");
        let entry = |query: String| crate::search::SearchHistoryEntry {
            query,
            is_regex: false,
            case_sensitive: false,
            timestamp: "1970-01-01".to_string(),
        };
        let snapshot = |query_len: usize| HistorySnapshot {
            version: 1,
            recent_commands: Vec::new(),
            search_history: vec![entry("x".repeat(query_len))],
        };

        let write = |path: &std::path::Path, query_len: usize| {
            snapshot(query_len).save(path).unwrap();
            std::fs::metadata(path).unwrap().len()
        };

        let over = root.0.join("over.json");
        std::fs::write(&over, b"last-good").unwrap();
        assert!(snapshot(MAX_HISTORY_SNAPSHOT_BYTES as usize + 1)
            .save(&over)
            .is_err());
        assert_eq!(std::fs::read(&over).unwrap(), b"last-good");

        // A hostile externally-created valid document over the limit is also
        // rejected by the reader.
        std::fs::write(
            &over,
            serde_json::to_vec(&snapshot(MAX_HISTORY_SNAPSHOT_BYTES as usize + 1)).unwrap(),
        )
        .unwrap();
        let loaded = HistorySnapshot::load(&over);
        assert!(loaded.recent_commands.is_empty());
        assert!(
            loaded.search_history.is_empty(),
            "valid JSON over the bound must still be refused"
        );

        let under = root.0.join("under.json");
        let written = write(&under, 1024);
        assert!(written <= MAX_HISTORY_SNAPSHOT_BYTES, "{written}");
        assert_eq!(HistorySnapshot::load(&under).search_history.len(), 1);
    }

    #[test]
    fn a_saved_history_file_round_trips_through_the_bounded_loader() {
        let root = TestDir::new("round-trip");
        let path = root.0.join("ui_history.json");
        let snapshot = HistorySnapshot {
            version: 1,
            recent_commands: Vec::new(),
            search_history: vec![crate::search::SearchHistoryEntry {
                query: "needle".to_string(),
                is_regex: false,
                case_sensitive: false,
                timestamp: "1970-01-01".to_string(),
            }],
        };
        snapshot.save(&path).unwrap();

        let loaded = HistorySnapshot::load(&path);
        assert_eq!(loaded.search_history.len(), 1);
        assert_eq!(loaded.search_history[0].query, "needle");
    }

    #[cfg(unix)]
    #[test]
    fn history_loader_does_not_follow_a_valid_snapshot_symlink() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("symlink");
        let target = root.0.join("target.json");
        let link = root.0.join("ui_history.json");
        let snapshot = HistorySnapshot {
            version: 1,
            recent_commands: Vec::new(),
            search_history: vec![crate::search::SearchHistoryEntry {
                query: "must-not-load".to_string(),
                is_regex: false,
                case_sensitive: false,
                timestamp: "1970-01-01".to_string(),
            }],
        };
        std::fs::write(&target, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        symlink(&target, &link).unwrap();

        let loaded = HistorySnapshot::load(&link);
        assert!(loaded.search_history.is_empty());
        assert_eq!(
            std::fs::read(&target).unwrap(),
            serde_json::to_vec(&snapshot).unwrap()
        );
    }
}
