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

/// Per-window persistence state. A failed load/save pauses later writes until
/// restart, retaining a recovery notice for the existing status UI.
pub(crate) struct UiHistoryPersistence {
    path: Option<std::path::PathBuf>,
    notice: Option<String>,
}

impl UiHistoryPersistence {
    pub(crate) fn restore(
        path: Result<std::path::PathBuf, Box<dyn std::error::Error>>,
    ) -> (HistorySnapshot, Self) {
        match path {
            Ok(path) => {
                let (snapshot, notice) = match HistorySnapshot::try_load(&path) {
                    Ok(snapshot) => (snapshot, None),
                    Err(error) => (
                        HistorySnapshot::default(),
                        Some(Self::recovery_notice(&path, &error)),
                    ),
                };
                (snapshot, Self { path: Some(path), notice })
            }
            Err(error) => (
                HistorySnapshot::default(),
                Self {
                    path: None,
                    notice: Some(crate::review_text::bound_toast_text(format!(
                        "UI history saving is paused. Check the config directory, then restart Ember. Cannot locate the history file: {error}"
                    ))),
                },
            ),
        }
    }

    fn recovery_notice(path: &std::path::Path, error: &dyn std::fmt::Display) -> String {
        crate::review_text::bound_toast_text(format!(
            "UI history saving is paused; the existing file is preserved. Repair or move {}, then restart Ember. {error}",
            path.display()
        ))
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.notice.is_some()
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Return a notice only for the first failure. Subsequent events do no I/O
    /// and leave both the recovery notice and the original file unchanged.
    pub(crate) fn save(&mut self, snapshot: &HistorySnapshot) -> Option<&str> {
        if self.is_paused() {
            return None;
        }
        let path = self.path.as_ref()?;
        match snapshot.save(path) {
            Ok(()) => None,
            Err(error) => {
                // An I/O error can occur after atomic rename but before the
                // directory durability sync. Do not promise that every kind
                // of save failure left the previous generation untouched.
                self.notice = Some(crate::review_text::bound_toast_text(format!(
                    "UI history saving is paused after a save error. Inspect and repair {}, then restart Ember. {error}",
                    path.display()
                )));
                self.notice()
            }
        }
    }
}

impl HistorySnapshot {
    fn decode_current(bytes: &[u8]) -> std::io::Result<Self> {
        let snapshot: Self = serde_json::from_slice(bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if snapshot.version != default_version() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unsupported UI history version {}", snapshot.version),
            ));
        }
        Ok(snapshot)
    }

    fn try_load(path: &std::path::Path) -> std::io::Result<Self> {
        match crate::persistence_file::read_bounded(path, MAX_HISTORY_SNAPSHOT_BYTES) {
            Ok(content) => Self::decode_current(content.as_bytes()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    /// Compatibility view for callers that do not own persistence state.
    /// The live application uses UiHistoryPersistence::restore so a failed
    /// load is visible and cannot cause repeated failed saves.
    #[cfg(test)]
    pub fn load(path: &std::path::Path) -> Self {
        Self::try_load(path).unwrap_or_default()
    }

    /// 原子写入(临时文件 + fsync + rename),失败返回 Err 由调用方决定如何提示。
    pub fn save(&self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        if self.version != default_version() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsupported UI history version {}", self.version),
            )
            .into());
        }
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
        // Startup falls back to empty UI state after a failed load. A later
        // search/palette edit must not overwrite the only recoverable bytes,
        // including a snapshot produced by a newer application version.
        let revision = crate::persistence_file::read_revision(path, MAX_HISTORY_SNAPSHOT_BYTES)?;
        if let Some(bytes) = revision.bytes() {
            Self::decode_current(bytes).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "refusing to overwrite unreadable UI history {}: {error}",
                        path.display()
                    ),
                )
            })?;
        }
        // Validate and replace the same generation under the directory lock;
        // a concurrent edit between the read and publication is a conflict.
        crate::persistence_file::write_atomic_if_unchanged(
            path,
            json.as_bytes(),
            &revision,
            MAX_HISTORY_SNAPSHOT_BYTES,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("ember-history-test-{label}-{}", std::process::id()));
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
    fn failed_or_future_load_cannot_be_overwritten_by_the_next_history_save() {
        let root = TestDir::new("preserve-rejected");
        for (name, bytes) in [
            ("malformed.json", "{\"version\":1,\"search_history\":["),
            (
                "future.json",
                "{\"version\":2,\"search_history\":[],\"future_state\":true}",
            ),
        ] {
            let path = root.0.join(name);
            std::fs::write(&path, bytes).unwrap();
            let _ = HistorySnapshot::load(&path);
            let error = HistorySnapshot::default().save(&path);
            assert!(error.is_err(), "must refuse to replace {name}");
            assert_eq!(std::fs::read(&path).unwrap(), bytes.as_bytes());
        }
    }

    #[test]
    fn future_history_version_is_not_loaded_as_current_state() {
        let root = TestDir::new("future-version");
        let path = root.0.join("ui_history.json");
        std::fs::write(&path, br#"{"version":2,"search_history":[{"query":"future","is_regex":false,"case_sensitive":false,"timestamp":"later"}]}"#).unwrap();
        let loaded = HistorySnapshot::load(&path);
        assert_eq!(loaded.version, 1);
        assert!(loaded.search_history.is_empty());
    }

    #[test]
    fn rejected_history_starts_paused_with_a_persistent_recovery_notice() {
        let root = TestDir::new("startup-paused");
        for (name, original) in [
            ("malformed.json", "{not JSON"),
            ("future.json", "{\"version\":2}"),
        ] {
            let path = root.0.join(name);
            std::fs::write(&path, original).unwrap();
            let (snapshot, mut persistence) = UiHistoryPersistence::restore(Ok(path.clone()));
            assert!(snapshot.search_history.is_empty());
            assert!(persistence.is_paused());
            let notice = persistence.notice().unwrap().to_owned();
            assert!(notice.contains("existing file is preserved"));
            assert!(notice.contains("Repair or move"));
            assert!(notice.contains("restart Ember"));
            assert!(notice.contains(name));
            for _ in 0..3 {
                assert!(persistence.save(&HistorySnapshot::default()).is_none());
            }
            assert_eq!(persistence.notice(), Some(notice.as_str()));
            assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
        }
    }

    #[test]
    fn first_runtime_failure_pauses_writes_and_reports_only_once_until_restart() {
        let root = TestDir::new("runtime-paused");
        let path = root.0.join("ui_history.json");
        let (snapshot, mut persistence) = UiHistoryPersistence::restore(Ok(path.clone()));
        assert!(!persistence.is_paused());
        std::fs::write(&path, "broken input").unwrap();
        assert!(persistence.save(&snapshot).is_some());
        assert!(persistence.is_paused());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken input");
        // Simulate the owner repairing the file. Further events in the old
        // window must still perform no writes; a restart is the recovery step.
        std::fs::write(&path, "{}").unwrap();
        for _ in 0..3 {
            assert!(persistence.save(&snapshot).is_none());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        }
        assert!(persistence.notice().is_some());
        let (snapshot, mut restarted) = UiHistoryPersistence::restore(Ok(path.clone()));
        assert!(!restarted.is_paused());
        assert!(restarted.save(&snapshot).is_none());
        assert_ne!(std::fs::read_to_string(&path).unwrap(), "{}");
        assert!(restarted.notice().is_none());
    }

    #[test]
    fn missing_history_and_path_lookup_failures_have_distinct_persistence_states() {
        let root = TestDir::new("lookup-state");
        let path = root.0.join("new.json");
        let (snapshot, mut ready) = UiHistoryPersistence::restore(Ok(path.clone()));
        assert!(!ready.is_paused());
        assert!(ready.save(&snapshot).is_none());
        assert!(path.exists());
        let (_, mut paused) = UiHistoryPersistence::restore(Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory",
        )
        .into()));
        assert!(paused.is_paused());
        assert!(paused
            .notice()
            .unwrap()
            .contains("Check the config directory"));
        assert!(paused.save(&snapshot).is_none());
    }

    #[test]
    fn startup_and_mutation_paths_surface_and_pause_ui_history_errors() {
        let startup = include_str!("main.rs");
        let window = include_str!("app/window.rs")
            .split_whitespace()
            .collect::<String>();
        let state = include_str!("app/state.rs");
        assert!(
            startup.contains("UiHistoryPersistence::restore(config::Config::ui_history_path())"),
            "startup must retain a failed history load as a paused recovery state"
        );
        assert!(
            window.contains("ifself.ui_history_persistence.is_paused()"),
            "later palette/search events must stop retrying failed persistence"
        );
        assert!(
            window.contains("self.ui_history_persistence.save(&snapshot)"),
            "saves must update the same per-window recovery state"
        );
        assert!(
            state.contains("self.ui_history_persistence.notice()"),
            "recovery notice must remain available after ordinary toasts expire"
        );
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
