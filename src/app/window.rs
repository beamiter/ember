// Window and session management module

use super::state::TerminalApp;
use crate::config;
use crate::session_persistence;

impl TerminalApp {
    // 配置保存相关方法
    pub fn schedule_config_save(&mut self) {
        self.config_save_pending = true;
        self.config_save_deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(500);
    }

    pub fn flush_config_save(&mut self) {
        if self.config_save_pending && std::time::Instant::now() >= self.config_save_deadline {
            self.config_save_pending = false;
            if let Err(e) = self.config.save() {
                eprintln!("[Config] Failed to save: {}", e);
            }
        }
    }

    pub fn schedule_session_save(&mut self) {
        self.session_save_pending = true;
        self.session_save_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    }

    pub fn flush_session_save(&mut self) {
        if self.session_save_pending && std::time::Instant::now() >= self.session_save_deadline {
            self.session_save_pending = false;
            if let Ok(path) = config::Config::session_history_path() {
                let _ = session_persistence::ensure_session_history_dir(&path);
                let snapshots = self.session_manager.get_session_snapshots();
                let active_index = Some(self.session_manager.active_index());
                let snapshot =
                    session_persistence::SessionsSnapshot::from_snapshots(snapshots, active_index);
                if let Err(e) = snapshot.save(&path) {
                    eprintln!("[SessionPersistence] Failed to save: {}", e);
                }
            }
        }
    }
}
