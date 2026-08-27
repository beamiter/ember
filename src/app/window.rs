// Window and session management module

use super::state::TerminalApp;
use crate::config;
use crate::history_persistence;
use crate::session_persistence;

const MAX_WINDOW_TITLE_CHARS: usize = 200;

/// Window titles originate in untrusted OSC output. Keep them single-line,
/// bounded, and free of bidi override/isolate controls that could make a
/// desktop task switcher display a deceptive title. An empty OSC title must
/// also restore the application fallback instead of leaving a stale title from
/// the previously focused session.
pub(crate) fn safe_window_title(reported: &str, fallback: &str) -> String {
    let source = if reported.trim().is_empty() {
        fallback
    } else {
        reported
    };
    let mut title = String::with_capacity(source.len().min(MAX_WINDOW_TITLE_CHARS));
    let mut pending_space = false;
    let mut truncated = false;
    let mut chars = 0;

    for ch in source.chars() {
        let bidi_control = matches!(
            ch,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        );
        if bidi_control || (ch.is_control() && !ch.is_whitespace()) {
            continue;
        }
        if ch.is_whitespace() {
            pending_space = !title.is_empty();
            continue;
        }
        if pending_space {
            if chars >= MAX_WINDOW_TITLE_CHARS {
                truncated = true;
                break;
            }
            title.push(' ');
            chars += 1;
            pending_space = false;
        }
        if chars >= MAX_WINDOW_TITLE_CHARS {
            truncated = true;
            break;
        }
        title.push(ch);
        chars += 1;
    }

    if title.is_empty() {
        title.push_str("Ember");
    } else if truncated {
        title.push('…');
    }
    title
}

/// 组装会话快照用于持久化。任务终端（Agent / 校验）等非常驻会话只存在于
/// 运行时：它们的任务元数据不进快照，重启后只会变成一个恰好落在任务
/// worktree 里的普通 shell，极易误操作——所以保存端把它们整个排除，而
/// 不是恢复原样。
fn sessions_snapshot_for_persistence(
    session_manager: &crate::session_manager::SessionManager,
    tabs: &crate::tab_manager::TabManager,
) -> session_persistence::SessionsSnapshot {
    let snapshots = session_manager.get_session_snapshots();
    // 布局树叶子存的是会话向量的全局下标，解析时必须用未过滤的全局 ID
    // 列表：用过滤后的列表会在中间夹着任务终端时把窗格映射到错误的会话
    // （或让整页转换失败被静默丢弃）。
    let session_ids: Vec<String> = session_manager
        .sessions()
        .iter()
        .map(|session| session.metadata.session_id.clone())
        .collect();
    // 每个 tab 存一棵树。转换失败的 tab（其会话缺少稳定 ID）整个跳过，
    // 恢复时它的会话会各自落到单窗格 tab 上，而不是让整份布局作废。
    let original_active_tab = tabs.active_index();
    let mut active_tab = None;
    let mut kept_tabs = Vec::new();
    for (original_index, (tab, flags)) in tabs.layouts().enumerate() {
        // 含非常驻会话窗格的 tab 整页跳过：任务元数据是运行时专有的，把
        // 窗格塞进快照只会在重启后冒出一个指向别处的重复页。被跳过页里
        // 的交互会话在恢复时按孤儿各自落到单窗格 tab。
        if tab.session_indices().into_iter().any(|index| {
            session_manager
                .sessions()
                .get(index)
                .is_none_or(|session| {
                    session.purpose != crate::session::SessionPurpose::Interactive
                })
        }) {
            continue;
        }
        let Some(mut snapshot) = tab.to_snapshot(&session_ids) else {
            continue;
        };
        // 固定/标记是 tab 级别的状态，布局快照本身不知道它们。
        snapshot.pinned = flags.pinned;
        snapshot.marked = flags.marked;
        snapshot.private_title = flags.private_title;
        if original_index == original_active_tab {
            active_tab = Some(kept_tabs.len());
        }
        kept_tabs.push(snapshot);
    }
    if active_tab.is_none() && !kept_tabs.is_empty() {
        active_tab = Some(0);
    }
    session_persistence::SessionsSnapshot::from_snapshots(
        snapshots,
        session_manager.restorable_active_index(),
        kept_tabs,
        active_tab,
    )
}

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
                // 只写 stderr 的话,拒写保护生效时用户会以为字号等改动已经落盘,
                // 直到下次启动才发现全都没了。
                self.set_status_for(
                    format!("配置未保存：{e}"),
                    std::time::Duration::from_secs(6),
                );
            } else {
                // Config::save adopts the exact bytes it published, so the
                // hot-reload watcher recognizes this generation immediately.
            }
        }
    }

    pub fn schedule_session_save(&mut self) {
        self.session_save_pending = true;
        self.session_save_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    }

    pub(crate) fn current_sessions_snapshot(&self) -> session_persistence::SessionsSnapshot {
        sessions_snapshot_for_persistence(&self.session_manager, &self.tabs)
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
            if self._lock_file.is_none() || self.session_persistence_blocked {
                return;
            }
            if let Ok(path) = self.config.resolved_session_history_path() {
                let _ = session_persistence::ensure_session_history_dir(&path);
                let snapshot = self.current_sessions_snapshot();
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

        let disk_revision = match config::Config::current_revision() {
            Ok(revision) => revision,
            Err(error) => {
                eprintln!("[Config] Hot-reload read error: {error}");
                self.set_status_for(
                    format!("配置暂时无法读取，已保留当前值：{error}"),
                    std::time::Duration::from_secs(6),
                );
                return;
            }
        };
        if self.config.observed_revision() == Some(&disk_revision) {
            return;
        }

        // A settings panel owns a stable editing surface. If the user saves
        // it after an external edit, the exact-revision CAS below rejects the
        // stale write instead of silently merging or overwriting fields.
        if self.config_panel.is_open {
            return;
        }

        if self.config_save_pending {
            self.config_save_pending = false;
            self.config.revision = Some(disk_revision);
            self.config.load_error = Some(
                "config changed outside this window while local edits were pending; reset or edit the file again before saving"
                    .to_string(),
            );
            self.set_status_for(
                "配置在本地修改待保存时被外部更改；已停止自动写入",
                std::time::Duration::from_secs(8),
            );
            return;
        }

        let config_path = match config::Config::config_path() {
            Ok(path) => path,
            Err(error) => {
                self.set_status_for(
                    format!("无法定位配置文件：{error}"),
                    std::time::Duration::from_secs(6),
                );
                return;
            }
        };
        match config::Config::from_revision(&config_path, &disk_revision) {
            Ok(new_config) => {
                let notes = self.apply_hot_reload(new_config, ctx);
                eprintln!("[Config] Hot-reloaded from {}", config_path.display());
                if notes.is_empty() {
                    self.set_status("配置已热重载");
                } else {
                    for note in &notes {
                        eprintln!("[Config] WARNING: {note}");
                    }
                    self.set_status_for(
                        format!("配置已重载（{} 项已调整）", notes.len()),
                        std::time::Duration::from_secs(5),
                    );
                }
            }
            Err(error) => {
                eprintln!("[Config] Hot-reload parse error: {error}");
                self.status_message = format!("配置解析失败,已沿用旧配置: {error}");
                self.status_expires_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(6));
                self.config.revision = Some(disk_revision);
                self.config.load_error = Some(error);
            }
        }
    }

    fn apply_hot_reload(
        &mut self,
        mut new: config::Config,
        ctx: &eframe::egui::Context,
    ) -> Vec<String> {
        let mut notes = Vec::new();
        if self.agent_runtime.has_any_activity() && !new.experimental_task_sidebar {
            new.experimental_task_sidebar = true;
            notes.push(
                "Tasks remains enabled while native work is active; turn it off after cleanup"
                    .to_string(),
            );
        }
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

        let configured_shell = std::env::var("EMBER_SHELL").ok().or(new.shell.clone());
        self.session_manager.set_configured_shell(configured_shell);
        // A toggled bottom bar changes the height left for the grid; re-grid
        // the PTY at once instead of waiting for the next natural resize.
        if new.bottom_bar != self.config.bottom_bar {
            self.force_resize_session = true;
        }
        self.config = new;
        self.config_panel.sync_from_config(&self.config);
        self.apply_runtime_config(ctx);
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::{safe_window_title, sessions_snapshot_for_persistence};

    #[test]
    fn empty_or_control_only_osc_title_uses_a_safe_fallback() {
        assert_eq!(safe_window_title("", "~/work — Ember"), "~/work — Ember");
        assert_eq!(safe_window_title("\u{1b}\u{202e}", ""), "Ember");
    }

    #[test]
    fn terminal_title_is_single_line_bidi_safe_and_bounded() {
        assert_eq!(
            safe_window_title(" build\n\u{202e}done\t now ", "fallback"),
            "build done now"
        );
        let long = "界".repeat(300);
        let title = safe_window_title(&long, "fallback");
        assert_eq!(title.chars().count(), 201);
        assert!(title.ends_with('…'));
    }

    fn interactive_fixture_session(
        name: &str,
    ) -> (crate::session::Session, String, egui::Context) {
        let repaint = egui::Context::default();
        let session_id = format!("test-{name}-{}", uuid::Uuid::new_v4());
        let shell = crate::shell::ShellSession::new_with_cwd(
            80,
            24,
            Some("/tmp"),
            Some(&session_id),
            Some("/bin/sh"),
            None,
            repaint.clone(),
        )
        .expect("interactive fixture shell starts");
        let session = crate::session::Session::new_with_session_id(
            name.to_string(),
            Vec::new(),
            std::sync::Arc::new(parking_lot::Mutex::new(
                crate::terminal::TerminalState::new(80, 24),
            )),
            shell,
            session_id.clone(),
        );
        (session, session_id, repaint)
    }

    #[test]
    fn task_terminal_tabs_are_pruned_from_session_snapshots() {
        let (first, first_id, repaint) = interactive_fixture_session("interactive");
        let mut manager =
            crate::session_manager::SessionManager::new(first, repaint, Some("/bin/sh".into()));

        // 任务终端：Agent CLI 走精确 argv，purpose 是 EphemeralCommand。
        let task = manager
            .new_command_session_in_cwd(
                "Agent".to_string(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "exit 0".to_string()],
                std::path::Path::new("/tmp"),
                80,
                24,
                100,
            )
            .expect("task terminal session starts");
        assert_eq!(task.session_index, 1);
        assert!(manager.switch_session(task.session_index));
        // 交互会话插到任务终端之后：全局下标 [S0, T1, S2]，过滤后 [S0, S2]。
        let second = manager
            .new_session_in_cwd(None, None, std::path::Path::new("/tmp"), 80, 24, 100)
            .expect("second interactive session starts");
        assert_eq!(second.session_index, 2);
        assert!(manager.switch_session(second.session_index));
        let second_id = manager.sessions()[second.session_index]
            .metadata
            .session_id
            .clone();

        let mut tabs = crate::tab_manager::TabManager::new(0);
        tabs.insert_tab_after_active(task.session_index);
        tabs.insert_tab_after_active(second.session_index);

        let snapshot = sessions_snapshot_for_persistence(&manager, &tabs);

        // 任务终端的会话与 tab 都不进快照……
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(
            snapshot.sessions[1].session_id.as_deref(),
            Some(second_id.as_str())
        );
        assert_eq!(snapshot.tabs.len(), 2);
        // ……而夹在中间的 S2 的窗格映射到它自己的稳定 ID（不会被任务终端
        // 顶掉或错配）。
        assert_eq!(
            snapshot.tabs[0].root,
            crate::session_persistence::LayoutNodeSnapshot::Pane {
                session_id: first_id
            }
        );
        assert_eq!(
            snapshot.tabs[1].root,
            crate::session_persistence::LayoutNodeSnapshot::Pane {
                session_id: second_id
            }
        );
        // 活动下标全部重映射到过滤后的幸存空间。
        assert_eq!(snapshot.active_tab, Some(1));
        assert_eq!(snapshot.active_index, Some(1));
    }
}
