//! Discover and resume existing provider-owned CLI conversations.
//! Discovery never runs on the event thread; launch uses argv, not shell text.
use super::state::TerminalApp;
use eframe::egui;
use jterm_core::agent_sessions::{
    AgentSession, SessionCatalog, SessionLaunch, SessionLibraryFilter, SessionLibrarySnapshot,
    SessionLibraryStore, SessionPreflight, SessionPreflightState, SessionProvider, SessionQuery,
    SessionScanner, SessionSearchIndex, SessionSort, SessionStatus,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type SessionKey = (SessionProvider, String);

#[derive(Clone, Debug, PartialEq, Eq)]
struct FilterKey {
    text: String,
    provider: Option<SessionProvider>,
    status: Option<SessionStatus>,
    live_only: bool,
    pinned_only: bool,
    recent_only: bool,
    project: Option<PathBuf>,
    sort: SessionSort,
}

#[derive(Clone, Debug)]
struct ScrollAnchor {
    key: SessionKey,
    within_row: f32,
}

#[derive(Default)]
pub(crate) struct AgentSessionsState {
    pub(crate) is_open: bool,
    scanner: SessionScanner,
    library: SessionLibraryStore,
    library_snapshot: SessionLibrarySnapshot,
    library_refresh: Option<Instant>,
    library_notice: Option<String>,
    preflight: SessionPreflight,
    pending_resume: Option<AgentSession>,
    catalog: SessionCatalog,
    index: SessionSearchIndex,
    cached_visible: RefCell<Option<(FilterKey, Vec<usize>)>>,
    query: String,
    provider: Option<SessionProvider>,
    status: Option<SessionStatus>,
    live_only: bool,
    pinned_only: bool,
    recent_only: bool,
    sort: SessionSort,
    scope_cwd: Option<PathBuf>,
    project_only: bool,
    selected: Option<SessionKey>,
    focus_search: bool,
    search_widget: Option<egui::Id>,
    row_focus_widget: Option<egui::Id>,
    scroll_selection: bool,
    scroll_anchor: Option<ScrollAnchor>,
    restore_anchor: bool,
    page_rows: usize,
    last_refresh: Option<Instant>,
    error: Option<String>,
    opened: HashMap<SessionKey, String>,
}

impl AgentSessionsState {
    fn filter_key(&self) -> FilterKey {
        FilterKey {
            text: self.query.clone(),
            provider: self.provider,
            status: self.status,
            live_only: self.live_only,
            pinned_only: self.pinned_only,
            recent_only: self.recent_only,
            sort: self.sort,
            project: self.project_only.then(|| self.scope_cwd.clone()).flatten(),
        }
    }

    fn visible(&self) -> Vec<usize> {
        let key = self.filter_key();
        if let Some((cached_key, rows)) = self.cached_visible.borrow().as_ref() {
            if *cached_key == key {
                return rows.clone();
            }
        }
        let query = SessionQuery {
            text: &key.text,
            provider: key.provider,
            status: key.status,
            live_only: key.live_only,
            project: key.project.as_deref(),
            sort: key.sort,
        };
        // Catalog replacement installs one normalized index for every query.
        let rows = self.index.query_with_library(
            &query,
            &self.library_snapshot,
            SessionLibraryFilter {
                pinned_only: key.pinned_only,
                recent_only: key.recent_only,
                pinned_first: true,
            },
        );
        *self.cached_visible.borrow_mut() = Some((key, rows.clone()));
        rows
    }

    fn replace_catalog(&mut self, catalog: SessionCatalog) -> bool {
        if self.catalog == catalog {
            return false;
        }
        self.index = SessionSearchIndex::new(&catalog);
        self.catalog = catalog;
        self.cached_visible.get_mut().take();
        self.restore_anchor = self.scroll_anchor.is_some();
        self.reconcile_selection();
        true
    }

    fn replace_library(&mut self, snapshot: SessionLibrarySnapshot) -> bool {
        if self.library_snapshot == snapshot {
            return false;
        }
        self.library_snapshot = snapshot;
        self.cached_visible.get_mut().take();
        self.restore_anchor = self.scroll_anchor.is_some();
        self.reconcile_selection();
        true
    }

    fn poll_library(&mut self) {
        if self.library.poll() {
            self.replace_library(self.library.snapshot().clone());
        }
    }

    fn sync_preflight(&mut self) {
        let selected = self
            .selected_session()
            .filter(|session| !self.opened.contains_key(&session_key(session)))
            .cloned();
        if self.preflight.select(selected.as_ref()) {
            self.pending_resume = None;
        }
        self.preflight.poll();
    }

    fn cancel_resume(&mut self) {
        self.pending_resume = None;
        self.preflight.select(None);
    }

    fn note_resumed(&mut self, session: &AgentSession) {
        self.library_notice = self
            .library
            .record_resumed(session.provider, &session.id)
            .err()
            .map(|error| {
                format!("Resumed successfully, but recent activity could not be saved: {error}")
            });
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
            self.selected
                .as_ref()
                .is_some_and(|key| matches_key(&self.catalog.sessions[*i], key))
        }) {
            self.selected = visible
                .first()
                .map(|i| session_key(&self.catalog.sessions[*i]));
        }
    }

    fn filters_changed(&mut self) {
        self.selected = None;
        self.error = None;
        self.scroll_anchor = None;
        self.restore_anchor = false;
        self.scroll_selection = true;
        self.reconcile_selection();
    }

    fn clear_filters(&mut self) {
        self.query.clear();
        self.provider = None;
        self.status = None;
        self.live_only = false;
        self.pinned_only = false;
        self.recent_only = false;
        self.project_only = false;
        self.sort = SessionSort::Relevance;
        self.filters_changed();
        self.focus_search = true;
    }

    fn navigate(&mut self, key: egui::Key) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let position = visible
            .iter()
            .position(|i| {
                self.selected
                    .as_ref()
                    .is_some_and(|selected| matches_key(&self.catalog.sessions[*i], selected))
            })
            .unwrap_or(0);
        let page = self.page_rows.max(1);
        let next = match key {
            egui::Key::ArrowUp => position.saturating_sub(1),
            egui::Key::ArrowDown => (position + 1).min(visible.len() - 1),
            egui::Key::PageUp => position.saturating_sub(page),
            egui::Key::PageDown => position.saturating_add(page).min(visible.len() - 1),
            egui::Key::Home => 0,
            egui::Key::End => visible.len() - 1,
            _ => return,
        };
        self.selected = Some(session_key(&self.catalog.sessions[visible[next]]));
        self.scroll_selection = true;
        self.restore_anchor = false;
    }

    fn anchor_offset(&self, visible: &[usize], stride: f32) -> Option<f32> {
        let anchor = self.scroll_anchor.as_ref()?;
        visible
            .iter()
            .position(|i| matches_key(&self.catalog.sessions[*i], &anchor.key))
            .map(|row| row as f32 * stride + anchor.within_row.min(stride))
    }

    fn remember_anchor(&mut self, visible: &[usize], offset: f32, stride: f32) {
        self.scroll_anchor = if visible.is_empty() {
            None
        } else {
            let row = ((offset / stride).floor() as usize).min(visible.len() - 1);
            Some(ScrollAnchor {
                key: session_key(&self.catalog.sessions[visible[row]]),
                within_row: (offset - row as f32 * stride).max(0.0),
            })
        };
    }

    fn refresh(&mut self) {
        if self.scanner.refresh() {
            self.last_refresh = Some(Instant::now());
        }
    }
}

fn same_launch_target(left: &AgentSession, right: &AgentSession) -> bool {
    left.provider == right.provider
        && left.id == right.id
        && left.cwd == right.cwd
        && left.source == right.source
        && left.archived == right.archived
}

fn matches_key(session: &AgentSession, key: &SessionKey) -> bool {
    session.provider == key.0 && session.id == key.1
}

fn session_key(session: &AgentSession) -> SessionKey {
    (session.provider, session.id.clone())
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

// Navigation/confirmation belongs to search and session rows. Focused filter
// controls retain their own keyboard gestures and cannot accidentally resume.
fn session_key_route(
    focused: Option<egui::Id>,
    search: Option<egui::Id>,
    row: Option<egui::Id>,
) -> bool {
    focused.is_none_or(|id| Some(id) == search || Some(id) == row)
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
        let cwd = self
            .session_manager
            .sessions()
            .get(self.session_manager.active_index())
            .and_then(|session| jterm_core::process::process_cwd(session.get_shell_pid()))
            .map(PathBuf::from);
        let state = &mut self.agent_sessions;
        state.is_open = !state.is_open;
        if !state.is_open {
            state.cancel_resume();
        }
        if state.is_open {
            let previous = state.filter_key();
            state.scope_cwd = cwd;
            if state.scope_cwd.is_none() {
                state.project_only = false;
            }
            if state.filter_key() != previous {
                state.filters_changed();
            }
            state.focus_search = true;
            state.error = None;
            state.library.refresh();
            state.library_refresh = Some(Instant::now());
            state.refresh();
        }
    }

    /// Own the complete key batch, including the frame that closes or resumes.
    /// PTY pumping and app persistence continue in the main event loop.
    pub(crate) fn handle_agent_sessions_input(&mut self, ctx: &egui::Context) -> bool {
        let previous_selection = self.agent_sessions.selected.clone();
        let previous_library_error = self.agent_sessions.library.error().map(str::to_owned);
        self.agent_sessions.poll_library();
        if !self.agent_sessions.is_open {
            if let Some(error) = self
                .agent_sessions
                .library
                .error()
                .map(str::to_owned)
                .filter(|error| Some(error) != previous_library_error.as_ref())
            {
                self.set_status_for(
                    format!("Session library could not sync: {error}"),
                    Duration::from_secs(6),
                );
            }
        }
        let metadata_changed_selection = previous_selection != self.agent_sessions.selected;
        if metadata_changed_selection {
            self.agent_sessions.pending_resume = None;
        }
        if !self.agent_sessions.is_open && self.agent_sessions.preflight.is_busy() {
            self.agent_sessions.preflight.poll();
        }
        if !self.agent_sessions.is_open {
            return false;
        }
        // A filter popup owns its Enter/arrows/Escape. Confirming a sort or
        // status choice must never also resume the current conversation.
        if egui::Popup::is_any_open(ctx) {
            return true;
        }
        self.agent_sessions.reconcile_selection();
        let query_is_changing = query_edit_pending(&self.frame_events);
        if query_is_changing {
            self.agent_sessions.pending_resume = None;
        }
        let session_confirmation = session_key_route(
            ctx.memory(|memory| memory.focused()),
            self.agent_sessions.search_widget,
            self.agent_sessions.row_focus_widget,
        );
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
                    self.agent_sessions.cancel_resume();
                    self.return_focus_to_terminal(ctx);
                    break;
                }
                egui::Key::Enter
                    if !repeat
                        && !query_is_changing
                        && !metadata_changed_selection
                        && session_confirmation =>
                {
                    self.resume_selected_agent_session(ctx);
                    // A fresh check is asynchronous: later cancellation or
                    // navigation in this same key batch must still take effect.
                    if !self.agent_sessions.is_open {
                        break;
                    }
                }
                egui::Key::L if modifiers.command && !modifiers.alt => {
                    self.agent_sessions.focus_search = true;
                    ctx.input_mut(|input| {
                        input.consume_key(modifiers, key);
                    });
                }
                egui::Key::ArrowUp
                | egui::Key::ArrowDown
                | egui::Key::PageUp
                | egui::Key::PageDown
                | egui::Key::Home
                | egui::Key::End
                    if !modifiers.command && !modifiers.alt && session_confirmation =>
                {
                    self.agent_sessions.navigate(key);
                    ctx.input_mut(|input| {
                        input.consume_key(modifiers, key);
                    });
                }
                egui::Key::F5 if !repeat => {
                    self.agent_sessions.refresh();
                    self.agent_sessions.preflight.refresh();
                    self.agent_sessions.library.refresh();
                }
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
            if existing.is_some_and(|index| self.activate_session(index)) {
                self.agent_sessions.note_resumed(&session);
                self.agent_sessions.cancel_resume();
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
        self.agent_sessions.preflight.select(Some(&session));
        self.agent_sessions.preflight.refresh();
        self.agent_sessions.pending_resume = Some(session);
        self.agent_sessions.error = None;
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    fn finish_pending_agent_resume(&mut self, ctx: &egui::Context) {
        if !self.agent_sessions.is_open {
            self.agent_sessions.pending_resume = None;
            return;
        }
        self.agent_sessions.sync_preflight();
        let Some(pending) = self.agent_sessions.pending_resume.clone() else {
            return;
        };
        if !self.agent_sessions.is_open
            || !self
                .agent_sessions
                .selected_session()
                .is_some_and(|session| same_launch_target(session, &pending))
        {
            self.agent_sessions.pending_resume = None;
            return;
        }
        if let Some(launch) = self
            .agent_sessions
            .preflight
            .ready_launch(&pending)
            .cloned()
        {
            self.agent_sessions.pending_resume = None;
            self.spawn_ready_agent_session(pending, launch, ctx);
        } else if let SessionPreflightState::Unavailable(error) =
            self.agent_sessions.preflight.state()
        {
            self.agent_sessions.error = Some(error.clone());
            self.agent_sessions.pending_resume = None;
        }
    }

    fn spawn_ready_agent_session(
        &mut self,
        session: AgentSession,
        launch: SessionLaunch,
        ctx: &egui::Context,
    ) {
        let key = session_key(&session);
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
                self.agent_sessions.note_resumed(&session);
                self.agent_sessions
                    .opened
                    .insert(key, created.session_id.clone());
                self.tabs.on_session_inserted(created.session_index);
                self.tabs.insert_tab_after_active(created.session_index);
                self.activate_session(created.session_index);
                self.schedule_session_save();
                self.claim_enter_for_prompt_recall();
                self.agent_sessions.is_open = false;
                self.agent_sessions.cancel_resume();
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
        state.poll_library();
        if state
            .library_refresh
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(5))
        {
            if state.library.refresh() {
                state.library_refresh = Some(Instant::now());
            }
        }
        if let Some(catalog) = state.scanner.poll() {
            state.replace_catalog(catalog);
        }
        if state
            .last_refresh
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(10))
        {
            state.refresh();
        }
        state.sync_preflight();
        ctx.request_repaint_after(Duration::from_millis(
            if state.scanner.is_scanning() || state.library.is_busy() || state.preflight.is_busy() {
                100
            } else {
                1000
            },
        ));
        let mut open = true;
        let mut resume = false;
        let mut pin_change = None;
        egui::Window::new("Agent Sessions")
            .id(egui::Id::new("agent_sessions_manager"))
            .open(&mut open)
            .default_size(egui::vec2(1040.0, 700.0))
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
                    ui.label(egui::RichText::new(format!("{total} sessions   ·   {codex} Codex   ·   {} Claude   ·   {waiting} waiting   ·   {live} live processes   ·   {} pinned", total - codex, state.library_snapshot.pinned_count())).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!state.scanner.is_scanning(), egui::Button::new("Refresh  F5")).clicked() { state.refresh(); state.preflight.refresh(); state.library.refresh(); }
                        if state.scanner.is_scanning() { ui.spinner(); }
                    });
                });
                let previous_filters = state.filter_key();
                ui.horizontal(|ui| {
                    let response = ui.add(egui::TextEdit::singleline(&mut state.query).hint_text("Search title, project, branch, session ID or preview…").desired_width(f32::INFINITY).char_limit(1024));
                    state.search_widget = Some(response.id);
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
                    ui.add_enabled(state.library.is_loaded(), egui::Checkbox::new(&mut state.pinned_only, "Pinned only"));
                    ui.add_enabled(state.library.is_loaded(), egui::Checkbox::new(&mut state.recent_only, "Previously resumed"));
                    if ui.small_button("Clear filters").clicked() { state.clear_filters(); }
                });
                ui.horizontal_wrapped(|ui| {
                    let scope_available = state.scope_cwd.is_some();
                    ui.add_enabled(scope_available, egui::Checkbox::new(&mut state.project_only, "This folder + subfolders"))
                        .on_hover_text("Captured from the active local terminal when this manager opened. The scope stays fixed while you inspect sessions.");
                    if let Some(cwd) = &state.scope_cwd {
                        ui.add(egui::Label::new(crate::review_text::visible_bounded(&cwd.to_string_lossy(), 140)).truncate()).on_hover_text(cwd.to_string_lossy());
                    } else { ui.weak("Opening folder unavailable"); }
                    ui.separator();
                    ui.label("Sort");
                    egui::ComboBox::from_id_salt("agent_session_sort")
                        .selected_text(state.sort.label()).width(110.0)
                        .show_ui(ui, |ui| {
                            for sort in SessionSort::ALL {
                                ui.selectable_value(&mut state.sort, sort, sort.label());
                            }
                        });
                });
                if state.filter_key() != previous_filters { state.filters_changed(); }
                state.reconcile_selection();
                let visible = state.visible();
                ui.separator();
                ui.small(format!("{} matching sessions · {}{}", visible.len(), state.sort.label(), if state.query.trim().is_empty() && state.sort == SessionSort::Relevance { " · newest first" } else { "" }));
                if let Some(error) = &state.error { ui.colored_label(egui::Color32::LIGHT_RED, crate::review_text::visible_bounded(error, 1024)); }
                if !state.library.is_loaded() { ui.small("Loading shared favorites and resume history…"); }
                else if state.library.is_busy() { ui.small("Syncing favorites and resume history…"); }
                if let Some(error) = state.library.error().or(state.library_notice.as_deref()) {
                    ui.colored_label(egui::Color32::LIGHT_RED, crate::review_text::visible_bounded(error, 1024));
                }
                if !state.catalog.warnings.is_empty() {
                    egui::CollapsingHeader::new(format!("{} discovery notices", state.catalog.warnings.len())).show(ui, |ui| {
                        for warning in &state.catalog.warnings { ui.small(crate::review_text::visible_bounded(warning, 1024)); }
                    });
                }
                let height = (ui.available_height() - 72.0).max(160.0);
                ui.columns(2, |columns| {
                    const ROW_HEIGHT: f32 = 70.0;
                    let stride = ROW_HEIGHT + columns[0].spacing().item_spacing.y;
                    state.page_rows = (height / stride).floor().max(1.0) as usize;
                    let mut list = egui::ScrollArea::vertical().id_salt("agent_sessions_list").max_height(height).auto_shrink([false, false]);
                    if state.restore_anchor {
                        if let Some(offset) = state.anchor_offset(&visible, stride) {
                            list = list.vertical_scroll_offset(offset);
                        } else { state.scroll_selection = true; }
                        state.restore_anchor = false;
                    }
                    if state.scroll_selection {
                        if let Some(row) = visible.iter().position(|i| state.selected.as_ref().is_some_and(|key| *key == session_key(&state.catalog.sessions[*i]))) {
                            let old_offset = state.scroll_anchor.as_ref().and_then(|_| state.anchor_offset(&visible, stride)).unwrap_or(0.0);
                            let top = row as f32 * stride;
                            let offset = if top < old_offset { top } else if top + stride > old_offset + height { (top + stride - height).max(0.0) } else { old_offset };
                            list = list.vertical_scroll_offset(offset);
                        } else { list = list.vertical_scroll_offset(0.0); }
                        state.scroll_selection = false;
                    }
                    let list_output = list.show_rows(&mut columns[0], ROW_HEIGHT, visible.len().max(1), |ui, range| {
                        if visible.is_empty() {
                            ui.label(if state.scanner.is_scanning() { "Looking for local conversations…" } else if state.catalog.sessions.is_empty() { "No Codex or Claude sessions found. Start a conversation in either CLI, then refresh." } else { "No sessions match. Try another query or clear the filters." });
                            return;
                        }
                        for row in range {
                            let session = &state.catalog.sessions[visible[row]];
                            let selected = state.selected.as_ref().is_some_and(|(p, id)| *p == session.provider && *id == session.id);
                            let live_pids = state.catalog.live_pids(session);
                            let live_badge = if live_pids.is_empty() { String::new() } else { format!("  ·  LIVE {}", live_pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")) };
                            let pinned = state.library_snapshot.is_pinned(session.provider, &session.id);
                            let title = format!("{}{}", if pinned { "★ " } else { "" }, crate::review_text::visible_bounded(&session.title, 160));
                            let text = format!("{}  ·  {}  ·  {}{}\n{}\n{}", session.provider.label(), session.status.label(), age_label(session.updated_at_ms), live_badge, title, crate::review_text::visible_bounded(&session.cwd.to_string_lossy(), 160));
                            let response = ui.push_id((session.provider.label(), &session.id), |ui| ui.add_sized([ui.available_width(), ROW_HEIGHT], egui::Button::selectable(selected, text).wrap())).inner;
                            if response.has_focus() { state.row_focus_widget = Some(response.id); }
                            if response.clicked() { state.selected = Some((session.provider, session.id.clone())); }
                        }
                    });
                    state.remember_anchor(&visible, list_output.state.offset.y, stride);
                    egui::ScrollArea::vertical().id_salt("agent_session_detail").max_height(height).show(&mut columns[1], |ui| {
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        if let Some(session) = state.selected_session() {
                            ui.heading(crate::review_text::visible_bounded(&session.title, 240));
                            ui.label(format!("{} · {} · {}", session.provider.label(), session.status.label(), age_label(session.updated_at_ms)));
                            let pinned = state.library_snapshot.is_pinned(session.provider, &session.id);
                            if ui.add_enabled(state.library.is_loaded() && !state.library.is_busy(), egui::Button::new(if pinned { "★ Pinned · Unpin" } else { "☆ Pin session" })).clicked() {
                                pin_change = Some((session.provider, session.id.clone(), !pinned));
                            }
                            if let Some(resumed) = state.library_snapshot.last_resumed(session.provider, &session.id) {
                                ui.small(format!("Last resumed {} · shared across your terminals", age_label(resumed)));
                            }
                            if state.opened.contains_key(&session_key(session)) {
                                ui.colored_label(egui::Color32::LIGHT_GREEN, "Ready to switch to the open tab");
                            } else if state.preflight.ready_launch(session).is_some() {
                                ui.colored_label(egui::Color32::LIGHT_GREEN, "Ready · project and CLI checked");
                            } else {
                                match state.preflight.state() {
                                    SessionPreflightState::Unavailable(error) => { ui.colored_label(egui::Color32::LIGHT_RED, crate::review_text::visible_bounded(error, 1024)); }
                                    _ => { ui.horizontal(|ui| { ui.spinner(); ui.label("Checking project and CLI…"); }); }
                                }
                            }
                            if state.pending_resume.is_some() { ui.small("Confirming a fresh readiness check…"); }
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
                            if let Some(launch) = state.preflight.ready_launch(session) {
                                ui.label(egui::RichText::new("Resume command").strong());
                                ui.monospace(crate::review_text::visible_bounded(&launch.display_command(), 1024));
                                if ui.button("Copy command").clicked() { ui.ctx().copy_text(launch.display_command()); }
                            }
                        } else { ui.label("Select a conversation to inspect its project and latest context."); }
                        });
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.small("↑ ↓ / PgUp PgDn / Home End select · Ctrl+L search · Enter resume · Esc close");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_resume = state.pending_resume.is_none() && (state.selected.as_ref().is_some_and(|key| state.opened.contains_key(key)) || state.selected_session().is_some_and(|s| state.preflight.ready_launch(s).is_some()));
                        if ui.add_enabled(can_resume, egui::Button::new(if state.selected.as_ref().is_some_and(|key| state.opened.contains_key(key)) { "Switch to Open Tab  ↵" } else { "Resume in New Tab  ↵" })).clicked() { resume = true; }
                    });
                });
                ui.small("States are last recorded events. LIVE badges show exact session-ID process matches; they do not imply busy.");
            });
        if !open {
            self.agent_sessions.is_open = false;
            self.agent_sessions.cancel_resume();
            self.return_focus_to_terminal(ctx);
        }
        if let Some((provider, id, pinned)) = pin_change {
            self.agent_sessions.library_notice = self
                .agent_sessions
                .library
                .set_pinned(provider, &id, pinned)
                .err();
        }
        if resume {
            self.resume_selected_agent_session(ctx);
        }
        self.finish_pending_agent_resume(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::agent_sessions::LiveSessionProcess;

    fn session(provider: SessionProvider, id: &str, title: &str) -> AgentSession {
        AgentSession {
            provider,
            id: id.into(),
            title: title.into(),
            cwd: PathBuf::from("/tmp/project"),
            updated_at_ms: 1,
            status: SessionStatus::Idle,
            preview: "latest response".into(),
            branch: None,
            source: PathBuf::from("/tmp/session.jsonl"),
            archived: false,
        }
    }

    fn state_with(sessions: Vec<AgentSession>) -> AgentSessionsState {
        let mut state = AgentSessionsState::default();
        state.replace_catalog(SessionCatalog {
            sessions,
            ..Default::default()
        });
        state
    }

    #[test]
    fn selection_tracks_provider_and_identity_across_refresh_order() {
        let mut state = state_with(vec![
            session(SessionProvider::Codex, "same-id", "Codex work"),
            session(SessionProvider::Claude, "same-id", "Claude work"),
        ]);
        state.selected = Some((SessionProvider::Claude, "same-id".into()));
        let mut refreshed = state.catalog.clone();
        refreshed.sessions.reverse();
        state.replace_catalog(refreshed);
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
    fn filter_controls_own_their_enter_and_arrow_keys() {
        let search = egui::Id::new("search");
        let row = egui::Id::new("session-row");
        assert!(session_key_route(None, Some(search), Some(row)));
        assert!(session_key_route(Some(search), Some(search), Some(row)));
        assert!(session_key_route(Some(row), Some(search), Some(row)));
        for control in [
            "sort",
            "status",
            "live-only",
            "scope",
            "clear-filters",
            "resume-button",
        ] {
            assert!(!session_key_route(
                Some(egui::Id::new(control)),
                Some(search),
                Some(row)
            ));
        }
    }

    #[test]
    fn observed_live_filter_is_independent_from_recorded_state() {
        let live = session(SessionProvider::Codex, "live-id", "awaiting input");
        let mut stale = session(SessionProvider::Claude, "stale-id", "old task start");
        stale.status = SessionStatus::Working;
        let mut state = AgentSessionsState::default();
        state.replace_catalog(SessionCatalog {
            sessions: vec![live, stale],
            live_processes: vec![LiveSessionProcess {
                provider: SessionProvider::Codex,
                id: "live-id".into(),
                pid: 1234,
            }],
            ..Default::default()
        });
        state.live_only = true;
        state.filters_changed();
        assert_eq!(state.visible(), vec![0]);
        assert_eq!(
            state.selected_session().unwrap().status,
            SessionStatus::Idle
        );
        state.status = Some(SessionStatus::Working);
        state.filters_changed();
        assert!(state.visible().is_empty());
        assert!(state.selected_session().is_none());
        state.status = None;
        let mut next = state.catalog.clone();
        next.live_processes.clear();
        state.replace_catalog(next);
        assert!(state.visible().is_empty());
    }

    #[test]
    fn edited_query_selects_title_match_over_previous_incidental_preview_match() {
        let mut incidental = session(SessionProvider::Claude, "incidental", "Different task");
        incidental.preview = "session navigation notes".into();
        incidental.updated_at_ms = 100;
        let wanted = session(SessionProvider::Codex, "wanted", "session navigation");
        let mut state = state_with(vec![incidental, wanted]);
        assert_eq!(state.selected_session().unwrap().id, "incidental");
        state.query = "session navigation".into();
        state.filters_changed();
        assert_eq!(state.selected_session().unwrap().id, "wanted");
        state.sort = SessionSort::Recent;
        state.filters_changed();
        assert_eq!(state.selected_session().unwrap().id, "incidental");
    }

    #[test]
    fn folder_snapshot_excludes_sibling_prefix_and_survives_refresh() {
        let mut child = session(SessionProvider::Codex, "child", "inside");
        child.cwd = PathBuf::from("/tmp/project/sub");
        let mut sibling = session(SessionProvider::Claude, "sibling", "outside");
        sibling.cwd = PathBuf::from("/tmp/project-other");
        let mut state = state_with(vec![child, sibling]);
        state.scope_cwd = Some(PathBuf::from("/tmp/project"));
        state.project_only = true;
        state.filters_changed();
        assert_eq!(state.visible(), vec![0]);
        let mut next = state.catalog.clone();
        next.sessions[0].preview = "updated".into();
        state.replace_catalog(next);
        assert_eq!(
            state.scope_cwd.as_deref(),
            Some(std::path::Path::new("/tmp/project"))
        );
        assert_eq!(state.visible(), vec![0]);
    }

    #[test]
    fn paging_and_boundaries_navigate_large_catalog_without_wrapping() {
        let sessions = (0..500)
            .map(|i| {
                let mut s = session(SessionProvider::Codex, &format!("session-{i:04}"), "work");
                s.updated_at_ms = 500 - i;
                s
            })
            .collect();
        let mut state = state_with(sessions);
        state.page_rows = 7;
        state.navigate(egui::Key::PageDown);
        assert_eq!(state.selected_session().unwrap().id, "session-0007");
        state.navigate(egui::Key::PageUp);
        assert_eq!(state.selected_session().unwrap().id, "session-0000");
        state.navigate(egui::Key::ArrowUp);
        assert_eq!(state.selected_session().unwrap().id, "session-0000");
        state.navigate(egui::Key::End);
        assert_eq!(state.selected_session().unwrap().id, "session-0499");
        state.navigate(egui::Key::PageDown);
        assert_eq!(state.selected_session().unwrap().id, "session-0499");
        state.navigate(egui::Key::Home);
        assert_eq!(state.selected_session().unwrap().id, "session-0000");
    }

    #[test]
    fn refresh_keeps_scroll_anchor_when_new_session_precedes_it_and_skips_unchanged() {
        let sessions = (0..30)
            .map(|i| {
                let mut s = session(SessionProvider::Codex, &format!("row-{i:02}"), "work");
                s.updated_at_ms = 30 - i;
                s
            })
            .collect();
        let mut state = state_with(sessions);
        let visible = state.visible();
        state.selected = Some((SessionProvider::Codex, "row-14".into()));
        state.remember_anchor(&visible, 10.0 * 74.0 + 3.0, 74.0);
        assert!(!state.replace_catalog(state.catalog.clone()));
        assert!(!state.restore_anchor);
        let mut next = state.catalog.clone();
        let mut newest = session(SessionProvider::Claude, "new", "new arrival");
        newest.updated_at_ms = 100;
        next.sessions.insert(0, newest);
        assert!(state.replace_catalog(next));
        assert_eq!(state.selected_session().unwrap().id, "row-14");
        assert_eq!(
            state.anchor_offset(&state.visible(), 74.0),
            Some(11.0 * 74.0 + 3.0)
        );
        assert!(state.restore_anchor);
    }

    const LIBRARY_A: &str = "11111111-1111-4111-8111-111111111111";
    const LIBRARY_B: &str = "22222222-2222-4222-8222-222222222222";

    fn library(entries: &[(SessionProvider, &str, bool, Option<u64>)]) -> SessionLibrarySnapshot {
        SessionLibrarySnapshot::from_entries(
            entries
                .iter()
                .map(|(provider, id, pinned, resumed)| {
                    jterm_core::agent_sessions::SessionLibraryEntry {
                        provider: *provider,
                        id: (*id).into(),
                        pinned: *pinned,
                        last_resumed_ms: *resumed,
                    }
                })
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn durable_metadata_reprojects_filters_without_losing_visible_selection_or_anchor() {
        let mut state = state_with(vec![
            session(SessionProvider::Codex, LIBRARY_A, "first"),
            session(SessionProvider::Claude, LIBRARY_B, "second"),
        ]);
        state.replace_library(library(&[
            (SessionProvider::Codex, LIBRARY_A, true, None),
            (SessionProvider::Claude, LIBRARY_B, true, None),
        ]));
        state.pinned_only = true;
        state.filters_changed();
        state.selected = Some((SessionProvider::Claude, LIBRARY_B.into()));
        state.remember_anchor(&state.visible(), 74.0, 74.0);
        state.replace_library(library(&[
            (SessionProvider::Codex, LIBRARY_A, false, None),
            (SessionProvider::Claude, LIBRARY_B, true, None),
        ]));
        assert_eq!(state.visible(), vec![1]);
        assert_eq!(state.selected_session().unwrap().id, LIBRARY_B);
        assert_eq!(state.anchor_offset(&state.visible(), 74.0), Some(0.0));
        state.replace_library(library(&[(
            SessionProvider::Claude,
            LIBRARY_B,
            false,
            None,
        )]));
        assert!(state.visible().is_empty());
        assert!(state.selected_session().is_none());
        assert!(
            state.opened.is_empty(),
            "metadata changes never create or resume a terminal"
        );
    }

    #[test]
    fn resumed_history_filter_and_sort_follow_only_recorded_successes() {
        let mut state = state_with(vec![
            session(SessionProvider::Codex, LIBRARY_A, "first"),
            session(SessionProvider::Claude, LIBRARY_B, "second"),
        ]);
        state.recent_only = true;
        state.filters_changed();
        assert!(
            state.visible().is_empty(),
            "viewing a session does not manufacture a resume"
        );
        state.replace_library(library(&[
            (SessionProvider::Codex, LIBRARY_A, false, Some(100)),
            (SessionProvider::Claude, LIBRARY_B, false, Some(200)),
        ]));
        state.sort = SessionSort::RecentlyResumed;
        state.filters_changed();
        assert_eq!(state.selected_session().unwrap().id, LIBRARY_B);
        state.replace_library(library(&[
            (SessionProvider::Codex, LIBRARY_A, false, Some(300)),
            (SessionProvider::Claude, LIBRARY_B, false, Some(200)),
        ]));
        assert_eq!(state.visible(), vec![0, 1]);
        assert_eq!(
            state.selected_session().unwrap().id,
            LIBRARY_B,
            "background metadata reorder preserves the inspected conversation"
        );
    }

    #[test]
    fn pin_unpin_and_resume_history_survive_a_fresh_async_store() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!(
            "ember-session-library-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("library.json");
        let mut state = state_with(vec![session(SessionProvider::Codex, LIBRARY_A, "first")]);
        state.library = SessionLibraryStore::with_path(path.clone());
        fn wait(state: &mut AgentSessionsState, ready: impl Fn(&AgentSessionsState) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                state.poll_library();
                if ready(state) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "store did not settle: {:?}",
                    state.library.error()
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        wait(&mut state, |s| s.library.is_loaded());
        state
            .library
            .set_pinned(SessionProvider::Codex, LIBRARY_A, true)
            .unwrap();
        wait(&mut state, |s| {
            s.library_snapshot
                .is_pinned(SessionProvider::Codex, LIBRARY_A)
        });
        assert!(
            state
                .library_snapshot
                .last_resumed(SessionProvider::Codex, LIBRARY_A)
                .is_none(),
            "pinning never marks a successful resume"
        );
        let selected = state.selected_session().unwrap().clone();
        state.note_resumed(&selected);
        wait(&mut state, |s| {
            s.library_snapshot
                .last_resumed(SessionProvider::Codex, LIBRARY_A)
                .is_some()
        });
        state.library = SessionLibraryStore::with_path(path.clone());
        wait(&mut state, |s| s.library.is_loaded());
        assert!(state
            .library_snapshot
            .is_pinned(SessionProvider::Codex, LIBRARY_A));
        assert!(state
            .library_snapshot
            .last_resumed(SessionProvider::Codex, LIBRARY_A)
            .is_some());
        state
            .library
            .set_pinned(SessionProvider::Codex, LIBRARY_A, false)
            .unwrap();
        wait(&mut state, |s| {
            !s.library_snapshot
                .is_pinned(SessionProvider::Codex, LIBRARY_A)
        });
        assert!(state
            .library_snapshot
            .last_resumed(SessionProvider::Codex, LIBRARY_A)
            .is_some());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pending_confirmation_is_cancelled_by_selection_target_changes_and_close() {
        let mut a = session(SessionProvider::Codex, LIBRARY_A, "first");
        a.cwd = PathBuf::from("/nonexistent-ember-preflight-fixture-a");
        let mut b = session(SessionProvider::Claude, LIBRARY_B, "second");
        b.cwd = PathBuf::from("/nonexistent-ember-preflight-fixture-b");
        let mut state = state_with(vec![a.clone(), b]);
        state.sync_preflight();
        state.preflight.refresh();
        state.pending_resume = Some(a.clone());
        state.selected = Some((SessionProvider::Claude, LIBRARY_B.into()));
        state.sync_preflight();
        assert!(state.pending_resume.is_none());
        state.selected = Some(session_key(&a));
        state.sync_preflight();
        state.pending_resume = Some(a);
        state.cancel_resume();
        assert!(state.pending_resume.is_none());
        assert!(matches!(
            state.preflight.state(),
            SessionPreflightState::Idle
        ));
        assert!(state.opened.is_empty());
        assert!(
            state.library_snapshot.is_empty(),
            "readiness and cancellation never record a successful resume"
        );
    }

    #[test]
    fn clearing_filters_restores_all_sessions_and_clears_stale_error() {
        let mut state = state_with(vec![
            session(SessionProvider::Codex, "one", "first"),
            session(SessionProvider::Claude, "two", "second"),
        ]);
        state.query = "no match".into();
        state.provider = Some(SessionProvider::Claude);
        state.status = Some(SessionStatus::Working);
        state.scope_cwd = Some(PathBuf::from("/elsewhere"));
        state.project_only = true;
        state.live_only = true;
        state.pinned_only = true;
        state.recent_only = true;
        state.sort = SessionSort::LiveFirst;
        state.error = Some("stale launch error".into());
        state.filters_changed();
        assert!(state.selected_session().is_none());
        state.clear_filters();
        assert_eq!(state.visible().len(), 2);
        assert!(state.selected_session().is_some());
        assert_eq!(state.sort, SessionSort::Relevance);
        assert!(!state.project_only);
        assert!(!state.pinned_only);
        assert!(!state.recent_only);
        assert!(state.error.is_none());
        assert!(state.focus_search);
    }
}
