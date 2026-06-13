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

    pub fn check_config_hot_reload(&mut self) {
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
                    Ok(new_config) => {
                        self.apply_hot_reload(&new_config);
                        eprintln!("[Config] Hot-reloaded from {}", config_path.display());
                    }
                    Err(e) => {
                        eprintln!("[Config] Hot-reload parse error: {}", e);
                    }
                }
            }
        }
    }

    fn apply_hot_reload(&mut self, new: &config::Config) {
        // 热重载来自磁盘上用户手改的文件,先 clamp 到合法范围,避免非法值
        // (负 padding、>1 不透明度、0 字号等)破坏渲染。
        let mut new = new.clone();
        new.font_size = config::Config::clamp_font_size(new.font_size);
        new.opacity = new.opacity.clamp(0.0, 1.0);
        new.padding = new.padding.clamp(0.0, 100.0);
        new.line_spacing = new.line_spacing.clamp(0.5, 3.0);
        let new = &new;
        let old = &self.config;

        let font_size_changed = (new.font_size - old.font_size).abs() > 0.01;
        let theme_changed = new.theme != old.theme;
        let opacity_changed = (new.opacity - old.opacity).abs() > 0.001;
        let padding_changed = (new.padding - old.padding).abs() > 0.01;
        let line_spacing_changed = (new.line_spacing - old.line_spacing).abs() > 0.01;
        let scrollback_changed = new.scrollback_lines != old.scrollback_lines;
        let scroll_speed_changed = new.scroll_speed != old.scroll_speed;

        if font_size_changed {
            self.config.font_size = new.font_size;
            self.renderer.invalidate_font_cache();
            for pr in &mut self.pane_renderers {
                pr.invalidate_font_cache();
            }
        }
        if theme_changed {
            self.config.theme = new.theme.clone();
            if let Some(theme) = crate::theme::Theme::get_theme(&new.theme) {
                self.current_theme = theme;
            }
        }
        if opacity_changed {
            self.config.opacity = new.opacity;
        }
        if padding_changed {
            self.config.padding = new.padding;
        }
        if line_spacing_changed {
            self.config.line_spacing = new.line_spacing;
            self.renderer.invalidate_font_cache();
            for pr in &mut self.pane_renderers {
                pr.invalidate_font_cache();
            }
        }
        if scrollback_changed {
            self.config.scrollback_lines = new.scrollback_lines;
        }
        if scroll_speed_changed {
            self.config.scroll_speed = new.scroll_speed;
        }
    }
}
