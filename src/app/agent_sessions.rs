//! Discover and resume existing provider-owned CLI conversations.
//! Discovery never runs on the event thread; launch uses argv, not shell text.
use super::state::TerminalApp;
use eframe::egui;
use jterm_core::agent_sessions::{
    AgentSession, SessionCatalog, SessionProvider, SessionScanner, SessionStatus,
};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub(crate) struct AgentSessionsState {
    pub(crate) is_open: bool,
    scanner: SessionScanner,
    catalog: SessionCatalog,
    query: String,
    provider: Option<SessionProvider>,
    status: Option<SessionStatus>,
    live_only: bool,
    selected: Option<(SessionProvider, String)>,
    focus_search: bool,
    scroll_selection: bool,
    last_refresh: Option<Instant>,
    error: Option<String>,
    opened: HashMap<(SessionProvider, String), String>,
}

impl AgentSessionsState {
    fn visible(&self) -> Vec<usize> {
        self.catalog
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                self.provider.is_none_or(|p| p == s.provider)
                    && self.status.is_none_or(|status| status == s.status)
                    && (!self.live_only || !self.catalog.live_pids(s).is_empty())
                    && s.matches_query(&self.query)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_session(&self) -> Option<&AgentSession> {
        let (provider, id) = self.selected.as_ref()?;
        self.catalog
            .sessions
            .iter()
            .find(|s| s.provider == *provider && s.id == *id)
    }

    fn reconcile_selection(&mut self) {
        let visible = self.visible();
        if !visible.iter().any(|i| {
            let s = &self.catalog.sessions[*i];
            self.selected
                .as_ref()
                .is_some_and(|(p, id)| *p == s.provider && *id == s.id)
        }) {
            self.selected = visible.first().map(|i| {
                let s = &self.catalog.sessions[*i];
                (s.provider, s.id.clone())
            });
        }
    }

    fn refresh(&mut self) {
        if self.scanner.refresh() {
            self.last_refresh = Some(Instant::now());
        }
    }
}

// egui applies text edits during rendering, after our event prepass. Never let
// an Enter bundled with a query edit resume the previous frame's selection.
fn query_edit_pending(events: &[egui::Event]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            egui::Event::Text(_)
                | egui::Event::Paste(_)
                | egui::Event::Ime(egui::ImeEvent::Commit(_))
                | egui::Event::Key {
                    key: egui::Key::Backspace | egui::Key::Delete,
                    pressed: true,
                    ..
                }
        )
    })
}

fn age_label(updated_at_ms: u64) -> String {
    if updated_at_ms == 0 {
        return "time unknown".into();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let seconds = now.saturating_sub(updated_at_ms) / 1000;
    match seconds {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

impl TerminalApp {
    pub(crate) fn toggle_agent_sessions(&mut self) {
        let state = &mut self.agent_sessions;
        state.is_open = !state.is_open;
        if state.is_open {
            state.focus_search = true;
            state.error = None;
            state.refresh();
        }
    }

    /// Own the complete key batch, including the frame that closes or resumes.
    /// PTY pumping and app persistence continue in the main event loop.
    pub(crate) fn handle_agent_sessions_input(&mut self, ctx: &egui::Context) -> bool {
        if !self.agent_sessions.is_open {
            return false;
        }
        self.agent_sessions.reconcile_selection();
        let query_is_changing = query_edit_pending(&self.frame_events);
        for event in self.frame_events.clone() {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            match key {
                egui::Key::Escape => {
                    self.agent_sessions.is_open = false;
                    self.return_focus_to_terminal(ctx);
                    break;
                }
                egui::Key::Enter if !repeat && !query_is_changing => {
                    self.resume_selected_agent_session(ctx);
                    break;
                }
                egui::Key::ArrowUp | egui::Key::ArrowDown
                    if !modifiers.command && !modifiers.alt =>
                {
                    let visible = self.agent_sessions.visible();
                    if visible.is_empty() {
                        continue;
                    }
                    let position = visible
                        .iter()
                        .position(|i| {
                            let s = &self.agent_sessions.catalog.sessions[*i];
                            self.agent_sessions
                                .selected
                                .as_ref()
                                .is_some_and(|(p, id)| *p == s.provider && *id == s.id)
                        })
                        .unwrap_or(0);
                    let next = if key == egui::Key::ArrowUp {
                        position.saturating_sub(1)
                    } else {
                        (position + 1).min(visible.len() - 1)
                    };
                    let s = &self.agent_sessions.catalog.sessions[visible[next]];
                    self.agent_sessions.selected = Some((s.provider, s.id.clone()));
                    self.agent_sessions.scroll_selection = true;
                }
                egui::Key::F5 if !repeat => self.agent_sessions.refresh(),
                _ => {}
            }
        }
        true
    }

    fn resume_selected_agent_session(&mut self, ctx: &egui::Context) {
        let Some(session) = self.agent_sessions.selected_session().cloned() else {
            return;
        };
        let key = (session.provider, session.id.clone());
        if let Some(terminal_id) = self.agent_sessions.opened.get(&key).cloned() {
            let existing = self.session_manager.index_of(&terminal_id).filter(|index| {
                self.session_manager.sessions()[*index].purpose
                    == crate::session::SessionPurpose::EphemeralCommand
            });
            if let Some(index) = existing {
                self.activate_session(index);
                self.claim_enter_for_prompt_recall();
                self.agent_sessions.is_open = false;
                self.return_focus_to_terminal(ctx);
                self.set_status(format!(
                    "Switched to the open {} conversation",
                    session.provider.label()
                ));
                return;
            }
            self.agent_sessions.opened.remove(&key);
        }
        let launch = match session
            .resume_launch()
            .and_then(|launch| launch.resolve_for_spawn())
        {
            Ok(launch) => launch,
            Err(error) => {
                self.agent_sessions.error = Some(error);
                return;
            }
        };
        let argv = std::iter::once(launch.program).chain(launch.args).collect();
        let (cols, rows) = crate::terminal::clamp_terminal_dimensions(self.cols, self.rows);
        let title = format!(
            "{} · {}",
            session.provider.label(),
            crate::review_text::visible_bounded(&session.title, 96)
        );
        match self.session_manager.new_command_session_in_cwd(
            title,
            argv,
            &launch.cwd,
            cols,
            rows,
            self.config.scrollback_lines,
        ) {
            Ok(created) => {
                self.agent_sessions
                    .opened
                    .insert(key, created.session_id.clone());
                self.tabs.on_session_inserted(created.session_index);
                self.tabs.insert_tab_after_active(created.session_index);
                self.activate_session(created.session_index);
                self.schedule_session_save();
                self.claim_enter_for_prompt_recall();
                self.agent_sessions.is_open = false;
                self.agent_sessions.error = None;
                self.return_focus_to_terminal(ctx);
                self.set_status(format!(
                    "Resuming {} conversation in a new tab",
                    session.provider.label()
                ));
            }
            Err(error) => {
                self.agent_sessions.error = Some(format!(
                    "Could not resume {}: {error}",
                    session.provider.label()
                ))
            }
        }
    }

    pub(crate) fn handle_agent_session_terminal_exit(&mut self, terminal_id: &str) -> bool {
        let owned = self
            .agent_sessions
            .opened
            .values()
            .any(|id| id == terminal_id);
        if owned {
            self.agent_sessions.opened.retain(|_, id| id != terminal_id);
            self.session_manager.retain_exited_command(terminal_id);
        }
        owned
    }

    pub(crate) fn render_agent_sessions(&mut self, ctx: &egui::Context) {
        if !self.agent_sessions.is_open {
            return;
        }
        let manager = &self.session_manager;
        self.agent_sessions.opened.retain(|_, terminal_id| {
            manager.index_of(terminal_id).is_some_and(|index| {
                manager.sessions()[index].purpose
                    == crate::session::SessionPurpose::EphemeralCommand
            })
        });
        let state = &mut self.agent_sessions;
        if let Some(catalog) = state.scanner.poll() {
            state.catalog = catalog;
            state.reconcile_selection();
        }
        if state
            .last_refresh
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(10))
        {
            state.refresh();
        }
        ctx.request_repaint_after(Duration::from_millis(if state.scanner.is_scanning() {
            100
        } else {
            1000
        }));
        let mut open = true;
        let mut resume = false;
        egui::Window::new("Agent Sessions")
            .id(egui::Id::new("agent_sessions_manager"))
            .open(&mut open)
            .default_size(egui::vec2(1000.0, 620.0))
            .min_size(egui::vec2(640.0, 420.0))
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Pick up where you left off").size(20.0).strong());
                ui.label("Codex + Claude · all projects · local conversation history");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let total = state.catalog.sessions.len();
                    let codex = state.catalog.sessions.iter().filter(|s| s.provider == SessionProvider::Codex).count();
                    let waiting = state.catalog.sessions.iter().filter(|s| s.status == SessionStatus::Waiting).count();
                    let live = state.catalog.live_processes.len();
                    ui.label(egui::RichText::new(format!("{total} sessions   ·   {codex} Codex   ·   {} Claude   ·   {waiting} waiting   ·   {live} live processes", total - codex)).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!state.scanner.is_scanning(), egui::Button::new("Refresh  F5")).clicked() { state.refresh(); }
                        if state.scanner.is_scanning() { ui.spinner(); }
                    });
                });
                ui.horizontal(|ui| {
                    let response = ui.add(egui::TextEdit::singleline(&mut state.query).hint_text("Search title, project, branch, session ID or preview…").desired_width(f32::INFINITY).char_limit(1024));
                    if state.focus_search { response.request_focus(); state.focus_search = false; }
                });
                ui.horizontal(|ui| {
                    ui.label("Provider");
                    for (label, value) in [("All", None), ("Codex", Some(SessionProvider::Codex)), ("Claude", Some(SessionProvider::Claude))] {
                        ui.selectable_value(&mut state.provider, value, label);
                    }
                    ui.separator();
                    egui::ComboBox::from_id_salt("agent_session_status_filter")
                        .selected_text(state.status.map(|s| s.label()).unwrap_or("All states"))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut state.status, None, "All states");
                            for status in [SessionStatus::Working, SessionStatus::Waiting, SessionStatus::Idle, SessionStatus::Unknown, SessionStatus::Archived] {
                                ui.selectable_value(&mut state.status, Some(status), status.label());
                            }
                        });
                    ui.checkbox(&mut state.live_only, "Observed live only").on_hover_text("Show only sessions with an exact local CLI process match at the latest refresh. Independent of the last recorded state.");
                    if ui.small_button("Reset").clicked() { state.query.clear(); state.provider = None; state.status = None; state.live_only = false; }
                });
                state.reconcile_selection();
                let visible = state.visible();
                ui.separator();
                ui.small(format!("{} matching sessions · newest first", visible.len()));
                if let Some(error) = &state.error { ui.colored_label(egui::Color32::LIGHT_RED, crate::review_text::visible_bounded(error, 1024)); }
                if !state.catalog.warnings.is_empty() {
                    egui::CollapsingHeader::new(format!("{} discovery notices", state.catalog.warnings.len())).show(ui, |ui| {
                        for warning in &state.catalog.warnings { ui.small(crate::review_text::visible_bounded(warning, 1024)); }
                    });
                }
                let height = (ui.available_height() - 46.0).max(160.0);
                ui.columns(2, |columns| {
                    let mut list = egui::ScrollArea::vertical().id_salt("agent_sessions_list").max_height(height);
                    if state.scroll_selection {
                        if let Some(row) = visible.iter().position(|i| {
                            let session = &state.catalog.sessions[*i];
                            state.selected.as_ref().is_some_and(|(p, id)| *p == session.provider && *id == session.id)
                        }) {
                            list = list.vertical_scroll_offset(row as f32 * 69.0);
                        }
                        state.scroll_selection = false;
                    }
                    list.show_rows(&mut columns[0], 69.0, visible.len(), |ui, range| {
                        for row in range {
                            let session = &state.catalog.sessions[visible[row]];
                            let selected = state.selected.as_ref().is_some_and(|(p, id)| *p == session.provider && *id == session.id);
                            let live_pids = state.catalog.live_pids(session);
                            let live_badge = if live_pids.is_empty() { String::new() } else { format!("  ·  LIVE {}", live_pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")) };
                            let title = crate::review_text::visible_bounded(&session.title, 160);
                            let text = format!("{}  ·  {}  ·  {}{}\n{}\n{}", session.provider.label(), session.status.label(), age_label(session.updated_at_ms), live_badge, title, crate::review_text::visible_bounded(&session.cwd.to_string_lossy(), 160));
                            let response = ui.push_id((session.provider.label(), &session.id), |ui| ui.add_sized([ui.available_width(), 64.0], egui::Button::selectable(selected, text).wrap())).inner;
                            if response.clicked() { state.selected = Some((session.provider, session.id.clone())); }
                        }
                    });
                    if visible.is_empty() {
                        columns[0].label(if state.scanner.is_scanning() { "Looking for local conversations…" } else if state.catalog.sessions.is_empty() { "No Codex or Claude sessions found. Start a conversation in either CLI, then refresh." } else { "No sessions match. Try another query or reset the filters." });
                    }
                    egui::ScrollArea::vertical().id_salt("agent_session_detail").max_height(height).show(&mut columns[1], |ui| {
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        if let Some(session) = state.selected_session() {
                            ui.heading(crate::review_text::visible_bounded(&session.title, 240));
                            ui.label(format!("{} · {} · {}", session.provider.label(), session.status.label(), age_label(session.updated_at_ms)));
                            let live_pids = state.catalog.live_pids(session);
                            if !live_pids.is_empty() {
                                ui.colored_label(egui::Color32::LIGHT_GREEN, format!("Live CLI process · PID {}", live_pids.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")));
                                ui.small("Exact session-ID process match; recorded state remains separate.");
                            }
                            if state.opened.contains_key(&(session.provider, session.id.clone())) {
                                ui.label("Open in this window · confirming switches to its tab");
                            }
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new("Project").strong());
                            ui.label(crate::review_text::visible_bounded(&session.cwd.to_string_lossy(), 1024));
                            if let Some(branch) = &session.branch { ui.label(format!("Branch: {}", crate::review_text::visible_bounded(branch, 160))); }
                            ui.label(format!("Session: {}", crate::review_text::visible_bounded(&session.id, 160)));
                            ui.separator();
                            ui.label(egui::RichText::new("Latest context").strong());
                            ui.label(if session.preview.is_empty() { "No message preview available".to_string() } else { crate::review_text::visible_bounded(&session.preview, 8192) });
                            ui.add_space(10.0);
                            match session.resume_launch() {
                                Ok(launch) => {
                                    ui.label(egui::RichText::new("Resume command").strong());
                                    ui.monospace(launch.display_command());
                                    if ui.button("Copy command").clicked() { ui.ctx().copy_text(launch.display_command()); }
                                }
                                Err(error) => { ui.colored_label(egui::Color32::LIGHT_RED, error); }
                            }
                        } else { ui.label("Select a conversation to inspect its project and latest context."); }
                        });
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.small("↑ ↓ select · Enter resume · Esc close");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_resume = state.selected.as_ref().is_some_and(|key| state.opened.contains_key(key)) || state.selected_session().is_some_and(|s| s.resume_launch().is_ok());
                        if ui.add_enabled(can_resume, egui::Button::new(if state.selected.as_ref().is_some_and(|key| state.opened.contains_key(key)) { "Switch to Open Tab  ↵" } else { "Resume in New Tab  ↵" })).clicked() { resume = true; }
                    });
                });
                ui.small("States are last recorded events. LIVE badges show exact session-ID process matches; they do not imply busy.");
            });
        if !open {
            self.agent_sessions.is_open = false;
            self.return_focus_to_terminal(ctx);
        }
        if resume {
            self.resume_selected_agent_session(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session(provider: SessionProvider, id: &str, title: &str) -> AgentSession {
        AgentSession {
            provider,
            id: id.into(),
            title: title.into(),
            cwd: PathBuf::from("/tmp"),
            updated_at_ms: 1,
            status: SessionStatus::Idle,
            preview: "latest response".into(),
            branch: None,
            source: PathBuf::from("/tmp/session.jsonl"),
            archived: false,
        }
    }

    #[test]
    fn selection_tracks_provider_and_identity_across_refresh_order() {
        let mut state = AgentSessionsState::default();
        state.catalog.sessions = vec![
            session(SessionProvider::Codex, "same-id", "Codex work"),
            session(SessionProvider::Claude, "same-id", "Claude work"),
        ];
        state.selected = Some((SessionProvider::Claude, "same-id".into()));
        state.catalog.sessions.reverse();
        state.reconcile_selection();
        assert_eq!(
            state.selected_session().unwrap().provider,
            SessionProvider::Claude
        );
        assert_eq!(state.selected_session().unwrap().title, "Claude work");
    }

    #[test]
    fn enter_batched_with_query_edit_cannot_resume_stale_selection() {
        assert!(query_edit_pending(&[egui::Event::Text(
            "another project".into()
        )]));
        assert!(query_edit_pending(&[egui::Event::Paste(
            "session-id".into()
        )]));
        assert!(query_edit_pending(&[egui::Event::Ime(
            egui::ImeEvent::Commit("项目".into())
        )]));
        assert!(!query_edit_pending(&[]));
    }

    #[test]
    fn observed_live_filter_is_independent_from_recorded_state() {
        let mut state = AgentSessionsState::default();
        let live = session(SessionProvider::Codex, "live-id", "awaiting input");
        let mut stale_working = session(SessionProvider::Claude, "stale-id", "old task start");
        stale_working.status = SessionStatus::Working;
        state.catalog.sessions = vec![live, stale_working];
        state
            .catalog
            .live_processes
            .push(jterm_core::agent_sessions::LiveSessionProcess {
                provider: SessionProvider::Codex,
                id: "live-id".into(),
                pid: 1234,
            });
        state.live_only = true;
        state.reconcile_selection();
        assert_eq!(state.visible(), vec![0]);
        assert_eq!(
            state.selected_session().unwrap().status,
            SessionStatus::Idle
        );
        state.status = Some(SessionStatus::Working);
        state.reconcile_selection();
        assert!(state.visible().is_empty());
        assert!(state.selected_session().is_none());
        state.status = None;
        state.catalog.live_processes.clear();
        state.reconcile_selection();
        assert!(state.visible().is_empty());
    }

    #[test]
    fn filters_cannot_resume_a_hidden_selection() {
        let mut state = AgentSessionsState::default();
        state.catalog.sessions = vec![
            session(SessionProvider::Codex, "codex-id", "first"),
            session(SessionProvider::Claude, "claude-id", "second"),
        ];
        state.reconcile_selection();
        state.provider = Some(SessionProvider::Claude);
        state.reconcile_selection();
        assert_eq!(state.selected_session().unwrap().id, "claude-id");
        state.query = "no matching conversation".into();
        state.reconcile_selection();
        assert!(state.selected_session().is_none());
    }
}
