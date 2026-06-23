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
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(path) {
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
        use std::io::Write;
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let json = serde_json::to_string_pretty(self)?;
        let tmp_path = path.with_file_name(
            path.file_name()
                .and_then(|s| s.to_str())
                .map(|name| format!("{}.tmp", name))
                .unwrap_or_else(|| "ui_history.json.tmp".to_string()),
        );
        {
            let mut f = std::fs::File::create(&tmp_path)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, path).or_else(|_| {
            let _ = std::fs::remove_file(path);
            std::fs::rename(&tmp_path, path)
        })?;
        Ok(())
    }
}
