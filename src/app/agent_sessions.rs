//! Discover and resume existing provider-owned CLI conversations.
//! Discovery never runs on the event thread; launch uses argv, not shell text.
use super::state::TerminalApp;
use eframe::egui;
use jterm_core::agent_sessions::{
    AgentSession, SessionArchiveFilter, SessionBatchAction, SessionCatalog, SessionLaunch,
    SessionLibraryFilter, SessionLibrarySnapshot, SessionLibraryStore, SessionPreflight,
    SessionPreflightState, SessionProjectSummary, SessionProvider, SessionQuery, SessionScanner,
    SessionSearchIndex, SessionSort, SessionStatus, SESSION_BATCH_LIMIT,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
    archive: SessionArchiveFilter,
    project: Option<PathBuf>,
    sort: SessionSort,
}

#[derive(Clone, Debug)]
struct ScrollAnchor {
    key: SessionKey,
    within_row: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionDetailsDraft {
    alias: String,
    tags: String,
    original_alias: String,
    original_tags: String,
}

impl SessionDetailsDraft {
    fn saved(library: &SessionLibrarySnapshot, key: &SessionKey) -> Self {
        let alias = library.alias(key.0, &key.1).unwrap_or_default().to_string();
        let tags = library.tags(key.0, &key.1).join(", ");
        Self {
            alias: alias.clone(),
            tags: tags.clone(),
            original_alias: alias,
            original_tags: tags,
        }
    }

    fn is_dirty(&self) -> bool {
        self.alias != self.original_alias || self.tags != self.original_tags
    }

    fn sync_saved(&mut self, saved: &Self) {
        if !self.is_dirty() {
            *self = saved.clone();
        }
    }
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
    archive: SessionArchiveFilter,
    sort: SessionSort,
    scope_cwd: Option<PathBuf>,
    project_only: bool,
    project_filter: Option<PathBuf>,
    projects: Vec<SessionProjectSummary>,
    details_drafts: HashMap<SessionKey, SessionDetailsDraft>,
    pending_details: Option<SessionKey>,
    selected: Option<SessionKey>,
    checked: HashSet<SessionKey>,
    batch_tag: String,
    batch_notice: Option<String>,
    pending_batch: Option<usize>,
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
            archive: self.archive,
            sort: self.sort,
            project: if self.project_only {
                self.scope_cwd.clone()
            } else {
                self.project_filter.clone()
            },
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
                archive: key.archive,
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
        self.projects = self.index.projects(&self.library_snapshot);
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
        self.projects = self.index.projects(&self.library_snapshot);
        self.cached_visible.get_mut().take();
        self.restore_anchor = self.scroll_anchor.is_some();
        self.reconcile_selection();
        true
    }

    fn poll_library(&mut self) {
        if self.library.poll() {
            self.replace_library(self.library.snapshot().clone());
        }
        if !self.library.is_busy() {
            if let Some(key) = self.pending_details.take() {
                if self.library.error().is_none() {
                    self.details_drafts.remove(&key);
                }
            }
            if let Some(count) = self.pending_batch.take() {
                self.batch_notice = Some(if self.library.error().is_none() {
                    format!("Updated {count} selected conversations. Your selection is retained.")
                } else {
                    "The batch could not be saved. Your selection and tag input are retained; review the error and retry.".into()
                });
            }
        }
    }

    fn checked_keys(&self) -> Vec<SessionKey> {
        let mut keys: Vec<_> = self.checked.iter().cloned().collect();
        keys.sort_by(|left, right| {
            left.0
                .label()
                .cmp(right.0.label())
                .then_with(|| left.1.cmp(&right.1))
        });
        keys
    }

    fn select_visible(&mut self, visible: &[usize]) -> Result<(), String> {
        let mut selected = self.checked.clone();
        selected.extend(
            visible
                .iter()
                .map(|index| session_key(&self.catalog.sessions[*index])),
        );
        if selected.len() > SESSION_BATCH_LIMIT {
            return Err(format!("Select up to {SESSION_BATCH_LIMIT} conversations per batch. Narrow the filters or clear the existing selection first."));
        }
        self.checked = selected;
        Ok(())
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
        self.archive = SessionArchiveFilter::Active;
        self.project_only = false;
        self.project_filter = None;
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

struct SessionManagerActions {
    open: bool,
    resume: bool,
    pin_change: Option<(SessionProvider, String, bool)>,
    details_change: Option<(SessionKey, String, String)>,
    archive_change: Option<(SessionKey, bool)>,
    batch_change: Option<(Vec<SessionKey>, SessionBatchAction)>,
    open_tasks: bool,
}

fn session_batch_button(
    ui: &mut egui::Ui,
    keys: &[SessionKey],
    action: &SessionBatchAction,
    enabled: bool,
    label: &str,
) -> egui::Response {
    let meaning = match action {
        SessionBatchAction::Pin(true) => ("pin", ""),
        SessionBatchAction::Pin(false) => ("unpin", ""),
        SessionBatchAction::Archive(true) => ("archive", ""),
        SessionBatchAction::Archive(false) => ("restore", ""),
        SessionBatchAction::AddTag(tag) => ("add_tag", tag.as_str()),
        SessionBatchAction::RemoveTag(tag) => ("remove_tag", tag.as_str()),
    };
    ui.push_id(("session_batch", keys, meaning, enabled), |ui| {
        ui.add_enabled(enabled, egui::Button::new(label))
    })
    .inner
}

fn show_session_batch_controls(
    ui: &mut egui::Ui,
    state: &mut AgentSessionsState,
    visible: &[usize],
) -> Option<(Vec<SessionKey>, SessionBatchAction)> {
    let mut change = None;
    let visible_keys: Vec<_> = visible
        .iter()
        .map(|index| session_key(&state.catalog.sessions[*index]))
        .collect();
    let checked_keys = state.checked_keys();
    ui.horizontal_wrapped(|ui| {
        let visible_checked = visible
            .iter()
            .filter(|index| {
                state
                    .checked
                    .contains(&session_key(&state.catalog.sessions[**index]))
            })
            .count();
        ui.strong(format!(
            "{} selected · {} hidden by filters or refresh",
            state.checked.len(),
            state.checked.len().saturating_sub(visible_checked)
        ));
        if ui
            .push_id(
                ("select_visible_sessions", &visible_keys, &checked_keys),
                |ui| ui.add_enabled(!visible.is_empty(), egui::Button::new("Select visible")),
            )
            .inner
            .clicked()
        {
            state.library_notice = state.select_visible(visible).err();
        }
        if ui
            .push_id(("clear_checked_sessions", &checked_keys), |ui| {
                ui.add_enabled(
                    !state.checked.is_empty(),
                    egui::Button::new("Clear selection"),
                )
            })
            .inner
            .clicked()
        {
            state.checked.clear();
            state.batch_notice = None;
        }
    });
    if !state.checked.is_empty() {
        let keys = state.checked_keys();
        let enabled = state.library.is_loaded() && !state.library.is_busy();
        ui.horizontal_wrapped(|ui| {
            for (label, action) in [
                ("Pin selected", SessionBatchAction::Pin(true)),
                ("Unpin selected", SessionBatchAction::Pin(false)),
                ("Archive selected", SessionBatchAction::Archive(true)),
                ("Restore selected", SessionBatchAction::Archive(false)),
            ] {
                if session_batch_button(ui, &keys, &action, enabled, label).clicked() {
                    change = Some((keys.clone(), action));
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.batch_tag)
                    .id(egui::Id::new("session_batch_tag"))
                    .hint_text("One tag for the selected conversations")
                    .char_limit(32)
                    .desired_width(260.0),
            );
            for (label, action) in [
                (
                    "Add tag",
                    SessionBatchAction::AddTag(state.batch_tag.clone()),
                ),
                (
                    "Remove tag",
                    SessionBatchAction::RemoveTag(state.batch_tag.clone()),
                ),
            ] {
                if session_batch_button(
                    ui,
                    &keys,
                    &action,
                    enabled && !state.batch_tag.trim().is_empty(),
                    label,
                )
                .clicked()
                {
                    change = Some((keys.clone(), action));
                }
            }
        });
        ui.small(format!("Actions apply to all {} checked conversations, including hidden selections. Limit: {SESSION_BATCH_LIMIT}.", keys.len()));
    }
    if let Some(notice) = &state.batch_notice {
        ui.small(notice);
    }
    change
}

fn session_resume_button(
    ui: &mut egui::Ui,
    selected: Option<&SessionKey>,
    enabled: bool,
    switch_to_tab: bool,
) -> egui::Response {
    ui.push_id(("resume_agent_session", selected), |ui| {
        ui.add_enabled(
            enabled,
            egui::Button::new(if switch_to_tab {
                "Switch to Open Tab  ↵"
            } else {
                "Resume in New Tab  ↵"
            }),
        )
    })
    .inner
}

// Shared metadata can reverse an action while a pointer is held. Key both the
// conversation and the displayed state so that release cannot invoke its inverse.
fn session_pin_button(
    ui: &mut egui::Ui,
    key: &SessionKey,
    pinned: bool,
    enabled: bool,
) -> egui::Response {
    ui.push_id(("pin_agent_session", key, pinned), |ui| {
        ui.add_enabled(
            enabled,
            egui::Button::new(if pinned {
                "★ Pinned · Unpin"
            } else {
                "☆ Pin session"
            }),
        )
    })
    .inner
}

fn session_archive_button(
    ui: &mut egui::Ui,
    key: &SessionKey,
    archived: bool,
    enabled: bool,
) -> egui::Response {
    ui.push_id(("archive_agent_session", key, archived), |ui| {
        ui.add_enabled(
            enabled,
            egui::Button::new(if archived {
                "Restore to active"
            } else {
                "Archive locally"
            }),
        )
    })
    .inner
}

fn session_row_button(
    ui: &mut egui::Ui,
    selected: bool,
    status: &str,
    title: &str,
    context: &str,
) -> egui::Response {
    let response = ui.add_sized(
        [ui.available_width(), 70.0],
        egui::Button::selectable(selected, ""),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled(),
            selected,
            format!("{title}. {status}. {context}"),
        )
    });
    let content = response.rect.shrink2(egui::vec2(10.0, 6.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .id(response.id.with("text"))
            .max_rect(content)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(content.intersect(ui.clip_rect()));
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add(egui::Label::new(egui::RichText::new(status).small().weak()).truncate());
            ui.add(egui::Label::new(egui::RichText::new(title).strong()).truncate());
            ui.add(egui::Label::new(egui::RichText::new(context).small().weak()).truncate());
        },
    );
    response
}

// Render pure navigation/editor state and stage exact-identity actions. The
// caller remains responsible for disk mutations, preflight and process launch.
fn show_session_manager(
    ctx: &egui::Context,
    state: &mut AgentSessionsState,
    task_dashboard_enabled: bool,
) -> SessionManagerActions {
    let mut open = true;
    let mut resume = false;
    let mut pin_change = None;
    let mut details_change = None;
    let mut archive_change = None;
    let mut batch_change = None;
    let mut open_tasks = false;
    egui::Window::new("Agent Sessions")
        .id(egui::Id::new("agent_sessions_manager"))
        .open(&mut open)
        .default_size(egui::vec2(1040.0, 700.0))
        .min_size(egui::vec2(640.0, 420.0))
        .collapsible(false)
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Agent command center").size(20.0).strong());
                if ui.add_enabled(task_dashboard_enabled, egui::Button::new("Task dashboard"))
                    .on_disabled_hover_text("Enable the Tasks sidebar in Settings to manage worktree tasks")
                    .clicked() { open_tasks = true; }
            });
            ui.label("Organize conversations, browse projects and return to active work");
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
                let response = ui.add(egui::TextEdit::singleline(&mut state.query).hint_text("Search · tag:review provider:codex project:core -term \"exact phrase\"").desired_width(f32::INFINITY).char_limit(1024));
                state.search_widget = Some(response.id);
                if state.focus_search { response.request_focus(); state.focus_search = false; }
            });
            ui.horizontal_wrapped(|ui| {
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
                egui::ComboBox::from_id_salt("agent_session_archive_filter")
                    .selected_text(state.archive.label())
                    .show_ui(ui, |ui| {
                        for archive in SessionArchiveFilter::ALL {
                            ui.selectable_value(&mut state.archive, archive, archive.label());
                        }
                    });
                if ui.small_button("Clear filters").clicked() { state.clear_filters(); }
            });
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("agent_session_project_filter")
                    .selected_text(state.project_filter.as_ref().filter(|_| !state.project_only)
                        .map_or_else(|| "All projects".to_string(), |root| crate::review_text::visible_bounded(&root.to_string_lossy(), 100)))
                    .show_ui(ui, |ui| {
                        if ui.selectable_value(&mut state.project_filter, None, "All projects").clicked() { state.project_only = false; }
                        for project in &state.projects {
                            let label = format!("{} · {} active · {} archived · {} live · {} waiting", crate::review_text::visible_bounded(&project.root.to_string_lossy(), 240), project.total, project.archived, project.live, project.waiting);
                            if ui.selectable_value(&mut state.project_filter, Some(project.root.clone()), label).clicked() { state.project_only = false; }
                        }
                    });
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
            batch_change = show_session_batch_controls(ui, state, &visible);
            ui.small(format!("{} matching sessions · {}{}", visible.len(), state.sort.label(), if state.query.trim().is_empty() && state.sort == SessionSort::Relevance { " · newest first" } else { "" }));
            if let Some(error) = SessionSearchIndex::query_error(&state.query) {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
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
                        let title = format!("{}{}{}", if pinned { "★ " } else { "" }, if state.library_snapshot.is_archived(session.provider, &session.id) { "[Archived] " } else { "" }, crate::review_text::visible_bounded(state.library_snapshot.display_title(session.provider, &session.id, &session.title), 160));
                        let tags = state.library_snapshot.tags(session.provider, &session.id);
                        let project = crate::review_text::visible_bounded(&session.cwd.to_string_lossy(), 120);
                        let context = if tags.is_empty() { project } else { format!("{}  ·  {}", project, tags.join(" · ")) };
                        let status = format!("{}  ·  {}  ·  {}{}", session.provider.label(), session.status.label(), age_label(session.updated_at_ms), live_badge);
                        let key = session_key(session);
                        let mut checked = state.checked.contains(&key);
                        let response = ui.push_id((session.provider.label(), &session.id), |ui| {
                            ui.horizontal(|ui| {
                                if ui.add_enabled(checked || state.checked.len() < SESSION_BATCH_LIMIT,
                                    egui::Checkbox::without_text(&mut checked))
                                    .on_hover_text("Include this conversation in batch actions")
                                    .on_disabled_hover_text(format!("A batch can contain at most {SESSION_BATCH_LIMIT} conversations"))
                                    .changed() {
                                    if checked { state.checked.insert(key.clone()); }
                                    else { state.checked.remove(&key); }
                                }
                                session_row_button(ui, selected, &status, &title, &context)
                            }).inner
                        }).inner;
                        if response.has_focus() { state.row_focus_widget = Some(response.id); }
                        if response.clicked() { state.selected = Some((session.provider, session.id.clone())); }
                    }
                });
                state.remember_anchor(&visible, list_output.state.offset.y, stride);
                egui::ScrollArea::vertical().id_salt("agent_session_detail").max_height(height).show(&mut columns[1], |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    if let Some(session) = state.selected_session().cloned() {
                        ui.heading(crate::review_text::visible_bounded(state.library_snapshot.display_title(session.provider, &session.id, &session.title), 240));
                        ui.label(format!("{} · {} · {}", session.provider.label(), session.status.label(), age_label(session.updated_at_ms)));
                        let pinned = state.library_snapshot.is_pinned(session.provider, &session.id);
                        if session_pin_button(ui, &session_key(&session), pinned, state.library.is_loaded() && !state.library.is_busy()).clicked() {
                            pin_change = Some((session.provider, session.id.clone(), !pinned));
                        }
                        if let Some(resumed) = state.library_snapshot.last_resumed(session.provider, &session.id) {
                            ui.small(format!("Last resumed {} · shared across your terminals", age_label(resumed)));
                        }
                        if state.opened.contains_key(&session_key(&session)) {
                            ui.colored_label(egui::Color32::LIGHT_GREEN, "Ready to switch to the open tab");
                        } else if state.preflight.ready_launch(&session).is_some() {
                            ui.colored_label(egui::Color32::LIGHT_GREEN, "Ready · project and CLI checked");
                        } else {
                            match state.preflight.state() {
                                SessionPreflightState::Unavailable(error) => { ui.colored_label(egui::Color32::LIGHT_RED, crate::review_text::visible_bounded(error, 1024)); }
                                _ => { ui.horizontal(|ui| { ui.spinner(); ui.label("Checking project and CLI…"); }); }
                            }
                        }
                        if state.pending_resume.is_some() { ui.small("Confirming a fresh readiness check…"); }
                        let live_pids = state.catalog.live_pids(&session);
                        if !live_pids.is_empty() {
                            ui.colored_label(egui::Color32::LIGHT_GREEN, format!("Live CLI process · PID {}", live_pids.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")));
                            ui.small("Exact session-ID process match; recorded state remains separate.");
                        }
                        if state.opened.contains_key(&(session.provider, session.id.clone())) {
                            ui.label("Open in this window · confirming switches to its tab");
                        }
                        ui.add_space(8.0);
                        let key = session_key(&session);
                        let saved = SessionDetailsDraft::saved(&state.library_snapshot, &key);
                        let can_edit = state.library.is_loaded() && state.pending_details.as_ref() != Some(&key);
                        let can_save = state.library.is_loaded() && !state.library.is_busy();
                        let draft = state.details_drafts.entry(key.clone()).or_insert_with(|| saved.clone());
                        draft.sync_saved(&saved);
                        egui::CollapsingHeader::new("Organize this conversation")
                            .id_salt(("session_details", session.provider.label(), &session.id))
                            .default_open(true)
                            .show(ui, |ui| {
                                ui.small("Local alias");
                                ui.add_enabled(can_edit, egui::TextEdit::singleline(&mut draft.alias)
                                    .id_salt(("session_alias", session.provider.label(), &session.id))
                                    .hint_text("A recognizable name for this work")
                                    .char_limit(160).desired_width(f32::INFINITY));
                                ui.small("Tags · comma separated");
                                ui.add_enabled(can_edit, egui::TextEdit::singleline(&mut draft.tags)
                                    .id_salt(("session_tags", session.provider.label(), &session.id))
                                    .hint_text("review, release, research")
                                    .char_limit(420).desired_width(f32::INFINITY));
                                let changed = draft.alias != saved.alias || draft.tags != saved.tags;
                                ui.horizontal_wrapped(|ui| {
                                    if ui.add_enabled(can_save && changed, egui::Button::new("Save details")).clicked() {
                                        details_change = Some((key.clone(), draft.alias.clone(), draft.tags.clone()));
                                    }
                                    if ui.add_enabled(can_edit && changed, egui::Button::new("Reset")).clicked() {
                                        *draft = saved.clone();
                                    }
                                    if state.pending_details.as_ref() == Some(&key) { ui.small("Saving details…"); }
                                    else if !state.library.is_loaded() { ui.small("Loading organization…"); }
                                    else if changed { ui.small("Unsaved changes"); }
                                });
                                if draft.is_dirty() && (draft.original_alias != saved.alias || draft.original_tags != saved.tags) {
                                    ui.colored_label(egui::Color32::YELLOW, "Saved details changed in another terminal. Reset to use those details, or save your edits.");
                                }
                                ui.small("Alias and tags are shared across your four terminals. The provider's conversation stays intact.");
                                if state.library_snapshot.alias(session.provider, &session.id).is_some() {
                                    ui.small(format!("Original title: {}", crate::review_text::visible_bounded(&session.title, 240)));
                                }
                                let archived = state.library_snapshot.is_archived(session.provider, &session.id);
                                if session_archive_button(ui, &key, archived, can_save)
                                    .on_hover_text("Moves this conversation between Active and Archived in the shared library. It does not delete history or stop a running agent.")
                                    .clicked() { archive_change = Some((key.clone(), !archived)); }
                            });
                        if !draft.is_dirty() {
                            state.details_drafts.remove(&key);
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
                        if let Some(launch) = state.preflight.ready_launch(&session) {
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
                    let switch_to_tab = state.selected.as_ref().is_some_and(|key| state.opened.contains_key(key));
                    if session_resume_button(ui, state.selected.as_ref(), can_resume, switch_to_tab).clicked() { resume = true; }
                });
            });
            ui.small("States are last recorded events. LIVE badges show exact session-ID process matches; they do not imply busy.");
        });
    SessionManagerActions {
        open,
        resume,
        pin_change,
        details_change,
        archive_change,
        batch_change,
        open_tasks,
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
            crate::review_text::visible_bounded(
                self.agent_sessions.library_snapshot.display_title(
                    session.provider,
                    &session.id,
                    &session.title
                ),
                96
            )
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
        let task_dashboard_enabled = self.config.experimental_task_sidebar;
        let state = &mut self.agent_sessions;
        state.poll_library();
        if state
            .library_refresh
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(5))
            && state.library.refresh()
        {
            state.library_refresh = Some(Instant::now());
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
        let SessionManagerActions {
            open,
            mut resume,
            pin_change,
            details_change,
            archive_change,
            batch_change,
            open_tasks,
        } = show_session_manager(ctx, state, task_dashboard_enabled);
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
        if let Some((key, alias, tags)) = details_change {
            self.agent_sessions.pending_resume = None;
            match self
                .agent_sessions
                .library
                .set_details(key.0, &key.1, &alias, &tags)
            {
                Ok(()) => {
                    self.agent_sessions.library_notice = None;
                    self.agent_sessions.pending_details = Some(key);
                }
                Err(error) => self.agent_sessions.library_notice = Some(error),
            }
        }
        if let Some((key, archived)) = archive_change {
            self.agent_sessions.pending_resume = None;
            resume = false;
            self.agent_sessions.library_notice = self
                .agent_sessions
                .library
                .set_archived(key.0, &key.1, archived)
                .err();
        }
        if let Some((keys, action)) = batch_change {
            self.agent_sessions.pending_resume = None;
            resume = false;
            match self.agent_sessions.library.apply_batch(&keys, action) {
                Ok(count) => {
                    self.agent_sessions.library_notice = None;
                    self.agent_sessions.batch_notice =
                        Some(format!("Saving changes to {count} selected conversations…"));
                    self.agent_sessions.pending_batch = Some(count);
                }
                Err(error) => self.agent_sessions.library_notice = Some(error),
            }
        }
        if open_tasks {
            self.agent_sessions.is_open = false;
            self.agent_sessions.cancel_resume();
            self.sidebar.note_files_user_intent();
            self.sidebar.visible = true;
            self.sidebar.view = crate::sidebar::SidebarView::Tasks;
            self.config.sidebar_view = crate::sidebar::SidebarView::Tasks;
            self.schedule_config_save();
            return;
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

    fn load_private_library(state: &mut AgentSessionsState) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let directory =
            std::env::temp_dir().join(format!("ember-batch-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        state.library = SessionLibraryStore::with_path(directory.join("library.json"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !state.library.is_loaded() {
            state.poll_library();
            assert!(
                Instant::now() < deadline,
                "library fixture failed to load: {:?}",
                state.library.error()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        directory
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
        let key = (SessionProvider::Codex, LIBRARY_A.to_string());
        let mut draft = SessionDetailsDraft::saved(&state.library_snapshot, &key);
        draft.alias = "Release review".into();
        draft.tags = "review, release".into();
        state.details_drafts.insert(key.clone(), draft);
        state
            .library
            .set_details(key.0, &key.1, "Release review", "review, release")
            .unwrap();
        state.pending_details = Some(key.clone());
        wait(&mut state, |s| {
            s.library_snapshot.alias(key.0, &key.1) == Some("Release review")
                && s.pending_details.is_none()
        });
        assert!(
            !state.details_drafts.contains_key(&key),
            "only a completed save clears its own draft"
        );
        state.library.set_archived(key.0, &key.1, true).unwrap();
        wait(&mut state, |s| {
            s.library_snapshot.is_archived(key.0, &key.1)
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
        assert_eq!(
            state.library_snapshot.alias(key.0, &key.1),
            Some("Release review")
        );
        assert_eq!(
            state.library_snapshot.tags(key.0, &key.1),
            &["release", "review"]
        );
        assert!(state.library_snapshot.is_archived(key.0, &key.1));
        assert!(state.visible().is_empty());
        state.library.set_archived(key.0, &key.1, false).unwrap();
        wait(&mut state, |s| {
            !s.library_snapshot.is_archived(key.0, &key.1)
        });
        assert_eq!(state.visible(), vec![0]);
        assert!(state.opened.is_empty());
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
        state.archive = SessionArchiveFilter::Archived;
        state.project_filter = Some(PathBuf::from("/missing-project"));
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
        assert_eq!(state.archive, SessionArchiveFilter::Active);
        assert!(state.project_filter.is_none());
        assert!(state.error.is_none());
        assert!(state.focus_search);
    }

    #[test]
    fn organization_search_archive_and_project_filters_preserve_draft_identity() {
        use jterm_core::agent_sessions::SessionOrganization;
        let mut a = session(SessionProvider::Codex, LIBRARY_A, "original title");
        a.cwd = "/work/core/sub".into();
        let mut b = session(SessionProvider::Claude, LIBRARY_B, "another conversation");
        b.cwd = "/work/core-other".into();
        let mut state = state_with(vec![a, b]);
        let snapshot = SessionLibrarySnapshot::default()
            .with_organization(
                SessionProvider::Codex,
                LIBRARY_A,
                SessionOrganization {
                    alias: Some("Release review".into()),
                    tags: vec!["review".into()],
                    archived: false,
                },
            )
            .unwrap()
            .with_organization(
                SessionProvider::Claude,
                LIBRARY_B,
                SessionOrganization {
                    archived: true,
                    ..Default::default()
                },
            )
            .unwrap();
        state.replace_library(snapshot);
        state.query = "tag:review provider:codex".into();
        state.project_filter = Some("/work/core".into());
        state.filters_changed();
        assert_eq!(state.visible(), vec![0]);
        let key = (SessionProvider::Codex, LIBRARY_A.to_string());
        let mut draft = SessionDetailsDraft::saved(&state.library_snapshot, &key);
        draft.alias = "Unsaved release plan".into();
        state.details_drafts.insert(key.clone(), draft);
        state.query.clear();
        state.project_filter = None;
        state.archive = SessionArchiveFilter::Archived;
        state.filters_changed();
        assert_eq!(state.visible(), vec![1]);
        assert_eq!(state.selected_session().unwrap().id, LIBRARY_B);
        assert_eq!(state.details_drafts[&key].alias, "Unsaved release plan");
        state.archive = SessionArchiveFilter::All;
        state.query = "\"Release review\"".into();
        state.filters_changed();
        assert_eq!(state.visible(), vec![0]);
        assert!(state.pending_resume.is_none());
        assert!(
            state.opened.is_empty(),
            "organizing never launches an agent"
        );
        assert_eq!(state.catalog.sessions[0].title, "original title");
    }

    #[test]
    fn organizer_refresh_updates_clean_drafts_and_preserves_unsaved_edits() {
        use jterm_core::agent_sessions::SessionOrganization;
        let key = (SessionProvider::Codex, LIBRARY_A.to_string());
        let empty = SessionLibrarySnapshot::default();
        let saved = empty
            .clone()
            .with_organization(
                key.0,
                &key.1,
                SessionOrganization {
                    alias: Some("Saved elsewhere".into()),
                    tags: vec!["release".into()],
                    archived: false,
                },
            )
            .unwrap();
        let next = SessionDetailsDraft::saved(&saved, &key);
        let mut clean = SessionDetailsDraft::saved(&empty, &key);
        clean.sync_saved(&next);
        assert_eq!(clean.alias, "Saved elsewhere");
        assert!(!clean.is_dirty());
        let mut dirty = SessionDetailsDraft::saved(&empty, &key);
        dirty.alias = "My unsaved draft".into();
        dirty.sync_saved(&next);
        assert_eq!(dirty.alias, "My unsaved draft");
        assert!(dirty.is_dirty());
        assert_ne!(dirty.original_alias, next.alias);
        let editor_id = egui::Id::new(("session_alias", "Codex", LIBRARY_A));
        assert!(
            !session_key_route(Some(editor_id), Some(egui::Id::new("search")), None),
            "Enter in the details editor must never resume a conversation"
        );
    }

    #[test]
    fn organizer_window_layout_is_navigation_only_at_wide_and_narrow_sizes() {
        use jterm_core::agent_sessions::SessionOrganization;
        let destination = std::env::var_os("EMBER_AGENT_UI_SNAPSHOT_DIR").map(PathBuf::from);
        if let Some(path) = &destination {
            std::fs::create_dir_all(path).unwrap();
        }
        for (name, width, height) in [
            ("agent-manager-wide", 1280, 900),
            ("agent-manager-narrow", 800, 650),
        ] {
            let ctx = egui::Context::default();
            crate::apply_theme_visuals(&ctx, &crate::theme::Theme::default());
            let mut state = state_with(vec![
                session(
                    SessionProvider::Codex,
                    LIBRARY_A,
                    "Review the shared terminal agent manager",
                ),
                session(
                    SessionProvider::Claude,
                    LIBRARY_B,
                    "Explore the terminal workspace",
                ),
            ]);
            state.scope_cwd = Some("/tmp/project".into());
            let library_directory = load_private_library(&mut state);
            state.replace_library(
                SessionLibrarySnapshot::default()
                    .with_organization(
                        SessionProvider::Codex,
                        LIBRARY_A,
                        SessionOrganization {
                            alias: Some("Release review — four terminal Agent management".into()),
                            tags: vec!["release".into(), "review".into(), "terminal".into()],
                            archived: false,
                        },
                    )
                    .unwrap(),
            );
            state.selected = Some((SessionProvider::Codex, LIBRARY_A.into()));
            state.checked.extend([
                (SessionProvider::Codex, LIBRARY_A.into()),
                (SessionProvider::Claude, LIBRARY_B.into()),
            ]);
            state.batch_tag = "review".into();
            let mut capture = super::super::visual_test_support::OffscreenCapture::default();
            for pass in 0..3 {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        time: Some(pass as f64 * 0.25),
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width as f32, height as f32),
                        )),
                        ..Default::default()
                    },
                    |_ui| {
                        let action = show_session_manager(&ctx, &mut state, true);
                        assert!(action.open);
                        assert!(!action.resume && !action.open_tasks);
                        assert!(
                            action.details_change.is_none()
                                && action.archive_change.is_none()
                                && action.pin_change.is_none()
                                && action.batch_change.is_none()
                        );
                    },
                );
                if let Some(path) = &destination {
                    capture.save(
                        &ctx,
                        &mut output,
                        [width, height],
                        &path.join(format!("{name}.png")),
                    );
                } else {
                    output.textures_delta.clear();
                }
            }
            assert!(state.opened.is_empty());
            assert!(state.pending_resume.is_none());
            assert_eq!(state.selected_session().unwrap().id, LIBRARY_A);
            drop(state);
            std::fs::remove_dir_all(library_directory).unwrap();
        }
    }

    #[test]
    fn batch_selection_survives_filters_and_refuses_partial_select_visible() {
        let mut state = state_with(vec![
            session(SessionProvider::Codex, LIBRARY_A, "first"),
            session(
                SessionProvider::Claude,
                LIBRARY_A,
                "second provider, same id",
            ),
        ]);
        state.select_visible(&state.visible()).unwrap();
        assert_eq!(state.checked.len(), 2);
        state.provider = Some(SessionProvider::Claude);
        state.filters_changed();
        assert_eq!(state.visible().len(), 1);
        assert_eq!(
            state.checked.len(),
            2,
            "hidden checked identities remain explicit batch targets"
        );
        let original = state.checked_keys();
        state.replace_catalog(SessionCatalog {
            sessions: (0..=SESSION_BATCH_LIMIT)
                .map(|index| {
                    session(
                        SessionProvider::Codex,
                        &format!("00000000-0000-4000-8000-{index:012x}"),
                        "large catalog",
                    )
                })
                .collect(),
            ..Default::default()
        });
        state.provider = None;
        state.filters_changed();
        assert!(state.select_visible(&state.visible()).is_err());
        assert_eq!(
            state.checked_keys(),
            original,
            "limit failure must not leave a partial selection"
        );
    }

    #[test]
    fn batch_pointer_release_cannot_retarget_selection_action_or_tag() {
        let first = vec![(SessionProvider::Codex, LIBRARY_A.to_string())];
        let replacement = vec![(SessionProvider::Claude, LIBRARY_A.to_string())];
        let original = SessionBatchAction::AddTag("review".into());
        for (keys, action) in [
            (replacement, original.clone()),
            (
                first.clone(),
                SessionBatchAction::RemoveTag("review".into()),
            ),
            (first.clone(), SessionBatchAction::AddTag("release".into())),
        ] {
            let ctx = egui::Context::default();
            let frame = |keys: &[SessionKey], action: &SessionBatchAction, events| {
                let mut response = None;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(600.0, 300.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        response = Some(session_batch_button(
                            ui,
                            keys,
                            action,
                            true,
                            "Apply to selection",
                        ));
                    },
                );
                output.textures_delta.clear();
                response.unwrap()
            };
            let position = frame(&first, &original, vec![]).rect.center();
            let pointer = |pressed| {
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            };
            assert!(!frame(&first, &original, pointer(true)).clicked());
            assert!(!frame(&keys, &action, pointer(false)).clicked());
            assert!(!frame(&keys, &action, pointer(true)).clicked());
            assert!(frame(&keys, &action, pointer(false)).clicked());
        }
    }

    #[test]
    fn batch_tag_enter_is_an_editor_event_without_resume_or_apply() {
        let ctx = egui::Context::default();
        let mut state = state_with(vec![session(SessionProvider::Codex, LIBRARY_A, "selected")]);
        let library_directory = load_private_library(&mut state);
        state
            .checked
            .insert((SessionProvider::Codex, LIBRARY_A.into()));
        state.batch_tag = "review".into();
        let tag_id = egui::Id::new("session_batch_tag");
        for pressed in [false, true] {
            ctx.memory_mut(|memory| memory.request_focus(tag_id));
            assert!(!session_key_route(
                ctx.memory(|memory| memory.focused()),
                Some(egui::Id::new("search")),
                None
            ));
            let visible = state.visible();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events: if pressed {
                        vec![egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        }]
                    } else {
                        Vec::new()
                    },
                    ..Default::default()
                },
                |ui| {
                    assert!(show_session_batch_controls(ui, &mut state, &visible).is_none());
                },
            );
            output.textures_delta.clear();
        }
        assert!(state.pending_resume.is_none());
        assert_eq!(state.checked.len(), 1);
        assert_eq!(state.batch_tag, "review");
        drop(state);
        std::fs::remove_dir_all(library_directory).unwrap();
    }

    #[test]
    fn resume_pointer_release_cannot_retarget_a_refreshed_selection() {
        let ctx = egui::Context::default();
        let first = (SessionProvider::Codex, LIBRARY_A.to_string());
        let replacement = (SessionProvider::Claude, LIBRARY_B.to_string());
        let frame = |selected: &SessionKey, events| {
            let mut response = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 300.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    response = Some(session_resume_button(ui, Some(selected), true, false));
                },
            );
            output.textures_delta.clear();
            response.unwrap()
        };
        let position = frame(&first, vec![]).rect.center();
        let pointer = |pressed| {
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        assert!(!frame(&first, pointer(true)).clicked());
        assert!(!frame(&replacement, pointer(false)).clicked(),
            "a catalog/library refresh must not transfer a pressed Resume control to another session");
        assert!(!frame(&replacement, pointer(true)).clicked());
        assert!(frame(&replacement, pointer(false)).clicked());
    }

    #[test]
    fn organization_pointer_release_cannot_invert_a_peer_metadata_update() {
        use jterm_core::agent_sessions::SessionOrganization;
        for archive in [false, true] {
            for initial in [false, true] {
                let ctx = egui::Context::default();
                let key = (SessionProvider::Codex, LIBRARY_A.to_string());
                let snapshot = |value| {
                    library(&[(key.0, &key.1, !archive && value, None)])
                        .with_organization(
                            key.0,
                            &key.1,
                            SessionOrganization {
                                archived: archive && value,
                                ..Default::default()
                            },
                        )
                        .unwrap()
                };
                let mut state = state_with(vec![session(key.0, &key.1, "Shared conversation")]);
                state.archive = SessionArchiveFilter::All;
                state.replace_library(snapshot(initial));
                let frame = |state: &AgentSessionsState, events| {
                    let current = if archive {
                        state.library_snapshot.is_archived(key.0, &key.1)
                    } else {
                        state.library_snapshot.is_pinned(key.0, &key.1)
                    };
                    let mut response = None;
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(600.0, 300.0),
                            )),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            response = Some(if archive {
                                session_archive_button(ui, &key, current, true)
                            } else {
                                session_pin_button(ui, &key, current, true)
                            });
                        },
                    );
                    output.textures_delta.clear();
                    let response = response.unwrap();
                    let mutation = response.clicked().then_some(!current);
                    (response, mutation)
                };
                let original = frame(&state, vec![]).0;
                let position = original.rect.left_center() + egui::vec2(8.0, 0.0);
                let pointer = |pressed| {
                    vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]
                };
                assert!(frame(&state, pointer(true)).1.is_none());
                state.replace_library(snapshot(!initial));
                assert_eq!(state.selected.as_ref(), Some(&key));
                let (updated, mutation) = frame(&state, pointer(false));
                assert!(updated.rect.contains(position));
                assert_ne!(original.id, updated.id);
                assert!(
                    mutation.is_none(),
                    "release must not undo a peer metadata update"
                );
                assert!(frame(&state, pointer(true)).1.is_none());
                assert_eq!(frame(&state, pointer(false)).1, Some(initial));
            }
        }
    }
}
