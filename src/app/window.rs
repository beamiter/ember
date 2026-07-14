// Window and session management module

use super::state::TerminalApp;
use crate::config;
use crate::history_persistence;
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
            } else {
                // Record our own write immediately so the hot-reload watcher
                // does not treat it as an external edit in the same frame.
                self.config_last_mtime = config::Config::config_mtime();
            }
        }
    }

    pub fn schedule_session_save(&mut self) {
        self.session_save_pending = true;
        self.session_save_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    }

    /// 即时持久化命令面板最近命令 + 搜索历史。两者都很小,无需 debounce。
    /// 写盘失败只记日志,不影响交互(下次启动顶多丢一次新增项)。
    pub fn save_ui_history(&self) {
        if let Ok(path) = config::Config::ui_history_path() {
            let snapshot = history_persistence::HistorySnapshot {
                version: 1,
                recent_commands: self.command_palette.recent_commands_snapshot(),
                search_history: self.search_state.history.iter().cloned().collect(),
            };
            if let Err(e) = snapshot.save(&path) {
                eprintln!("[HistoryPersistence] Failed to save: {}", e);
            }
        }
    }

    pub fn flush_session_save(&mut self) {
        if self.session_save_pending && std::time::Instant::now() >= self.session_save_deadline {
            self.session_save_pending = false;
            // Only the process holding the instance lock owns the shared
            // snapshot. A secondary window must not overwrite the primary
            // instance's complete session list when it exits or changes tabs.
            if self._lock_file.is_none() {
                return;
            }
            if let Ok(path) = self.config.resolved_session_history_path() {
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

    pub fn check_config_hot_reload(&mut self, ctx: &eframe::egui::Context) {
        let now = std::time::Instant::now();
        if now.duration_since(self.config_last_check) < std::time::Duration::from_secs(2) {
            return;
        }
        self.config_last_check = now;

        // Don't reload if we just saved (avoid feedback loop)
        if self.config_save_pending {
            return;
        }

        let current_mtime = config::Config::config_mtime();
        if current_mtime == self.config_last_mtime {
            return;
        }
        self.config_last_mtime = current_mtime;

        if let Ok(config_path) = config::Config::config_path() {
            if let Ok(content) = std::fs::read_to_string(&config_path) {
                match toml::from_str::<config::Config>(&content) {
                    Ok(mut new_config) => {
                        let mut notes = new_config.normalize();
                        notes.extend(self.apply_hot_reload(new_config, ctx));
                        eprintln!("[Config] Hot-reloaded from {}", config_path.display());
                        if notes.is_empty() {
                            self.set_status("配置已热重载");
                        } else {
                            for note in &notes {
                                eprintln!("[Config] WARNING: {}", note);
                            }
                            self.set_status_for(
                                format!("配置已重载（{} 项已调整）", notes.len()),
                                std::time::Duration::from_secs(5),
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!("[Config] Hot-reload parse error: {}", e);
                        // 同时在状态栏提示用户,避免改坏配置后毫无反馈、误以为已生效。
                        self.status_message = format!("配置解析失败,已沿用旧配置: {}", e);
                        self.status_expires_at =
                            Some(std::time::Instant::now() + std::time::Duration::from_secs(6));
                    }
                }
            }
        }
    }

    fn apply_hot_reload(
        &mut self,
        mut new: config::Config,
        ctx: &eframe::egui::Context,
    ) -> Vec<String> {
        let mut notes = Vec::new();
        let font_changed = new.font_family != self.config.font_family
            || new.font_backend != self.config.font_backend
            || (new.font_size - self.config.font_size).abs() > 0.01
            || (new.font_weight - self.config.font_weight).abs() > 0.01
            || (new.font_sharpness - self.config.font_sharpness).abs() > 0.01
            || (new.line_spacing - self.config.line_spacing).abs() > 0.01
            || new.subpixel_rendering != self.config.subpixel_rendering
            || new.font_ligatures != self.config.font_ligatures;

        if new.theme != self.config.theme {
            if let Some(theme) = crate::theme::Theme::get_theme(&new.theme) {
                self.current_theme = theme;
            } else {
                notes.push(format!(
                    "theme '{}' was not found; keeping '{}'",
                    new.theme, self.config.theme
                ));
                new.theme = self.config.theme.clone();
            }
        }

        if font_changed {
            self.renderer.invalidate_font_cache();
            for pr in &mut self.pane_renderers {
                pr.invalidate_font_cache();
            }
        }

        let configured_shell = std::env::var("JTERM2_SHELL").ok().or(new.shell.clone());
        self.session_manager.set_configured_shell(configured_shell);
        self.config = new;
        self.config_panel.sync_from_config(&self.config);
        self.apply_runtime_config(ctx);
        notes
    }
}
