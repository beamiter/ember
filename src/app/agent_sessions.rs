//! Discover and resume existing provider-owned CLI conversations.
//! Discovery never runs on the event thread; launch uses argv, not shell text.
use super::state::TerminalApp;
use eframe::egui;
use jterm_core::agent_sessions::{
    validate_session_batch_tag, validate_session_details, AgentSession, SessionArchiveFilter,
    SessionBatchAction, SessionCatalog, SessionLaunch, SessionLibraryFilter,
    SessionLibrarySnapshot, SessionLibraryStore, SessionPreflight, SessionPreflightState,
    SessionProjectSummary, SessionProvider, SessionQuery, SessionScanner, SessionSearchIndex,
    SessionSort, SessionStatus, SESSION_ALIAS_MAX_BYTES, SESSION_BATCH_LIMIT,
    SESSION_QUERY_MAX_BYTES, SESSION_RESUME_ANYWAY_DELAY, SESSION_TAG_LIMIT, SESSION_TAG_MAX_BYTES,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type SessionKey = (SessionProvider, String);
type OpenSessionKey = (SessionProvider, String, PathBuf);

/// While open, rescan once the previous scan has been idle this long, as the
/// other three terminals do. The shared library reloads in the same step, so
/// other terminals' pins, names and archive changes arrive with the catalog
/// rather than on a second timer.
const SESSION_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// Raw tag-editor budget accepted by `validate_session_details`: every tag at
/// its byte limit plus the ", " separators the editor displays between them.
const SESSION_TAGS_EDITOR_MAX_BYTES: usize = (SESSION_TAG_MAX_BYTES + 2) * SESSION_TAG_LIMIT;
/// How long an inline "Copied …" receipt stays beside its button.
const COPY_RECEIPT: Duration = Duration::from_secs(3);

/// egui bounds an editor in Unicode scalar values, while the shared library
/// validates UTF-8 bytes. One scalar above the byte budget never truncates
/// valid input, and a truncated paste stays over budget, so the core explains
/// the limit instead of a paste silently losing its final terms or tags.
const fn editor_char_limit(max_bytes: usize) -> usize {
    max_bytes + 1
}

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

/// One projection of the catalog for the current filters. Every per-frame
/// reader shares the rows; the fingerprint binds gestures to exactly the
/// identities shown, and the query explanation is parsed once per edit.
struct VisibleRows {
    key: FilterKey,
    rows: Rc<[usize]>,
    fingerprint: u64,
    query_error: Option<String>,
}

/// Overview counts, computed once per catalog rather than on every frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CatalogStats {
    total: usize,
    codex: usize,
    waiting: usize,
    /// Conversations with an exact session-ID match to at least one local CLI
    /// process. Several processes holding one conversation count once.
    live_conversations: usize,
}

impl CatalogStats {
    fn of(catalog: &SessionCatalog) -> Self {
        let live: HashSet<(SessionProvider, &str)> = catalog
            .live_processes
            .iter()
            .map(|process| (process.provider, process.id.as_str()))
            .collect();
        let mut stats = Self {
            total: catalog.sessions.len(),
            ..Self::default()
        };
        for session in &catalog.sessions {
            stats.codex += usize::from(session.provider == SessionProvider::Codex);
            stats.waiting += usize::from(session.status == SessionStatus::Waiting);
            stats.live_conversations +=
                usize::from(live.contains(&(session.provider, session.id.as_str())));
        }
        stats
    }
}

/// Resume is a two-step action while another CLI is observed holding the
/// conversation. Both steps are bound to one provider/session identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum ResumeGate {
    #[default]
    Clear,
    /// The first Resume showed the live-process warning at this instant. Only
    /// "Resume anyway" for exactly this identity may continue, and not before
    /// [`SESSION_RESUME_ANYWAY_DELAY`] has passed.
    Armed(SessionKey, Instant),
    /// The user chose "Resume anyway" for exactly this identity. It covers
    /// only the pending attempt it started.
    Confirmed(SessionKey),
}

/// What a pending resume needs next.
#[derive(Debug)]
enum ResumeStep {
    /// Nothing is pending, or its fresh readiness check is still running.
    Wait,
    /// The fresh check passed and nothing new blocks the launch.
    Launch(Box<(AgentSession, SessionLaunch)>),
    /// A scan observed a live CLI after Resume was chosen; warn first.
    Rearmed,
}

/// The user's own organization change to the selected conversation. When it
/// moves the conversation out of the view, the detached notice says so and
/// names the view that finds it, instead of blaming a refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnChange {
    Archived,
    Restored,
    Unpinned,
    Details,
}

impl OwnChange {
    fn of_batch(action: &SessionBatchAction) -> Option<Self> {
        match action {
            SessionBatchAction::Archive(true) => Some(Self::Archived),
            SessionBatchAction::Archive(false) => Some(Self::Restored),
            SessionBatchAction::Pin(false) => Some(Self::Unpinned),
            SessionBatchAction::Pin(true) => None,
            SessionBatchAction::AddTag(_) | SessionBatchAction::RemoveTag(_) => Some(Self::Details),
        }
    }

    fn explanation(self, title: &str) -> String {
        match self {
            Self::Archived => format!(
                "You archived “{title}”, so it left this view. Choose Archived here or All conversations to find or restore it."
            ),
            Self::Restored => format!(
                "You restored “{title}” to the Active library, so it left this view. Choose Active library or All conversations to find it."
            ),
            Self::Unpinned => format!(
                "You unpinned “{title}”, so it left this view. Turn off Pinned only to find it."
            ),
            Self::Details => format!(
                "Your new name or tags for “{title}” no longer match this search or these filters, so it left this view. Edit the search or choose Clear filters to find it."
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ResumeButtonMode {
    NewTab,
    SwitchToTab,
    ResumeAnyway,
}

impl ResumeButtonMode {
    fn label(self) -> &'static str {
        match self {
            Self::NewTab => "Resume in New Tab  ↵",
            Self::SwitchToTab => "Switch to Open Tab  ↵",
            Self::ResumeAnyway => "Resume anyway  ↵",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyKind {
    Command,
    Path,
}

impl CopyKind {
    fn receipt(self) -> &'static str {
        match self {
            Self::Command => "Copied the resume command",
            Self::Path => "Copied the project path",
        }
    }
}

#[derive(Clone, Debug)]
struct CopyReceipt {
    key: SessionKey,
    kind: CopyKind,
    at: Instant,
}

/// Escape is layered: leave a text editor, then dismiss a resume warning,
/// and only then close the manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionEscape {
    LeaveEditor,
    CancelConfirmation,
    Close,
}

fn session_escape_action(editor_focused: bool, confirmation_armed: bool) -> SessionEscape {
    if editor_focused {
        SessionEscape::LeaveEditor
    } else if confirmation_armed {
        SessionEscape::CancelConfirmation
    } else {
        SessionEscape::Close
    }
}

/// Whether an Enter just raised a live-process warning (for any identity).
/// Later Enter presses in the same key batch must not confirm a warning the
/// user has not yet seen.
fn enter_raised_warning(before: &ResumeGate, after: &ResumeGate) -> bool {
    matches!(after, ResumeGate::Armed(..)) && after != before
}

/// One batch tag, checked live with the rules `apply_batch` enforces. A batch
/// adds or removes a single tag, so the core rejects a comma rather than
/// splitting on it. An empty field disables the buttons without an error.
fn batch_tag_problem(tag: &str) -> Option<String> {
    if tag.trim().is_empty() {
        None
    } else {
        validate_session_batch_tag(tag).err()
    }
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
        let facts = library.facts(key.0, &key.1);
        let alias = facts.alias.unwrap_or_default().to_string();
        let tags = facts.tags.join(", ");
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
    /// Sidebar activation is nonmodal. Only a conflict opens the full manager.
    workspace_pending: bool,
    pub(crate) workspace_task: Option<jterm_core::agent_task::TaskId>,
    catalog_epoch: u64,
    workspace: crate::workspace_sessions_bridge::WorkspaceSessions,
    workspace_source: Option<OpenSessionKey>,
    scanner: SessionScanner,
    library: SessionLibraryStore,
    library_snapshot: SessionLibrarySnapshot,
    library_notice: Option<String>,
    preflight: SessionPreflight,
    pending_resume: Option<AgentSession>,
    resume_gate: ResumeGate,
    catalog: SessionCatalog,
    catalog_stats: CatalogStats,
    index: SessionSearchIndex,
    cached_visible: RefCell<Option<VisibleRows>>,
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
    bring_to_front: bool,
    search_widget: Option<egui::Id>,
    row_focus_widget: Option<egui::Id>,
    /// Text editors rendered in the previous frame. Escape leaves them first.
    text_editors: Vec<egui::Id>,
    scroll_selection: bool,
    scroll_anchor: Option<ScrollAnchor>,
    restore_anchor: bool,
    page_rows: usize,
    copy_receipt: Option<CopyReceipt>,
    /// The latest organization change the user made to the selection.
    own_change: Option<(SessionKey, OwnChange)>,
    error: Option<String>,
    opened: HashMap<OpenSessionKey, String>,
}

impl AgentSessionsState {
    fn workspace_visible(&self) -> Vec<usize> {
        if SessionSearchIndex::query_error(&self.query).is_some() {
            return Vec::new();
        }
        let mut result: Vec<_> = self
            .catalog
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| {
                let row = self.workspace.row(session);
                let archived = session.archived || row.is_some_and(|row| row.archived);
                let archive = match self.archive {
                    SessionArchiveFilter::Active => !archived,
                    SessionArchiveFilter::Archived => archived,
                    SessionArchiveFilter::All => true,
                };
                archive
                    && self
                        .provider
                        .is_none_or(|provider| provider == session.provider)
                    && self.status.is_none_or(|status| status == session.status)
                    && (!self.live_only
                        || self
                            .workspace
                            .row(session)
                            .is_some_and(|row| row.runtime.is_live())
                        || self
                            .catalog
                            .live_processes
                            .iter()
                            .any(|p| p.provider == session.provider && p.id == session.id))
                    && (!self.pinned_only || self.workspace.pinned(session))
                    && self
                        .filter_project()
                        .is_none_or(|project| session.cwd.starts_with(project))
                    && row.map_or_else(
                        || session.matches_query(&self.query),
                        |row| row.matches(&self.query),
                    )
            })
            .map(|(index, _)| index)
            .collect();
        result.sort_by(|&a, &b| {
            self.workspace
                .pinned(&self.catalog.sessions[b])
                .cmp(&self.workspace.pinned(&self.catalog.sessions[a]))
                .then(
                    self.catalog.sessions[b]
                        .updated_at_ms
                        .cmp(&self.catalog.sessions[a].updated_at_ms),
                )
        });
        result
    }
    fn filter_project(&self) -> Option<&Path> {
        if self.project_only {
            self.scope_cwd.as_deref()
        } else {
            self.project_filter.as_deref()
        }
    }

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
            project: self.filter_project().map(Path::to_path_buf),
        }
    }

    /// Compare the current filters without copying the query or project.
    fn filter_matches(&self, key: &FilterKey) -> bool {
        key.text == self.query
            && key.provider == self.provider
            && key.status == self.status
            && key.live_only == self.live_only
            && key.pinned_only == self.pinned_only
            && key.recent_only == self.recent_only
            && key.archive == self.archive
            && key.sort == self.sort
            && key.project.as_deref() == self.filter_project()
    }

    fn visible(&self) -> Rc<[usize]> {
        self.visible_rows().0
    }

    /// Visible catalog indices and their identity fingerprint, recomputed only
    /// after a filter, catalog or library change.
    fn visible_rows(&self) -> (Rc<[usize]>, u64) {
        if let Some(cached) = self.cached_visible.borrow().as_ref() {
            if self.filter_matches(&cached.key) {
                return (cached.rows.clone(), cached.fingerprint);
            }
        }
        let key = self.filter_key();
        let query = SessionQuery {
            text: &key.text,
            provider: key.provider,
            status: key.status,
            live_only: key.live_only,
            project: key.project.as_deref(),
            sort: key.sort,
        };
        // Catalog replacement installs one normalized index for every query.
        let rows: Rc<[usize]> = self
            .index
            .query_with_library(
                &query,
                &self.library_snapshot,
                SessionLibraryFilter {
                    pinned_only: key.pinned_only,
                    recent_only: key.recent_only,
                    pinned_first: true,
                    archive: key.archive,
                },
            )
            .into();
        let mut hasher = DefaultHasher::new();
        for index in rows.iter() {
            let session = &self.catalog.sessions[*index];
            session.provider.hash(&mut hasher);
            session.id.hash(&mut hasher);
        }
        let fingerprint = hasher.finish();
        let query_error = SessionSearchIndex::query_error(&key.text);
        *self.cached_visible.borrow_mut() = Some(VisibleRows {
            key,
            rows: rows.clone(),
            fingerprint,
            query_error,
        });
        (rows, fingerprint)
    }

    /// The shared parser's explanation for the current search, if invalid.
    fn query_error(&self) -> Option<String> {
        self.visible_rows();
        self.cached_visible
            .borrow()
            .as_ref()
            .and_then(|cached| cached.query_error.clone())
    }

    fn replace_catalog(&mut self, catalog: SessionCatalog) -> bool {
        if self.catalog == catalog {
            return false;
        }
        self.catalog_epoch = self.catalog_epoch.wrapping_add(1);
        self.index = SessionSearchIndex::new(&catalog);
        self.projects = self.index.projects(&self.library_snapshot);
        self.catalog_stats = CatalogStats::of(&catalog);
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
            self.replace_library(
                self.workspace
                    .projection(&self.catalog, self.library.snapshot()),
            );
        }
        // Only user writes decide these outcomes; the periodic read-only
        // reload never delays them. A failed write is reported even if a
        // reload finished at the same moment, until the next successful
        // reload replaces it.
        if !self.workspace.busy() {
            if let Some(key) = self.pending_details.take() {
                if self.workspace.notice.is_none() {
                    self.details_drafts.remove(&key);
                }
            }
            if let Some(count) = self.pending_batch.take() {
                self.batch_notice = Some(if self.workspace.notice.is_none() {
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
        let index = self.selected_index(&self.visible());
        let selected = index
            .map(|index| &self.catalog.sessions[index])
            .filter(|session| !self.opened.contains_key(&open_session_key(session)));
        if self.preflight.select(selected) {
            self.end_resume_attempt();
        }
        self.preflight.poll();
        self.reconcile_resume_gate();
    }

    /// End the pending resume attempt without launching it. A "Resume anyway"
    /// covers only the attempt it started, so the next Resume of a
    /// conversation another CLI holds warns again.
    fn end_resume_attempt(&mut self) {
        self.pending_resume = None;
        if matches!(self.resume_gate, ResumeGate::Confirmed(_)) {
            self.resume_gate = ResumeGate::Clear;
        }
    }

    fn cancel_resume(&mut self) {
        self.pending_resume = None;
        self.resume_gate = ResumeGate::Clear;
        self.preflight.select(None);
    }

    /// Dismiss the live-process warning without resuming.
    fn cancel_confirmation(&mut self) {
        if matches!(self.resume_gate, ResumeGate::Armed(..)) {
            self.resume_gate = ResumeGate::Clear;
        }
    }

    fn confirmation_armed(&self) -> bool {
        matches!(self.resume_gate, ResumeGate::Armed(..))
    }

    fn confirmation_armed_for(&self, key: &SessionKey) -> bool {
        matches!(&self.resume_gate, ResumeGate::Armed(armed, _) if armed == key)
    }

    fn admit_resume(&mut self, session: &AgentSession) -> bool {
        self.admit_resume_at(session, Instant::now())
    }

    /// Decide whether a Resume request may start its fresh readiness check.
    /// Positive live-process evidence turns the first request into a warning;
    /// only a later, explicit request for exactly that identity proceeds. A
    /// repeat within [`SESSION_RESUME_ANYWAY_DELAY`] of the warning, such as
    /// the second click of a double-click, does nothing.
    fn admit_resume_at(&mut self, session: &AgentSession, now: Instant) -> bool {
        if self.catalog.resume_conflict(session).is_none() {
            self.resume_gate = ResumeGate::Clear;
            return true;
        }
        let key = session_key(session);
        let pending_for_key = self
            .pending_resume
            .as_ref()
            .is_some_and(|pending| matches_key(pending, &key));
        match &self.resume_gate {
            ResumeGate::Armed(armed, at) if *armed == key => {
                if now.saturating_duration_since(*at) < SESSION_RESUME_ANYWAY_DELAY {
                    return false;
                }
                self.resume_gate = ResumeGate::Confirmed(key);
                true
            }
            // Repeating Resume while the confirmed attempt is still pending.
            ResumeGate::Confirmed(confirmed) if *confirmed == key && pending_for_key => true,
            _ => {
                self.resume_gate = ResumeGate::Armed(key, now);
                self.pending_resume = None;
                false
            }
        }
    }

    /// A scan can observe a live CLI after Resume was requested. Launch only
    /// without such evidence or after "Resume anyway" for this identity.
    fn conflict_blocks_launch(&self, session: &AgentSession) -> bool {
        self.catalog.resume_conflict(session).is_some()
            && !jterm_core::agent_workspace::managed_session_running(session)
            && !matches!(&self.resume_gate, ResumeGate::Confirmed(key) if matches_key(session, key))
    }

    /// A confirmation belongs to one visible identity. Selecting another
    /// conversation, a refresh that hides or removes it, an open tab taking
    /// over, or (for the warning) the observed process ending clears it.
    fn reconcile_resume_gate(&mut self) {
        let (key, needs_conflict) = match &self.resume_gate {
            ResumeGate::Clear => return,
            ResumeGate::Armed(key, _) => (key, true),
            ResumeGate::Confirmed(key) => (key, false),
        };
        // A confirmation lives only as long as the attempt it started.
        let attempt_alive = needs_conflict
            || self
                .pending_resume
                .as_ref()
                .is_some_and(|pending| matches_key(pending, key));
        let keep = attempt_alive
            && self.selected_session().is_some_and(|session| {
                !self.opened.contains_key(&open_session_key(session))
                    && matches_key(session, key)
                    && (!needs_conflict || self.catalog.resume_conflict(session).is_some())
            });
        if !keep {
            self.resume_gate = ResumeGate::Clear;
        }
    }

    /// Advance the pending resume after this frame's readiness poll. Every
    /// way it ends without a launch also ends its "Resume anyway".
    fn advance_pending_resume(&mut self) -> ResumeStep {
        if !self.is_open && !self.workspace_pending {
            self.end_resume_attempt();
            return ResumeStep::Wait;
        }
        self.sync_preflight();
        let Some(pending) = self.pending_resume.clone() else {
            return ResumeStep::Wait;
        };
        if !self
            .selected_session()
            .is_some_and(|session| same_launch_target(session, &pending))
        {
            self.end_resume_attempt();
            return ResumeStep::Wait;
        }
        if let Some(launch) = self.preflight.ready_launch(&pending).cloned() {
            self.pending_resume = None;
            if self.conflict_blocks_launch(&pending) {
                // A scan observed a live CLI after Resume was requested.
                self.resume_gate = ResumeGate::Armed(session_key(&pending), Instant::now());
                return ResumeStep::Rearmed;
            }
            return ResumeStep::Launch(Box::new((pending, launch)));
        }
        if let SessionPreflightState::Unavailable(error) = self.preflight.state() {
            self.error = Some(error.clone());
            self.end_resume_attempt();
        }
        ResumeStep::Wait
    }

    /// Apply one Escape press: leave a focused editor (the caller moves the
    /// focus), else dismiss a pending warning, else close. Leaving an editor
    /// never dismisses the warning, so the next Enter can still confirm it.
    fn escape(&mut self, editor_focused: bool) -> SessionEscape {
        let action = session_escape_action(editor_focused, self.confirmation_armed());
        match action {
            SessionEscape::LeaveEditor => {}
            SessionEscape::CancelConfirmation => self.cancel_confirmation(),
            SessionEscape::Close => {
                self.is_open = false;
                self.cancel_resume();
            }
        }
        action
    }

    /// Remember an organization change the user made, so a conversation it
    /// moves out of the view is explained as their own change.
    fn note_own_change(&mut self, key: SessionKey, change: OwnChange) {
        self.own_change = Some((key, change));
    }

    /// The user's own change that may have moved the selection out of view.
    fn own_change_for_selection(&self) -> Option<OwnChange> {
        self.own_change
            .as_ref()
            .filter(|(key, _)| self.selected.as_ref() == Some(key))
            .map(|(_, change)| *change)
    }

    fn note_resumed(&mut self, _session: &AgentSession) {
        self.workspace.refresh();
    }

    fn shared_target(&self, key: &SessionKey) -> Result<AgentSession, String> {
        if self.workspace_source.is_some() {
            if let Some(session) = self
                .selected_session()
                .filter(|session| session_key(session) == *key)
            {
                return Ok(session.clone());
            }
        }
        let matches: Vec<_> = self
            .catalog
            .sessions
            .iter()
            .filter(|session| session_key(session) == *key)
            .collect();
        if matches.len() != 1 {
            return Err("Choose the exact source in the workspace sidebar before organizing this conversation.".into());
        }
        Ok(matches[0].clone())
    }
    fn shared_pin(&mut self, key: &SessionKey, pinned: bool) -> Result<(), String> {
        let target = self.shared_target(key)?;
        self.workspace.set_pinned(&target, pinned)
    }
    fn shared_archive(&mut self, key: &SessionKey, archived: bool) -> Result<(), String> {
        let target = self.shared_target(key)?;
        self.workspace.set_archived(&target, archived)
    }
    fn shared_details(&mut self, key: &SessionKey, title: &str, tags: &str) -> Result<(), String> {
        let target = self.shared_target(key)?;
        self.workspace.set_details(&target, title, tags)
    }
    fn shared_batch(
        &mut self,
        keys: &[SessionKey],
        action: &SessionBatchAction,
    ) -> Result<usize, String> {
        let sessions = keys
            .iter()
            .map(|key| self.shared_target(key))
            .collect::<Result<Vec<_>, _>>()?;
        self.workspace.apply_batch(&sessions, action)
    }

    /// Catalog index of the selected conversation when it is in this view.
    fn selected_index(&self, visible: &[usize]) -> Option<usize> {
        let key = self.selected.as_ref()?;
        visible
            .iter()
            .copied()
            .find(|index| matches_key(&self.catalog.sessions[*index], key))
    }

    /// The selected conversation, only while it is visible. Resume and
    /// organization act on this; a hidden selection is never acted on.
    fn selected_session(&self) -> Option<&AgentSession> {
        if let Some(source) = &self.workspace_source {
            if self
                .selected
                .as_ref()
                .is_some_and(|key| key.0 == source.0 && key.1 == source.1)
            {
                return self
                    .workspace_visible()
                    .into_iter()
                    .map(|index| &self.catalog.sessions[index])
                    .find(|session| open_session_key(session) == *source);
            }
        }
        self.selected_index(&self.visible())
            .map(|index| &self.catalog.sessions[index])
    }

    /// Title of a selection that a refresh or another terminal moved out of
    /// this view, for the notice that replaces its details.
    fn detached_selection_title(&self, visible: &[usize]) -> Option<String> {
        let key = self.selected.as_ref()?;
        if self.selected_index(visible).is_some() {
            return None;
        }
        Some(
            self.catalog
                .sessions
                .iter()
                .find(|session| matches_key(session, key))
                .map(|session| {
                    crate::review_text::visible_bounded(
                        self.library_snapshot
                            .facts(session.provider, &session.id)
                            .display_title(&session.title),
                        160,
                    )
                })
                .unwrap_or_else(|| "The selected conversation".into()),
        )
    }

    /// Fill an empty selection with the first visible row: the initial
    /// population or an explicit filter/search edit. A selection is never
    /// retargeted; if a background refresh hides or removes it, it stays
    /// selected but detached until the user chooses another conversation.
    fn reconcile_selection(&mut self) {
        if self.selected.is_none() {
            self.selected = self
                .visible()
                .first()
                .map(|index| session_key(&self.catalog.sessions[*index]));
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
        let last = visible.len() - 1;
        let position = visible.iter().position(|i| {
            self.selected
                .as_ref()
                .is_some_and(|selected| matches_key(&self.catalog.sessions[*i], selected))
        });
        let page = self.page_rows.max(1);
        let next = match (position, key) {
            // A detached or empty selection restarts from a list edge.
            (None, egui::Key::End) => last,
            (
                None,
                egui::Key::ArrowUp
                | egui::Key::ArrowDown
                | egui::Key::PageUp
                | egui::Key::PageDown
                | egui::Key::Home,
            ) => 0,
            (Some(position), egui::Key::ArrowUp) => position.saturating_sub(1),
            (Some(position), egui::Key::ArrowDown) => (position + 1).min(last),
            (Some(position), egui::Key::PageUp) => position.saturating_sub(page),
            (Some(position), egui::Key::PageDown) => position.saturating_add(page).min(last),
            (Some(_), egui::Key::Home) => 0,
            (Some(_), egui::Key::End) => last,
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

    /// Opening the manager, F5 and Refresh: rescan now (a running scan's
    /// result is already on its way), reload the shared library and recheck
    /// the selected conversation's project and CLI (which also recomputes its
    /// resume command off the UI thread).
    fn refresh_all(&mut self) {
        // A scan that finished while the manager was closed is already old.
        // Apply it now, so it cannot count as the "previous scan" and delay
        // the fresh one by a whole refresh interval.
        if let Some(catalog) = self.scanner.poll() {
            self.replace_catalog(catalog);
        }
        self.scanner.refresh();
        self.library.refresh();
        self.preflight.refresh();
    }

    /// Opening is an explicit action: a selection that is not listed under
    /// the current filters (left over from the last visit) gives way to the
    /// first match instead of a "no longer in this view" notice.
    fn adopt_listed_selection(&mut self) {
        if self.selected_session().is_none() {
            self.selected = None;
            self.reconcile_selection();
        }
    }

    /// While open: start a scan once the previous one has been idle for the
    /// refresh interval, and reload the library in the same step.
    fn refresh_if_due(&mut self) {
        if self.scanner.refresh_if_idle_for(SESSION_REFRESH_INTERVAL) {
            self.library.refresh();
        }
    }

    fn note_copied(&mut self, key: &SessionKey, kind: CopyKind) {
        self.copy_receipt = Some(CopyReceipt {
            key: key.clone(),
            kind,
            at: Instant::now(),
        });
    }

    fn copy_receipt_remaining(&self, key: &SessionKey, kind: CopyKind) -> Option<Duration> {
        let receipt = self
            .copy_receipt
            .as_ref()
            .filter(|receipt| receipt.key == *key && receipt.kind == kind)?;
        COPY_RECEIPT
            .checked_sub(receipt.at.elapsed())
            .filter(|remaining| !remaining.is_zero())
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

fn open_session_key(session: &AgentSession) -> OpenSessionKey {
    (
        session.provider,
        session.id.clone(),
        crate::workspace_sessions_bridge::source_profile(session),
    )
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
    cancel_confirmation: bool,
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
    visible_fingerprint: u64,
) -> Option<(Vec<SessionKey>, SessionBatchAction)> {
    let mut change = None;
    // An empty selection costs no key copies or per-row hashing. The visible
    // set is bound to "Select visible" through its cached fingerprint.
    let mut checked_keys = state.checked_keys();
    let visible_checked = if state.checked.is_empty() {
        0
    } else {
        let checked: HashSet<(SessionProvider, &str)> = state
            .checked
            .iter()
            .map(|(provider, id)| (*provider, id.as_str()))
            .collect();
        visible
            .iter()
            .filter(|index| {
                let session = &state.catalog.sessions[**index];
                checked.contains(&(session.provider, session.id.as_str()))
            })
            .count()
    };
    let mut selection_changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!(
            "{} selected · {} hidden by filters or refresh",
            state.checked.len(),
            state.checked.len().saturating_sub(visible_checked)
        ));
        if ui
            .push_id(
                (
                    "select_visible_sessions",
                    visible_fingerprint,
                    &checked_keys,
                ),
                |ui| ui.add_enabled(!visible.is_empty(), egui::Button::new("Select visible")),
            )
            .inner
            .clicked()
        {
            state.library_notice = state.select_visible(visible).err();
            selection_changed = true;
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
            selection_changed = true;
        }
    });
    if !state.checked.is_empty() {
        if selection_changed {
            checked_keys = state.checked_keys();
        }
        let keys = checked_keys;
        let enabled = state.library.is_loaded() && !state.library.is_saving();
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
        let mut tag_problem = None;
        ui.horizontal_wrapped(|ui| {
            let editor = ui.add(
                egui::TextEdit::singleline(&mut state.batch_tag)
                    .id(egui::Id::new("session_batch_tag"))
                    .hint_text("One tag for the selected conversations")
                    .char_limit(editor_char_limit(SESSION_TAGS_EDITOR_MAX_BYTES))
                    .desired_width(260.0),
            );
            state.text_editors.push(editor.id);
            // Validate after this frame's edit so the buttons match the text.
            tag_problem = batch_tag_problem(&state.batch_tag);
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
                    enabled && !state.batch_tag.trim().is_empty() && tag_problem.is_none(),
                    label,
                )
                .clicked()
                {
                    change = Some((keys.clone(), action));
                }
            }
        });
        if let Some(problem) = &tag_problem {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                crate::review_text::visible_bounded(problem, 512),
            );
        }
        ui.small(format!("Actions apply to all {} checked conversations, including hidden selections. Limit: {SESSION_BATCH_LIMIT}. A tag is at most {SESSION_TAG_MAX_BYTES} bytes.", keys.len()));
    }
    if let Some(notice) = &state.batch_notice {
        ui.small(notice);
    }
    change
}

// The label is part of the identity: a press that began on one meaning can
// never be released as another after a refresh or a new live-process scan.
fn session_resume_button(
    ui: &mut egui::Ui,
    selected: Option<&SessionKey>,
    enabled: bool,
    mode: ResumeButtonMode,
) -> egui::Response {
    ui.push_id(("resume_agent_session", selected, mode), |ui| {
        ui.add_enabled(enabled, egui::Button::new(mode.label()))
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

fn show_copy_receipt(
    ui: &mut egui::Ui,
    state: &AgentSessionsState,
    key: &SessionKey,
    kind: CopyKind,
) {
    if let Some(remaining) = state.copy_receipt_remaining(key, kind) {
        ui.small(egui::RichText::new(kind.receipt()).color(egui::Color32::LIGHT_GREEN));
        ui.ctx().request_repaint_after(remaining);
    }
}

// Identity-bound like Resume: a press on one conversation's warning cannot be
// released as a confirmation for a replacement.
fn session_resume_anyway_button(ui: &mut egui::Ui, key: &SessionKey) -> egui::Response {
    ui.push_id(("resume_agent_session_anyway", key), |ui| {
        ui.button("Resume anyway")
    })
    .inner
}

/// Details for the selected, visible conversation. Only this function offers
/// organize and resume-warning actions, so a detached selection has none.
fn show_session_details(
    ui: &mut egui::Ui,
    state: &mut AgentSessionsState,
    index: usize,
    actions: &mut SessionManagerActions,
) {
    let session = state.catalog.sessions[index].clone();
    let key = session_key(&session);
    let facts = state.library_snapshot.facts(session.provider, &session.id);
    let (pinned, archived, last_resumed, has_alias) = (
        facts.pinned,
        facts.archived,
        facts.last_resumed_ms,
        facts.alias.is_some(),
    );
    let title = crate::review_text::visible_bounded(facts.display_title(&session.title), 240);
    // Periodic read-only reloads never disable organization; only a save does.
    let can_organize = state.library.is_loaded() && !state.library.is_saving();
    ui.heading(title);
    ui.label(format!(
        "{} · {} · {}",
        session.provider.label(),
        session.status.label(),
        age_label(session.updated_at_ms)
    ));
    if session_pin_button(ui, &key, pinned, can_organize).clicked() {
        actions.pin_change = Some((session.provider, session.id.clone(), !pinned));
    }
    if let Some(resumed) = last_resumed {
        ui.small(format!(
            "Last resumed {} · shared across your terminals",
            age_label(resumed)
        ));
    }
    let opened_here = state.opened.contains_key(&open_session_key(&session));
    if opened_here {
        ui.colored_label(
            egui::Color32::LIGHT_GREEN,
            "Ready to switch to the open tab",
        );
    } else if state.preflight.ready_launch(&session).is_some() {
        ui.colored_label(
            egui::Color32::LIGHT_GREEN,
            "Ready · project and CLI checked",
        );
    } else {
        match state.preflight.state() {
            SessionPreflightState::Unavailable(error) => {
                ui.colored_label(
                    egui::Color32::LIGHT_RED,
                    crate::review_text::visible_bounded(error, 1024),
                );
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking project and CLI…");
                });
            }
        }
    }
    if state.pending_resume.is_some() {
        ui.small("Confirming a fresh readiness check…");
    }
    let live_pids = state.catalog.live_pids(&session);
    if !live_pids.is_empty() {
        ui.colored_label(
            egui::Color32::LIGHT_GREEN,
            format!(
                "Live CLI process · PID {}",
                live_pids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        ui.small("Exact session-ID process match; recorded state remains separate.");
    }
    if opened_here {
        ui.label("Open in this window · confirming switches to its tab");
    } else if let Some(warning) = state.catalog.resume_conflict(&session) {
        let armed = state.confirmation_armed_for(&key);
        ui.group(|ui| {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                crate::review_text::visible_bounded(&warning, 1024),
            );
            if armed {
                ui.label("Resume anyway only if you mean to run both CLIs, or after the other one exits.");
                ui.horizontal_wrapped(|ui| {
                    if session_resume_anyway_button(ui, &key).clicked() {
                        actions.resume = true;
                    }
                    if ui.button("Cancel").clicked() {
                        actions.cancel_confirmation = true;
                    }
                });
            } else {
                ui.small("Resume asks for a second confirmation while that process is observed.");
            }
        });
    }
    ui.add_space(8.0);
    let saved = SessionDetailsDraft::saved(&state.library_snapshot, &key);
    let can_edit = state.library.is_loaded() && state.pending_details.as_ref() != Some(&key);
    let editors = &mut state.text_editors;
    let pending_details = state.pending_details.as_ref() == Some(&key);
    let loaded = state.library.is_loaded();
    let draft = state
        .details_drafts
        .entry(key.clone())
        .or_insert_with(|| saved.clone());
    draft.sync_saved(&saved);
    egui::CollapsingHeader::new("Organize this conversation")
        .id_salt(("session_details", session.provider.label(), &session.id))
        .default_open(true)
        .show(ui, |ui| {
            ui.small(format!(
                "Local alias · {} / {SESSION_ALIAS_MAX_BYTES} bytes",
                draft.alias.len()
            ));
            let alias = ui.add_enabled(
                can_edit,
                egui::TextEdit::singleline(&mut draft.alias)
                    .id_salt(("session_alias", session.provider.label(), &session.id))
                    .hint_text("A recognizable name for this work")
                    .char_limit(editor_char_limit(SESSION_ALIAS_MAX_BYTES))
                    .desired_width(f32::INFINITY),
            );
            ui.small(format!(
                "Tags · comma separated · up to {SESSION_TAG_LIMIT}, each at most {SESSION_TAG_MAX_BYTES} bytes"
            ));
            let tags = ui.add_enabled(
                can_edit,
                egui::TextEdit::singleline(&mut draft.tags)
                    .id_salt(("session_tags", session.provider.label(), &session.id))
                    .hint_text("review, release, research")
                    .char_limit(editor_char_limit(SESSION_TAGS_EDITOR_MAX_BYTES))
                    .desired_width(f32::INFINITY),
            );
            editors.extend([alias.id, tags.id]);
            let changed = draft.alias != saved.alias || draft.tags != saved.tags;
            // The same checks Save applies, live, so a byte limit or bad tag
            // is explained while typing instead of after a rejected save.
            let problem = validate_session_details(&draft.alias, &draft.tags).err();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        can_organize && changed && problem.is_none(),
                        egui::Button::new("Save details"),
                    )
                    .clicked()
                {
                    actions.details_change =
                        Some((key.clone(), draft.alias.clone(), draft.tags.clone()));
                }
                if ui
                    .add_enabled(can_edit && changed, egui::Button::new("Reset"))
                    .clicked()
                {
                    *draft = saved.clone();
                }
                if pending_details {
                    ui.small("Saving details…");
                } else if !loaded {
                    ui.small("Loading organization…");
                } else if changed {
                    ui.small("Unsaved changes");
                }
            });
            if let Some(problem) = &problem {
                ui.colored_label(
                    egui::Color32::LIGHT_RED,
                    crate::review_text::visible_bounded(problem, 512),
                );
            }
            if draft.is_dirty()
                && (draft.original_alias != saved.alias || draft.original_tags != saved.tags)
            {
                ui.colored_label(egui::Color32::YELLOW, "Saved details changed in another terminal. Reset to use those details, or save your edits.");
            }
            ui.small("Alias and tags are shared across your four terminals. The provider's conversation stays intact.");
            if has_alias {
                ui.small(format!(
                    "Original title: {}",
                    crate::review_text::visible_bounded(&session.title, 240)
                ));
            }
            if session_archive_button(ui, &key, archived, can_organize)
                .on_hover_text("Moves this conversation between Active and Archived in the shared library. It does not delete history or stop a running agent.")
                .clicked()
            {
                actions.archive_change = Some((key.clone(), !archived));
            }
        });
    if !draft.is_dirty() {
        state.details_drafts.remove(&key);
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Project").strong());
    let project = session.cwd.to_string_lossy();
    ui.label(crate::review_text::visible_bounded(&project, 1024));
    ui.horizontal_wrapped(|ui| {
        if ui.small_button("Copy path").clicked() {
            ui.ctx()
                .copy_text(crate::review_text::visible_bounded(&project, 4096));
            state.note_copied(&key, CopyKind::Path);
        }
        show_copy_receipt(ui, state, &key, CopyKind::Path);
    });
    if let Some(branch) = &session.branch {
        ui.label(format!(
            "Branch: {}",
            crate::review_text::visible_bounded(branch, 160)
        ));
    }
    ui.label(format!(
        "Session: {}",
        crate::review_text::visible_bounded(&session.id, 160)
    ));
    ui.separator();
    ui.label(egui::RichText::new("Latest context").strong());
    ui.label(if session.preview.is_empty() {
        "No message preview available".to_string()
    } else {
        crate::review_text::visible_bounded(&session.preview, 8192)
    });
    ui.add_space(10.0);
    if opened_here {
        return;
    }
    // The background readiness check computes the exact command together
    // with its project and history checks, so this never touches the disk.
    // A missing CLI still leaves the command copyable for another terminal.
    ui.label(egui::RichText::new("Resume command").strong());
    let command = state.preflight.command(&session).map(str::to_owned);
    match &command {
        Some(command) => {
            ui.monospace(crate::review_text::visible_bounded(command, 1024));
        }
        None if matches!(
            state.preflight.state(),
            SessionPreflightState::Unavailable(_)
        ) =>
        {
            // The readiness check above already shows the reason.
            ui.small("Unavailable · the readiness check above explains why");
        }
        None => {
            ui.small("Checking the resume command…");
        }
    }
    ui.horizontal_wrapped(|ui| {
        let copy = ui
            .add_enabled(command.is_some(), egui::Button::new("Copy command"))
            .on_disabled_hover_text("Available once the readiness check finds the conversation");
        if let (true, Some(command)) = (copy.clicked(), &command) {
            ui.ctx().copy_text(command.clone());
            state.note_copied(&key, CopyKind::Command);
        }
        show_copy_receipt(ui, state, &key, CopyKind::Command);
    });
}

// Render pure navigation/editor state and stage exact-identity actions. The
// caller remains responsible for disk mutations, preflight and process launch.
fn show_session_manager(
    ctx: &egui::Context,
    state: &mut AgentSessionsState,
    task_dashboard_enabled: bool,
) -> SessionManagerActions {
    let mut open = true;
    let mut actions = SessionManagerActions {
        open: true,
        resume: false,
        cancel_confirmation: false,
        pin_change: None,
        details_change: None,
        archive_change: None,
        batch_change: None,
        open_tasks: false,
    };
    state.text_editors.clear();
    let window = egui::Window::new("Agent Sessions")
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
                    .clicked() { actions.open_tasks = true; }
            });
            ui.label("Organize conversations, browse projects and return to active work");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let stats = state.catalog_stats;
                let live = stats.live_conversations;
                ui.label(egui::RichText::new(format!(
                    "{} sessions   ·   {} Codex   ·   {} Claude   ·   {} waiting   ·   {live} live CLI {}   ·   {} pinned",
                    stats.total,
                    stats.codex,
                    stats.total - stats.codex,
                    stats.waiting,
                    if live == 1 { "conversation" } else { "conversations" },
                    state.library_snapshot.pinned_count()
                )).strong())
                .on_hover_text("Live CLI conversations have an exact session-ID match to at least one local CLI process at the latest refresh.");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!state.scanner.is_scanning(), egui::Button::new("Refresh  F5")).clicked() { state.refresh_all(); }
                    if state.scanner.is_scanning() { ui.spinner(); }
                });
            });
            let previous_filters = state.filter_key();
            ui.horizontal(|ui| {
                let response = ui.add(egui::TextEdit::singleline(&mut state.query).hint_text("Search · tag:review provider:codex project:core -term \"exact phrase\"").desired_width(f32::INFINITY).char_limit(editor_char_limit(SESSION_QUERY_MAX_BYTES)));
                state.search_widget = Some(response.id);
                state.text_editors.push(response.id);
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
            // An explicit filter or search edit may select its first match.
            if !state.filter_matches(&previous_filters) { state.filters_changed(); }
            state.reconcile_selection();
            let (visible, visible_fingerprint) = state.visible_rows();
            ui.separator();
            actions.batch_change = show_session_batch_controls(ui, state, &visible, visible_fingerprint);
            ui.small(format!("{} matching sessions · {}{}", visible.len(), state.sort.label(), if state.query.trim().is_empty() && state.sort == SessionSort::Relevance { " · newest first" } else { "" }));
            if let Some(error) = state.query_error() {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            if let Some(error) = &state.error { ui.colored_label(egui::Color32::LIGHT_RED, crate::review_text::visible_bounded(error, 1024)); }
            if !state.library.is_loaded() { ui.small("Loading shared favorites and resume history…"); }
            else if state.library.is_saving() { ui.small("Saving favorites and organization…"); }
            if let Some(error) = state.library.error().or(state.library_notice.as_deref()) {
                ui.colored_label(egui::Color32::LIGHT_RED, crate::review_text::visible_bounded(error, 1024));
            }
            if !state.catalog.warnings.is_empty() {
                egui::CollapsingHeader::new(format!("{} discovery notices", state.catalog.warnings.len())).show(ui, |ui| {
                    for warning in &state.catalog.warnings { ui.small(crate::review_text::visible_bounded(warning, 1024)); }
                });
            }
            let height = (ui.available_height() - 72.0).max(160.0);
            let selected_before = state.selected_index(&visible);
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
                    if let Some(row) = selected_before.and_then(|index| visible.iter().position(|i| *i == index)) {
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
                        let index = visible[row];
                        let session = &state.catalog.sessions[index];
                        let selected = selected_before == Some(index);
                        let live_pids = state.catalog.live_pids(session);
                        let live_badge = if live_pids.is_empty() { String::new() } else { format!("  ·  LIVE {}", live_pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")) };
                        // One identity lookup answers every library question for the row.
                        let facts = state.library_snapshot.facts(session.provider, &session.id);
                        let title = format!("{}{}{}", if facts.pinned { "★ " } else { "" }, if facts.archived { "[Archived] " } else { "" }, crate::review_text::visible_bounded(facts.display_title(&session.title), 160));
                        let project = crate::review_text::visible_bounded(&session.cwd.to_string_lossy(), 120);
                        let context = if facts.tags.is_empty() { project } else { format!("{}  ·  {}", project, facts.tags.join(" · ")) };
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
                        if response.clicked() { state.selected = Some(key); }
                    }
                });
                state.remember_anchor(&visible, list_output.state.offset.y, stride);
                // A click in the list this frame is an explicit selection.
                let selected_index = state.selected_index(&visible);
                egui::ScrollArea::vertical().id_salt("agent_session_detail").max_height(height).show(&mut columns[1], |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        if let Some(index) = selected_index {
                            show_session_details(ui, state, index, &mut actions);
                        } else if let Some(title) = state.detached_selection_title(&visible) {
                            ui.colored_label(ui.visuals().warn_fg_color, format!("“{title}” is no longer in this view"));
                            match state.own_change_for_selection() {
                                Some(change) => ui.label(format!("{} Resume and organization are paused until you select a conversation.", change.explanation(&title))),
                                None => ui.label("A refresh, another terminal or a filter moved the selected conversation out of this list. Resume and organization are paused until you select a conversation."),
                            };
                        } else { ui.label("Select a conversation to inspect its project and latest context."); }
                    });
                });
            });
            ui.separator();
            let selected_index = state.selected_index(&visible);
            let selected_key = selected_index.map(|index| session_key(&state.catalog.sessions[index]));
            let armed = selected_key.as_ref().is_some_and(|key| state.confirmation_armed_for(key));
            ui.horizontal(|ui| {
                ui.small(if armed { "Enter or Resume anyway starts a second CLI · Esc cancels the warning" } else { "↑ ↓ / PgUp PgDn / Home End select · Ctrl+L search · Enter resume · Esc leaves a field, then closes" });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let switch_to_tab = selected_index.is_some_and(|index| state.opened.contains_key(&open_session_key(&state.catalog.sessions[index])));
                    let can_resume = state.pending_resume.is_none() && (switch_to_tab || selected_index.is_some_and(|index| state.preflight.ready_launch(&state.catalog.sessions[index]).is_some()));
                    let mode = if switch_to_tab { ResumeButtonMode::SwitchToTab } else if armed { ResumeButtonMode::ResumeAnyway } else { ResumeButtonMode::NewTab };
                    if session_resume_button(ui, selected_key.as_ref(), can_resume, mode)
                        .on_disabled_hover_text(if selected_key.is_none() { "Select a conversation in this view first" } else if matches!(state.preflight.state(), SessionPreflightState::Unavailable(_)) { "This conversation cannot be resumed here; its details explain why" } else { "Waiting for the project and CLI check" })
                        .clicked() { actions.resume = true; }
                });
            });
            ui.small("States are last recorded events. LIVE badges show exact session-ID process matches; they do not imply busy.");
        });
    if std::mem::take(&mut state.bring_to_front) {
        if let Some(window) = &window {
            ctx.move_to_top(window.response.layer_id);
        }
    }
    actions.open = open;
    actions
}

#[derive(Clone)]
struct WorkspaceSessionRef {
    epoch: u64,
    session: AgentSession,
}

fn workspace_reference_current(
    state: &AgentSessionsState,
    reference: &WorkspaceSessionRef,
) -> bool {
    state.catalog_epoch == reference.epoch
        && state
            .workspace_visible()
            .iter()
            .any(|&index| same_launch_target(&state.catalog.sessions[index], &reference.session))
}

impl TerminalApp {
    /// Constant navigation backed by provider history, live terminals, API chats and tasks.
    /// A row captures its source and catalog epoch; a refresh cannot retarget a release.
    pub(crate) fn render_workspace_sessions(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.agent_sessions.poll_library();
        if let Some(catalog) = self.agent_sessions.scanner.poll() {
            self.agent_sessions.replace_catalog(catalog);
        }
        self.agent_sessions.refresh_if_due();
        {
            let state = &mut self.agent_sessions;
            if state.library.is_loaded()
                && state
                    .workspace
                    .sync(&state.catalog, state.library.snapshot())
            {
                state.replace_library(
                    state
                        .workspace
                        .projection(&state.catalog, state.library.snapshot()),
                );
            }
        }
        self.agent_sessions.sync_preflight();
        self.finish_pending_agent_resume(&ctx);
        ctx.request_repaint_after(Duration::from_millis(
            if self.agent_sessions.pending_resume.is_some() {
                50
            } else {
                1000
            },
        ));

        let old_filter = self.agent_sessions.filter_key();
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.agent_sessions.query)
                .id_salt("workspace-session-search")
                .char_limit(editor_char_limit(SESSION_QUERY_MAX_BYTES))
                .hint_text("Search conversations…")
                .desired_width(f32::INFINITY),
        );
        if search.changed() {
            self.agent_sessions.filters_changed();
        }
        let selected_project = self
            .agent_sessions
            .project_filter
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "All projects".into());
        egui::ComboBox::from_id_salt("workspace-project")
            .selected_text(selected_project)
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.agent_sessions.project_filter,
                    None,
                    "All projects",
                );
                for project in self.agent_sessions.projects.clone() {
                    let label = project
                        .root
                        .file_name()
                        .unwrap_or(project.root.as_os_str())
                        .to_string_lossy();
                    ui.selectable_value(
                        &mut self.agent_sessions.project_filter,
                        Some(project.root.clone()),
                        label,
                    );
                }
            });
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(
                    !self.agent_sessions.live_only && self.agent_sessions.status.is_none(),
                    "All",
                )
                .clicked()
            {
                self.agent_sessions.live_only = false;
                self.agent_sessions.status = None;
            }
            if ui
                .selectable_label(self.agent_sessions.live_only, "Running")
                .on_hover_text("A live CLI process is observed; its busy state is not inferred")
                .clicked()
            {
                self.agent_sessions.live_only = true;
                self.agent_sessions.status = None;
            }
            if ui
                .selectable_label(
                    self.agent_sessions.status == Some(SessionStatus::Waiting),
                    "Needs you",
                )
                .on_hover_text(
                    "Latest provider event recorded as waiting; open to inspect current state",
                )
                .clicked()
            {
                self.agent_sessions.live_only = false;
                self.agent_sessions.status = Some(SessionStatus::Waiting);
            }
        });
        if !self.agent_sessions.filter_matches(&old_filter) {
            self.agent_sessions.filters_changed();
        }
        if let Some(error) = self.agent_sessions.query_error() {
            ui.small(error);
        }
        if let Some(error) = self.agent_sessions.error.as_ref().or(self
            .agent_sessions
            .workspace
            .notice
            .as_ref())
        {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if self.agent_sessions.pending_resume.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.small("Checking conversation…");
            });
        }

        let epoch = self.agent_sessions.catalog_epoch;
        let rows: Vec<_> = self
            .agent_sessions
            .workspace_visible()
            .iter()
            .map(|&index| {
                let session = self.agent_sessions.catalog.sessions[index].clone();
                (
                    session.clone(),
                    self.agent_sessions.workspace.title(&session).to_owned(),
                    self.agent_sessions.workspace.pinned(&session),
                    self.agent_sessions
                        .opened
                        .get(&open_session_key(&session))
                        .cloned(),
                )
            })
            .collect();
        let mut chosen = None;
        let mut pin = None;
        let mut group = None;
        for (session, title, pinned, terminal) in &rows {
            if group != Some(*pinned) {
                ui.add_space(8.0);
                ui.weak(if *pinned {
                    "PINNED"
                } else {
                    "RECENT CONVERSATIONS"
                });
                group = Some(*pinned);
            }
            let selected = !self.ai_chat_panel.is_open
                && self.agent_sessions.workspace_task.is_none()
                && terminal.as_ref().is_some_and(|id| {
                    self.session_manager.index_of(id) == Some(self.session_manager.active_index())
                });
            let id = egui::Id::new(("workspace-cli", open_session_key(session), epoch));
            ui.push_id(id, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .small_button(if *pinned { "★" } else { "☆" })
                        .on_hover_text("Pin conversation")
                        .clicked()
                    {
                        pin = Some((session.clone(), !pinned));
                    }
                    let response = ui
                        .add_sized(
                            [ui.available_width(), 42.0],
                            egui::Button::new(format!(
                                "{}\n{} · {}",
                                crate::review_text::visible_bounded(title, 70),
                                session.provider.label(),
                                if terminal.is_some() {
                                    "Open terminal"
                                } else {
                                    "Ready to resume"
                                }
                            ))
                            .selected(selected),
                        )
                        .on_hover_text(format!(
                            "{}\n{}\n{}",
                            session.cwd.display(),
                            session.source.display(),
                            session.preview
                        ));
                    if response.clicked() {
                        chosen = Some(WorkspaceSessionRef {
                            epoch,
                            session: session.clone(),
                        });
                    }
                });
            });
        }
        if rows.is_empty() {
            ui.weak(if self.agent_sessions.scanner.is_scanning() {
                "Discovering local sessions…"
            } else {
                "No CLI conversations match"
            });
        }
        if let Some((session, pinned)) = pin {
            self.agent_sessions.error = self
                .agent_sessions
                .workspace
                .set_pinned(&session, pinned)
                .err();
        }
        if let Some(reference) = chosen {
            self.activate_workspace_session(reference, &ctx);
        }

        let query = self.agent_sessions.query.trim().to_lowercase();
        let chats = self
            .ai_chat_panel
            .workspace_summaries(self.config.ai_redact_secrets);
        let mut api_choice = None;
        let mut api_pin = None;
        ui.add_space(8.0);
        ui.weak("API CONVERSATIONS");
        for chat in chats.iter().filter(|chat| {
            !chat.archived
                && (!self.agent_sessions.live_only || chat.runtime.is_live())
                && (self.agent_sessions.status != Some(SessionStatus::Waiting)
                    || matches!(
                        chat.runtime,
                        jterm_core::agent_workspace::RuntimeState::Failed
                            | jterm_core::agent_workspace::RuntimeState::Conflict
                    ))
                && self
                    .agent_sessions
                    .filter_project()
                    .is_none_or(|project| std::path::Path::new(&chat.cwd).starts_with(project))
                && (query.is_empty()
                    || format!("{} {}", chat.title, chat.preview)
                        .to_lowercase()
                        .contains(&query))
        }) {
            let selected = self.ai_chat_panel.is_open
                && self.ai_chat_panel.workspace_active_id().as_ref() == Some(&chat.id);
            let status = chat.runtime.label();
            ui.horizontal(|ui| {
                if ui
                    .small_button(if chat.pinned { "★" } else { "☆" })
                    .on_hover_text("Pin conversation")
                    .clicked()
                {
                    api_pin = Some((chat.id.clone(), !chat.pinned));
                }
                if ui
                    .selectable_label(
                        selected,
                        format!(
                            "{}\n{}",
                            crate::review_text::visible_bounded(&chat.title, 70),
                            status
                        ),
                    )
                    .clicked()
                {
                    api_choice = Some(chat.id.clone());
                }
            });
        }
        if let Some((id, pinned)) = api_pin {
            self.ai_chat_panel.workspace.pin(id, pinned);
        }
        if let Some(id) = api_choice {
            self.agent_sessions.cancel_resume();
            self.agent_sessions.workspace_pending = false;
            self.agent_sessions.workspace_task = None;
            self.ai_chat_panel
                .open_workspace_chat(Some(id), &self.config);
        }

        if self.config.experimental_task_sidebar {
            ui.add_space(8.0);
            ui.weak("TASK CONVERSATIONS");
            let mut task_choice = None;
            for task in self.task_manager.tasks().iter().filter(|task| {
                use jterm_core::agent_task::TaskStatus;
                task.status != TaskStatus::Archived
                    && (!self.agent_sessions.live_only
                        || matches!(
                            task.status,
                            TaskStatus::Starting
                                | TaskStatus::Working
                                | TaskStatus::WaitingForApproval
                        ))
                    && (self.agent_sessions.status != Some(SessionStatus::Waiting)
                        || matches!(
                            task.status,
                            TaskStatus::WaitingForApproval
                                | TaskStatus::WaitingForHuman
                                | TaskStatus::ReadyForReview
                                | TaskStatus::Failed
                        ))
            }) {
                if !query.is_empty() && !task.title.to_lowercase().contains(&query) {
                    continue;
                }
                if self
                    .agent_sessions
                    .project_filter
                    .as_ref()
                    .is_some_and(|project| project != &task.repo_root)
                {
                    continue;
                }
                if ui
                    .selectable_label(
                        self.agent_sessions.workspace_task == Some(task.id),
                        format!(
                            "{}\n{} · {}",
                            crate::review_text::visible_bounded(&task.title, 70),
                            task.provider.display_name(),
                            task.status.label()
                        ),
                    )
                    .clicked()
                {
                    task_choice = Some(task.id);
                }
            }
            if let Some(id) = task_choice {
                self.ai_chat_panel.close();
                self.agent_sessions.cancel_resume();
                self.agent_sessions.workspace_pending = false;
                self.agent_sessions.workspace_task = Some(id);
                self.task_sidebar.open_workspace_task(id);
            }
        }
        ui.add_space(8.0);
        ui.weak("TERMINALS");
        let mut shell_choice = None;
        for terminal in self.session_manager.sessions() {
            let id = &terminal.metadata.session_id;
            if self.agent_sessions.opened.values().any(|bound| bound == id) {
                continue;
            }
            let selected = !self.ai_chat_panel.is_open
                && self.agent_sessions.workspace_task.is_none()
                && self.session_manager.index_of(id) == Some(self.session_manager.active_index());
            let title = terminal.metadata.custom_name.clone().unwrap_or_else(|| {
                terminal
                    .terminal
                    .lock()
                    .current_working_dir
                    .clone()
                    .unwrap_or_else(|| "Terminal".into())
            });
            if ui
                .selectable_label(selected, crate::review_text::visible_bounded(&title, 70))
                .clicked()
            {
                shell_choice = Some(id.clone());
            }
        }
        if let Some(index) = shell_choice.and_then(|id| self.session_manager.index_of(&id)) {
            self.ai_chat_panel.close();
            self.agent_sessions.workspace_task = None;
            self.agent_sessions.workspace_pending = false;
            self.agent_sessions.cancel_resume();
            self.sidebar.view = crate::sidebar::SidebarView::Sessions;
            self.activate_session(index);
            self.return_focus_to_terminal(&ctx);
        }
    }

    fn activate_workspace_session(&mut self, reference: WorkspaceSessionRef, ctx: &egui::Context) {
        if !workspace_reference_current(&self.agent_sessions, &reference) {
            return;
        }
        self.agent_sessions.cancel_resume();
        self.agent_sessions.selected = Some(session_key(&reference.session));
        self.agent_sessions.workspace_source = Some(open_session_key(&reference.session));
        // Legacy organization keys predate source profiles. Never resume a different
        // source even if two imported histories happen to reuse a provider ID.
        if !self
            .agent_sessions
            .selected_session()
            .is_some_and(|session| same_launch_target(session, &reference.session))
        {
            self.agent_sessions.error =
                Some("Select this source in the session details before resuming.".into());
            return;
        }
        self.agent_sessions.workspace_pending = true;
        self.resume_selected_agent_session(ctx);
    }

    pub(crate) fn toggle_agent_sessions(&mut self) {
        if self.agent_sessions.is_open {
            self.agent_sessions.is_open = false;
            self.agent_sessions.cancel_resume();
        } else {
            self.open_agent_sessions();
        }
    }

    /// Open the manager, or bring an already open manager forward with its
    /// search focused. Links such as the Tasks panel's "Conversation library"
    /// use this, so a second click never closes it.
    pub(crate) fn open_agent_sessions(&mut self) {
        if self.agent_sessions.is_open {
            self.agent_sessions.bring_to_front = true;
            self.agent_sessions.focus_search = true;
            return;
        }
        let cwd = self
            .session_manager
            .sessions()
            .get(self.session_manager.active_index())
            .and_then(|session| jterm_core::process::process_cwd(session.get_shell_pid()))
            .map(PathBuf::from);
        let state = &mut self.agent_sessions;
        state.is_open = true;
        state.bring_to_front = true;
        let previous = state.filter_key();
        state.scope_cwd = cwd;
        if state.scope_cwd.is_none() {
            state.project_only = false;
        }
        if !state.filter_matches(&previous) {
            state.filters_changed();
        }
        state.refresh_all();
        state.adopt_listed_selection();
        state.focus_search = true;
        state.error = None;
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
            self.agent_sessions.end_resume_attempt();
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
            self.agent_sessions.end_resume_attempt();
        }
        // egui drops focus on Escape before this prepass runs, so an editor
        // that held focus last frame still counts as the Escape target.
        let (focused, mut editor_focused) = ctx.memory(|memory| {
            (
                memory.focused(),
                self.agent_sessions
                    .text_editors
                    .iter()
                    .any(|id| memory.focused() == Some(*id) || memory.had_focus_last_frame(*id)),
            )
        });
        let session_confirmation = session_key_route(
            focused,
            self.agent_sessions.search_widget,
            self.agent_sessions.row_focus_widget,
        );
        // A warning raised by Enter must be visible before Enter can confirm it.
        let mut warning_raised = false;
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
                    match self.agent_sessions.escape(editor_focused) {
                        SessionEscape::LeaveEditor => {
                            let editors = &self.agent_sessions.text_editors;
                            ctx.memory_mut(|memory| {
                                for id in editors {
                                    memory.surrender_focus(*id);
                                }
                            });
                            editor_focused = false;
                        }
                        SessionEscape::CancelConfirmation => {}
                        SessionEscape::Close => {
                            self.return_focus_to_terminal(ctx);
                            break;
                        }
                    }
                    ctx.input_mut(|input| {
                        input.consume_key(modifiers, key);
                    });
                }
                egui::Key::Enter
                    if !repeat
                        && !query_is_changing
                        && !metadata_changed_selection
                        && !warning_raised
                        && session_confirmation =>
                {
                    let gate_before = self.agent_sessions.resume_gate.clone();
                    self.resume_selected_agent_session(ctx);
                    warning_raised =
                        enter_raised_warning(&gate_before, &self.agent_sessions.resume_gate);
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
                egui::Key::F5 if !repeat => self.agent_sessions.refresh_all(),
                _ => {}
            }
        }
        true
    }

    fn resume_selected_agent_session(&mut self, ctx: &egui::Context) {
        let Some(session) = self.agent_sessions.selected_session().cloned() else {
            return;
        };
        let key = open_session_key(&session);
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
                self.agent_sessions.workspace_pending = false;
                self.agent_sessions.workspace_task = None;
                self.sidebar.view = crate::sidebar::SidebarView::Sessions;
                self.ai_chat_panel.close();
                self.return_focus_to_terminal(ctx);
                self.set_status(format!(
                    "Switched to the open {} conversation",
                    session.provider.label()
                ));
                return;
            }
            self.agent_sessions.opened.remove(&key);
        }
        if !jterm_core::agent_workspace::managed_session_running(&session)
            && !self.agent_sessions.admit_resume(&session)
        {
            // The live-process warning and "Resume anyway" are now shown.
            self.agent_sessions.error = None;
            self.agent_sessions.is_open = true;
            self.agent_sessions.workspace_pending = false;
            ctx.request_repaint();
            return;
        }
        self.agent_sessions.preflight.select(Some(&session));
        self.agent_sessions.preflight.refresh();
        self.agent_sessions.pending_resume = Some(session);
        self.agent_sessions.error = None;
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    fn finish_pending_agent_resume(&mut self, ctx: &egui::Context) {
        match self.agent_sessions.advance_pending_resume() {
            ResumeStep::Wait => {}
            ResumeStep::Rearmed => ctx.request_repaint(),
            ResumeStep::Launch(ready) => {
                let (session, launch) = *ready;
                self.spawn_ready_agent_session(session, launch, ctx)
            }
        }
    }

    fn spawn_ready_agent_session(
        &mut self,
        session: AgentSession,
        _launch: SessionLaunch,
        ctx: &egui::Context,
    ) {
        let key = open_session_key(&session);
        // The host owns the provider PTY; this tab attaches to that exact source identity.
        // The final launch repeats validation after asynchronous preflight.
        let launch = match jterm_core::agent_workspace::managed_launch(&session) {
            Ok(launch) => launch,
            Err(error) => {
                self.agent_sessions.end_resume_attempt();
                self.agent_sessions.error = Some(error);
                return;
            }
        };
        let argv = std::iter::once(launch.program).chain(launch.args).collect();
        let (cols, rows) = crate::terminal::clamp_terminal_dimensions(self.cols, self.rows);
        let title = format!(
            "{} · {}",
            session.provider.label(),
            crate::review_text::visible_bounded(
                self.agent_sessions
                    .library_snapshot
                    .facts(session.provider, &session.id)
                    .display_title(&session.title),
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
                self.agent_sessions.workspace_pending = false;
                self.agent_sessions.workspace_task = None;
                self.sidebar.view = crate::sidebar::SidebarView::Sessions;
                self.ai_chat_panel.close();
                self.agent_sessions.cancel_resume();
                self.agent_sessions.error = None;
                self.return_focus_to_terminal(ctx);
                self.set_status(format!(
                    "Resuming {} conversation in a new tab",
                    session.provider.label()
                ));
            }
            Err(error) => {
                self.agent_sessions.end_resume_attempt();
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
        if let Some(catalog) = state.scanner.poll() {
            state.replace_catalog(catalog);
        }
        state.refresh_if_due();
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
            cancel_confirmation,
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
        if cancel_confirmation {
            self.agent_sessions.cancel_confirmation();
            resume = false;
        }
        if let Some((provider, id, pinned)) = pin_change {
            self.agent_sessions.library_notice = self
                .agent_sessions
                .shared_pin(&(provider, id.clone()), pinned)
                .err();
            if !pinned && self.agent_sessions.library_notice.is_none() {
                self.agent_sessions
                    .note_own_change((provider, id), OwnChange::Unpinned);
            }
        }
        if let Some((key, alias, tags)) = details_change {
            self.agent_sessions.end_resume_attempt();
            match self.agent_sessions.shared_details(&key, &alias, &tags) {
                Ok(()) => {
                    self.agent_sessions.library_notice = None;
                    self.agent_sessions.pending_details = Some(key.clone());
                    self.agent_sessions.note_own_change(key, OwnChange::Details);
                }
                Err(error) => self.agent_sessions.library_notice = Some(error),
            }
        }
        if let Some((key, archived)) = archive_change {
            self.agent_sessions.end_resume_attempt();
            resume = false;
            self.agent_sessions.library_notice =
                self.agent_sessions.shared_archive(&key, archived).err();
            if self.agent_sessions.library_notice.is_none() {
                let change = if archived {
                    OwnChange::Archived
                } else {
                    OwnChange::Restored
                };
                self.agent_sessions.note_own_change(key, change);
            }
        }
        if let Some((keys, action)) = batch_change {
            self.agent_sessions.end_resume_attempt();
            resume = false;
            let selected_change = OwnChange::of_batch(&action).and_then(|change| {
                let selected = self.agent_sessions.selected.as_ref()?;
                keys.contains(selected).then(|| (selected.clone(), change))
            });
            match self.agent_sessions.shared_batch(&keys, &action) {
                Ok(count) => {
                    if let Some((key, change)) = selected_change {
                        self.agent_sessions.note_own_change(key, change);
                    }
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
            self.open_tasks_view();
            return;
        }
        if resume && self.agent_sessions.is_open {
            self.resume_selected_agent_session(ctx);
        }
        self.finish_pending_agent_resume(ctx);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_click_rejects_replaced_source_and_hidden_rows() {
        let first = session(SessionProvider::Codex, "exact-id", "First source");
        let mut state = state_with(vec![first.clone()]);
        let reference = WorkspaceSessionRef {
            epoch: state.catalog_epoch,
            session: first.clone(),
        };
        assert!(workspace_reference_current(&state, &reference));
        state.query = "no matching conversation".into();
        assert!(!workspace_reference_current(&state, &reference));
        state.query.clear();
        let mut replacement = first;
        replacement.source = PathBuf::from("/another/profile/session.jsonl");
        state.replace_catalog(SessionCatalog {
            sessions: vec![replacement],
            ..Default::default()
        });
        assert!(!workspace_reference_current(&state, &reference));
    }

    #[test]
    fn workspace_live_binding_includes_provider_source() {
        let first = session(SessionProvider::Codex, "same-native-id", "First source");
        let mut second = first.clone();
        second.source = PathBuf::from("/another/profile/session.jsonl");
        assert_ne!(open_session_key(&first), open_session_key(&second));
        assert!(!same_launch_target(&first, &second));
    }

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
        assert_eq!(state.visible()[..], [0]);
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
        assert_eq!(state.visible()[..], [0]);
        let mut next = state.catalog.clone();
        next.sessions[0].preview = "updated".into();
        state.replace_catalog(next);
        assert_eq!(
            state.scope_cwd.as_deref(),
            Some(std::path::Path::new("/tmp/project"))
        );
        assert_eq!(state.visible()[..], [0]);
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
        assert_eq!(state.visible()[..], [1]);
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
        assert_eq!(state.visible()[..], [0, 1]);
        assert_eq!(
            state.selected_session().unwrap().id,
            LIBRARY_B,
            "background metadata reorder preserves the inspected conversation"
        );
    }

    #[test]
    fn legacy_pin_alias_and_resume_history_remain_available_for_migration() {
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
        state
            .library
            .record_resumed(selected.provider, &selected.id)
            .unwrap();
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
        assert_eq!(state.visible()[..], [0]);
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
        assert_eq!(state.visible()[..], [0]);
        let key = (SessionProvider::Codex, LIBRARY_A.to_string());
        let mut draft = SessionDetailsDraft::saved(&state.library_snapshot, &key);
        draft.alias = "Unsaved release plan".into();
        state.details_drafts.insert(key.clone(), draft);
        state.query.clear();
        state.project_filter = None;
        state.archive = SessionArchiveFilter::Archived;
        state.filters_changed();
        assert_eq!(state.visible()[..], [1]);
        assert_eq!(state.selected_session().unwrap().id, LIBRARY_B);
        assert_eq!(state.details_drafts[&key].alias, "Unsaved release plan");
        state.archive = SessionArchiveFilter::All;
        state.query = "\"Release review\"".into();
        state.filters_changed();
        assert_eq!(state.visible()[..], [0]);
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
                        assert!(
                            !action.resume && !action.open_tasks && !action.cancel_confirmation
                        );
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
            let (visible, fingerprint) = state.visible_rows();
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
                    assert!(
                        show_session_batch_controls(ui, &mut state, &visible, fingerprint)
                            .is_none()
                    );
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
                    response = Some(session_resume_button(
                        ui,
                        Some(selected),
                        true,
                        ResumeButtonMode::NewTab,
                    ));
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
    fn text_shapes(shape: &egui::Shape, found: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::Shape::Text(text) => found.push((
                text.galley.text().to_owned(),
                text.galley.rect.translate(text.pos.to_vec2()),
            )),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    text_shapes(shape, found);
                }
            }
            _ => (),
        }
    }

    fn rendered_text(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        let mut found = Vec::new();
        for shape in &output.shapes {
            text_shapes(&shape.shape, &mut found);
        }
        found
    }

    fn live(provider: SessionProvider, id: &str, pid: u32) -> LiveSessionProcess {
        LiveSessionProcess {
            provider,
            id: id.into(),
            pid,
        }
    }

    #[test]
    fn background_refresh_never_retargets_a_selection_it_hides_or_removes() {
        use jterm_core::agent_sessions::SessionOrganization;
        let a = session(SessionProvider::Codex, LIBRARY_A, "first");
        let b = session(SessionProvider::Claude, LIBRARY_B, "second");
        let mut state = state_with(vec![a.clone(), b.clone()]);
        let b_key = session_key(&b);
        state.selected = Some(b_key.clone());
        state.sync_preflight();
        state.pending_resume = Some(b.clone());
        // Another terminal archives the inspected conversation.
        state.replace_library(
            SessionLibrarySnapshot::default()
                .with_organization(
                    b.provider,
                    &b.id,
                    SessionOrganization {
                        archived: true,
                        ..Default::default()
                    },
                )
                .unwrap(),
        );
        state.reconcile_selection();
        assert_eq!(state.visible()[..], [0]);
        assert_eq!(
            state.selected.as_ref(),
            Some(&b_key),
            "a background refresh keeps the user's choice instead of selecting another row"
        );
        assert!(
            state.selected_session().is_none(),
            "a detached selection is never resumed or organized"
        );
        assert_eq!(
            state.detached_selection_title(&state.visible()).as_deref(),
            Some("second")
        );
        state.sync_preflight();
        assert!(state.pending_resume.is_none());
        assert!(matches!(
            state.preflight.state(),
            SessionPreflightState::Idle
        ));
        // A catalog refresh that drops the conversation keeps it detached too.
        let mut next = state.catalog.clone();
        next.sessions.retain(|session| session.id != LIBRARY_B);
        assert!(state.replace_catalog(next));
        assert_eq!(state.selected.as_ref(), Some(&b_key));
        assert_eq!(
            state.detached_selection_title(&state.visible()).as_deref(),
            Some("The selected conversation")
        );
        // An explicit choice re-attaches, starting from the list edge.
        state.navigate(egui::Key::ArrowDown);
        assert_eq!(state.selected_session().unwrap().id, LIBRARY_A);
        assert!(state.detached_selection_title(&state.visible()).is_none());
        // An explicit search edit may still select its first match.
        state.selected = Some((SessionProvider::Claude, "gone".into()));
        state.query = "first".into();
        state.filters_changed();
        assert_eq!(state.selected_session().unwrap().id, LIBRARY_A);
        // Initial population selects the first row of an empty selection.
        let mut fresh = AgentSessionsState::default();
        assert!(fresh.selected.is_none());
        fresh.replace_catalog(SessionCatalog {
            sessions: vec![a.clone()],
            ..Default::default()
        });
        assert_eq!(fresh.selected.as_ref(), Some(&session_key(&a)));
        assert!(state.opened.is_empty() && fresh.opened.is_empty());
    }

    #[test]
    fn live_conflict_requires_a_second_confirmation_bound_to_one_identity() {
        let a = session(SessionProvider::Codex, LIBRARY_A, "running elsewhere");
        let b = session(SessionProvider::Claude, LIBRARY_B, "also running");
        let mut state = AgentSessionsState::default();
        state.replace_catalog(SessionCatalog {
            sessions: vec![a.clone(), b.clone()],
            live_processes: vec![
                live(SessionProvider::Codex, LIBRARY_A, 4242),
                live(SessionProvider::Claude, LIBRARY_B, 7),
            ],
            ..Default::default()
        });
        assert_eq!(state.selected_session().unwrap().id, LIBRARY_A);
        let t0 = Instant::now();
        assert!(
            !state.admit_resume_at(&a, t0),
            "the first Resume only warns"
        );
        assert!(state.confirmation_armed_for(&session_key(&a)));
        assert!(state.pending_resume.is_none());
        assert!(state.conflict_blocks_launch(&a));
        assert!(
            !state.admit_resume_at(&b, t0 + SESSION_RESUME_ANYWAY_DELAY),
            "a warning for one conversation never confirms another"
        );
        assert!(state.confirmation_armed_for(&session_key(&b)));
        state.cancel_confirmation();
        assert!(!state.confirmation_armed());
        // Resume, then Resume anyway, for exactly the selected identity. The
        // second click of a double-click cannot confirm the unread warning.
        assert!(!state.admit_resume_at(&a, t0));
        let early = t0 + SESSION_RESUME_ANYWAY_DELAY - Duration::from_millis(1);
        assert!(!state.admit_resume_at(&a, early));
        assert!(
            matches!(&state.resume_gate, ResumeGate::Armed(key, at) if *key == session_key(&a) && *at == t0),
            "an early repeat changes nothing"
        );
        assert!(state.admit_resume_at(&a, t0 + SESSION_RESUME_ANYWAY_DELAY));
        assert_eq!(state.resume_gate, ResumeGate::Confirmed(session_key(&a)));
        // As `resume_selected_agent_session` does once admitted.
        state.pending_resume = Some(a.clone());
        assert!(!state.conflict_blocks_launch(&a));
        state.reconcile_resume_gate();
        assert_eq!(state.resume_gate, ResumeGate::Confirmed(session_key(&a)));
        assert!(
            state.admit_resume(&a),
            "repeating Resume while that attempt is pending keeps it"
        );
        // Selecting another conversation cancels the confirmation.
        state.selected = Some(session_key(&b));
        state.reconcile_resume_gate();
        assert_eq!(state.resume_gate, ResumeGate::Clear);
        // A warning ends with the observed process; Resume then proceeds directly.
        state.selected = Some(session_key(&a));
        assert!(!state.admit_resume(&a));
        let mut ended = state.catalog.clone();
        ended
            .live_processes
            .retain(|process| process.id != LIBRARY_A);
        state.replace_catalog(ended);
        state.reconcile_resume_gate();
        assert!(!state.confirmation_armed());
        assert!(state.admit_resume(&a));
        // A scan that observes a live CLI after Resume was chosen blocks the
        // launch until that exact identity is confirmed.
        let mut started = state.catalog.clone();
        started
            .live_processes
            .push(live(SessionProvider::Codex, LIBRARY_A, 99));
        state.replace_catalog(started);
        assert!(state.conflict_blocks_launch(&a));
        // A refresh that removes the identity clears an armed warning.
        assert!(!state.admit_resume(&a));
        let mut removed = state.catalog.clone();
        removed.sessions.retain(|session| session.id != LIBRARY_A);
        state.replace_catalog(removed);
        state.reconcile_resume_gate();
        assert_eq!(state.resume_gate, ResumeGate::Clear);
        state.resume_gate = ResumeGate::Armed(session_key(&b), Instant::now());
        state.cancel_resume();
        assert_eq!(state.resume_gate, ResumeGate::Clear);
        assert!(state.opened.is_empty(), "confirmation logic never launches");
    }

    #[test]
    fn escape_leaves_an_editor_before_dismissing_a_warning_or_closing() {
        assert_eq!(
            session_escape_action(true, true),
            SessionEscape::LeaveEditor
        );
        assert_eq!(
            session_escape_action(true, false),
            SessionEscape::LeaveEditor
        );
        assert_eq!(
            session_escape_action(false, true),
            SessionEscape::CancelConfirmation
        );
        assert_eq!(session_escape_action(false, false), SessionEscape::Close);
        let a = (SessionProvider::Codex, LIBRARY_A.to_string());
        let b = (SessionProvider::Claude, LIBRARY_B.to_string());
        let t = Instant::now();
        for (before, after, raised) in [
            (ResumeGate::Clear, ResumeGate::Armed(a.clone(), t), true),
            (
                ResumeGate::Armed(a.clone(), t),
                ResumeGate::Armed(b.clone(), t),
                true,
            ),
            (
                ResumeGate::Confirmed(a.clone()),
                ResumeGate::Armed(b.clone(), t),
                true,
            ),
            (
                ResumeGate::Armed(a.clone(), t),
                ResumeGate::Confirmed(a.clone()),
                false,
            ),
            (
                ResumeGate::Armed(a.clone(), t),
                ResumeGate::Armed(a.clone(), t),
                false,
            ),
            (ResumeGate::Clear, ResumeGate::Clear, false),
            (ResumeGate::Armed(a.clone(), t), ResumeGate::Clear, false),
        ] {
            assert_eq!(
                enter_raised_warning(&before, &after),
                raised,
                "{before:?} -> {after:?}"
            );
        }

        // egui clears focus for Escape before the manager's prepass. The
        // editor that held focus is still recognized as the Escape target.
        let ctx = egui::Context::default();
        let mut state = state_with(vec![session(SessionProvider::Codex, LIBRARY_A, "first")]);
        state
            .checked
            .insert((SessionProvider::Codex, LIBRARY_A.into()));
        let frame = |state: &mut AgentSessionsState, events: Vec<egui::Event>| {
            let mut prepass = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 900.0),
                    )),
                    events,
                    ..Default::default()
                },
                |_ui| {
                    prepass = Some(ctx.memory(|memory| {
                        (
                            memory.focused(),
                            state.text_editors.iter().any(|id| {
                                memory.focused() == Some(*id) || memory.had_focus_last_frame(*id)
                            }),
                        )
                    }));
                    show_session_manager(&ctx, state, false);
                },
            );
            output.textures_delta.clear();
            prepass.unwrap()
        };
        frame(&mut state, Vec::new());
        assert_eq!(
            state.text_editors.len(),
            4,
            "search, batch tag, alias and tags are all Escape targets"
        );
        let search = state.search_widget.unwrap();
        assert!(state.text_editors.contains(&search));
        ctx.memory_mut(|memory| memory.request_focus(search));
        assert_eq!(frame(&mut state, Vec::new()), (Some(search), true));
        let escape = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert_eq!(frame(&mut state, vec![escape.clone()]), (None, true));
        assert_eq!(
            frame(&mut state, vec![escape]),
            (None, false),
            "the next Escape is no longer an editor Escape"
        );

        // The handler: leaving an editor keeps an armed warning, so the next
        // Enter can still confirm it. The next Escape dismisses the warning
        // and only a third closes the manager.
        let mut state = AgentSessionsState::default();
        state.replace_catalog(SessionCatalog {
            sessions: vec![session(SessionProvider::Codex, LIBRARY_A, "held")],
            live_processes: vec![live(SessionProvider::Codex, LIBRARY_A, 9)],
            ..Default::default()
        });
        state.is_open = true;
        let held = state.selected_session().unwrap().clone();
        assert!(!state.admit_resume(&held));
        assert_eq!(state.escape(true), SessionEscape::LeaveEditor);
        assert!(
            state.confirmation_armed_for(&session_key(&held)),
            "leaving an editor never dismisses the warning"
        );
        assert_eq!(state.escape(false), SessionEscape::CancelConfirmation);
        assert!(!state.confirmation_armed());
        assert!(state.is_open);
        assert_eq!(state.escape(false), SessionEscape::Close);
        assert!(!state.is_open);
    }

    #[test]
    fn editors_follow_the_core_byte_budgets_and_never_silently_truncate() {
        assert_eq!(editor_char_limit(SESSION_QUERY_MAX_BYTES), 4097);
        let long_alias = "名".repeat(SESSION_ALIAS_MAX_BYTES / 3 + 1);
        assert!(long_alias.chars().count() < editor_char_limit(SESSION_ALIAS_MAX_BYTES));
        assert!(validate_session_details(&long_alias, "").is_err());
        assert!(validate_session_details("Release review", "review, release").is_ok());
        assert!(batch_tag_problem("").is_none());
        assert!(batch_tag_problem(" Review ").is_none());
        assert!(batch_tag_problem("review,release").is_some());
        assert!(batch_tag_problem(&"t".repeat(SESSION_TAG_MAX_BYTES + 1)).is_some());
        assert!(batch_tag_problem("bad\u{202e}tag").is_some());

        // A pasted search keeps its trailing exclusion when within budget,
        // and an oversized paste is explained instead of quietly broadened.
        let within = format!("{} -tag:blocked", "x".repeat(4000));
        let beyond = format!("{} -tag:blocked", "x".repeat(5000));
        for (pasted, valid) in [(within, true), (beyond, false)] {
            let ctx = egui::Context::default();
            let mut state = state_with(vec![session(SessionProvider::Codex, LIBRARY_A, "first")]);
            let frame = |state: &mut AgentSessionsState, events| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1280.0, 900.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |_ui| {
                        show_session_manager(&ctx, state, false);
                    },
                );
                output.textures_delta.clear();
            };
            frame(&mut state, Vec::new());
            ctx.memory_mut(|memory| memory.request_focus(state.search_widget.unwrap()));
            frame(&mut state, Vec::new());
            frame(&mut state, vec![egui::Event::Paste(pasted.clone())]);
            if valid {
                assert_eq!(state.query, pasted, "the core sees the full final term");
                assert!(state.query_error().is_none());
            } else {
                assert!(state.query.len() > SESSION_QUERY_MAX_BYTES);
                assert!(
                    state.query_error().unwrap().contains("4,096 bytes"),
                    "the shared parser explains the limit"
                );
                assert!(
                    state.visible().is_empty(),
                    "an over-budget search never shows a broadened result"
                );
            }
        }
    }

    #[test]
    fn copy_command_and_display_use_the_exact_resume_launch() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("ember-resume-command-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        let history = root.join("codex-home").join("sessions").join("2026");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&history).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = history.join("rollout.jsonl");
        std::fs::write(&source, "{}\n").unwrap();
        let mut resumable = session(SessionProvider::Codex, LIBRARY_A, "resumable");
        resumable.cwd = project.canonicalize().unwrap();
        resumable.source = source;
        let expected = resumable.resume_launch().unwrap().display_command();
        assert!(
            expected.contains("CODEX_HOME="),
            "a provider-home override is part of the command: {expected}"
        );
        let mut state = state_with(vec![resumable.clone()]);
        let ctx = egui::Context::default();
        let frame = |state: &mut AgentSessionsState, events| {
            let mut actions = SessionManagerActions {
                open: true,
                resume: false,
                cancel_confirmation: false,
                pin_change: None,
                details_change: None,
                archive_change: None,
                batch_change: None,
                open_tasks: false,
            };
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 2400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| show_session_details(ui, state, 0, &mut actions),
            );
            output.textures_delta.clear();
            assert!(!actions.resume && actions.details_change.is_none());
            output
        };
        // Before the background check reports, nothing is computed on the
        // UI thread: the pane says it is checking and Copy has nothing to copy.
        let output = frame(&mut state, Vec::new());
        let text = rendered_text(&output);
        assert!(text
            .iter()
            .any(|(shown, _)| shown == "Checking the resume command…"));
        assert!(!text.iter().any(|(shown, _)| *shown == expected));
        state.sync_preflight();
        let deadline = Instant::now() + Duration::from_secs(5);
        while state.preflight.command(&resumable).is_none() {
            state.preflight.poll();
            assert!(
                Instant::now() < deadline,
                "the readiness check did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // The CLI may be missing here; the command is still exact and copyable.
        assert_eq!(state.preflight.command(&resumable), Some(expected.as_str()));
        let output = frame(&mut state, Vec::new());
        let text = rendered_text(&output);
        assert!(
            text.iter().any(|(shown, _)| *shown == expected),
            "the displayed command is the exact resume command"
        );
        let button = text
            .iter()
            .find(|(shown, _)| shown == "Copy command")
            .map(|(_, rect)| rect.center())
            .unwrap();
        let click = |pressed| {
            vec![
                egui::Event::PointerMoved(button),
                egui::Event::PointerButton {
                    pos: button,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        frame(&mut state, click(true));
        let output = frame(&mut state, click(false));
        assert!(output
            .platform_output
            .commands
            .iter()
            .any(|command| *command == egui::OutputCommand::CopyText(expected.clone())));
        let key = session_key(&resumable);
        assert!(state
            .copy_receipt_remaining(&key, CopyKind::Command)
            .is_some());
        assert!(state.copy_receipt_remaining(&key, CopyKind::Path).is_none());
        let output = frame(&mut state, Vec::new());
        assert!(rendered_text(&output)
            .iter()
            .any(|(shown, _)| shown == "Copied the resume command"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn overview_counts_live_conversations_not_processes() {
        let catalog = SessionCatalog {
            sessions: vec![
                session(SessionProvider::Codex, LIBRARY_A, "two processes"),
                session(SessionProvider::Claude, LIBRARY_B, "quiet"),
            ],
            live_processes: vec![
                live(SessionProvider::Codex, LIBRARY_A, 1),
                live(SessionProvider::Codex, LIBRARY_A, 2),
                live(SessionProvider::Claude, LIBRARY_A, 3),
            ],
            ..Default::default()
        };
        let stats = CatalogStats::of(&catalog);
        assert_eq!(
            stats,
            CatalogStats {
                total: 2,
                codex: 1,
                waiting: 0,
                live_conversations: 1,
            }
        );
        let mut state = AgentSessionsState::default();
        state.replace_catalog(catalog.clone());
        assert_eq!(state.catalog_stats, stats);
        // Select-visible gestures are bound to the identities shown.
        let (_, before) = state.visible_rows();
        assert_eq!(state.visible_rows().1, before);
        let mut fewer = catalog;
        fewer.sessions.pop();
        state.replace_catalog(fewer);
        assert_ne!(state.visible_rows().1, before);
    }

    fn settle_pending_resume(state: &mut AgentSessionsState) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match state.advance_pending_resume() {
                ResumeStep::Wait if state.pending_resume.is_none() => return,
                ResumeStep::Wait => {}
                step => panic!("the attempt must end without a launch: {step:?}"),
            }
            assert!(Instant::now() < deadline, "readiness check did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_resume_anyway_covers_only_the_attempt_it_started() {
        let mut held = session(SessionProvider::Codex, LIBRARY_A, "held elsewhere");
        held.cwd = PathBuf::from("/nonexistent-ember-resume-anyway-fixture");
        let mut state = AgentSessionsState::default();
        state.replace_catalog(SessionCatalog {
            sessions: vec![held.clone()],
            live_processes: vec![live(SessionProvider::Codex, LIBRARY_A, 4242)],
            ..Default::default()
        });
        state.is_open = true;
        let key = session_key(&held);
        let t0 = Instant::now();
        assert!(!state.admit_resume_at(&held, t0));
        assert!(state.admit_resume_at(&held, t0 + SESSION_RESUME_ANYWAY_DELAY));
        // What `resume_selected_agent_session` does once admitted.
        state.preflight.select(Some(&held));
        state.preflight.refresh();
        state.pending_resume = Some(held.clone());
        // The fresh check fails, so the attempt ends without a launch ...
        settle_pending_resume(&mut state);
        assert!(state
            .error
            .as_deref()
            .is_some_and(|error| error.contains("project directory")));
        // ... and its confirmation ends with it: the next Resume warns again
        // instead of launching straight away once the problem is fixed.
        assert_eq!(state.resume_gate, ResumeGate::Clear);
        assert!(!state.admit_resume(&held));
        assert!(state.confirmation_armed_for(&key));
        // A confirmation without its attempt never authorizes anything.
        state.resume_gate = ResumeGate::Confirmed(key.clone());
        state.reconcile_resume_gate();
        assert_eq!(state.resume_gate, ResumeGate::Clear);
        state.resume_gate = ResumeGate::Confirmed(key.clone());
        assert!(
            !state.admit_resume(&held),
            "a stale confirmation warns again"
        );
        assert!(state.confirmation_armed_for(&key));
        // Editing the query (or any other way of dropping the attempt) also
        // ends a confirmation.
        state.resume_gate = ResumeGate::Confirmed(key.clone());
        state.pending_resume = Some(held.clone());
        state.end_resume_attempt();
        assert_eq!(state.resume_gate, ResumeGate::Clear);
        assert!(state.pending_resume.is_none());
        // Closing ends an attempt as well.
        state.resume_gate = ResumeGate::Confirmed(key);
        state.pending_resume = Some(held);
        state.is_open = false;
        assert!(matches!(state.advance_pending_resume(), ResumeStep::Wait));
        assert_eq!(state.resume_gate, ResumeGate::Clear);
        assert!(state.opened.is_empty(), "the attempt never launched");
    }

    #[test]
    fn own_changes_that_move_the_selection_out_of_view_are_explained() {
        use jterm_core::agent_sessions::SessionOrganization;
        let a = session(SessionProvider::Codex, LIBRARY_A, "first");
        let b = session(SessionProvider::Claude, LIBRARY_B, "second");
        let mut state = state_with(vec![a.clone(), b.clone()]);
        let b_key = session_key(&b);
        state.selected = Some(b_key.clone());
        state.note_own_change(b_key.clone(), OwnChange::Archived);
        // The archive lands and the conversation leaves the Active library.
        state.replace_library(
            SessionLibrarySnapshot::default()
                .with_organization(
                    b.provider,
                    &b.id,
                    SessionOrganization {
                        archived: true,
                        ..Default::default()
                    },
                )
                .unwrap(),
        );
        assert_eq!(state.selected.as_ref(), Some(&b_key), "never retargeted");
        assert!(state.selected_session().is_none());
        assert_eq!(state.own_change_for_selection(), Some(OwnChange::Archived));
        let ctx = egui::Context::default();
        let mut text = Vec::new();
        // A new window's first frame only measures it.
        for _ in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 900.0),
                    )),
                    ..Default::default()
                },
                |_ui| {
                    show_session_manager(&ctx, &mut state, false);
                },
            );
            output.textures_delta.clear();
            text = rendered_text(&output);
        }
        assert!(
            text.iter().any(|(shown, _)| shown
                .starts_with("You archived “second”, so it left this view. Choose Archived here")),
            "the notice names the user's own change and the view that finds it: {:?}",
            text.iter().map(|(shown, _)| shown).collect::<Vec<_>>()
        );
        // Another conversation's change never explains this one.
        state.note_own_change(session_key(&a), OwnChange::Unpinned);
        assert_eq!(state.own_change_for_selection(), None);
        for (action, change) in [
            (SessionBatchAction::Archive(true), Some(OwnChange::Archived)),
            (
                SessionBatchAction::Archive(false),
                Some(OwnChange::Restored),
            ),
            (SessionBatchAction::Pin(false), Some(OwnChange::Unpinned)),
            (SessionBatchAction::Pin(true), None),
            (
                SessionBatchAction::RemoveTag("review".into()),
                Some(OwnChange::Details),
            ),
        ] {
            assert_eq!(OwnChange::of_batch(&action), change, "{action:?}");
        }
        assert!(OwnChange::Unpinned
            .explanation("x")
            .contains("Turn off Pinned only"));
    }

    #[test]
    fn opening_replaces_a_selection_that_is_no_longer_listed() {
        use jterm_core::agent_sessions::SessionOrganization;
        let a = session(SessionProvider::Codex, LIBRARY_A, "first");
        let b = session(SessionProvider::Claude, LIBRARY_B, "second");
        let mut state = state_with(vec![a.clone(), b.clone()]);
        state.selected = Some(session_key(&b));
        // Another terminal archived it while the manager was closed.
        state.replace_library(
            SessionLibrarySnapshot::default()
                .with_organization(
                    b.provider,
                    &b.id,
                    SessionOrganization {
                        archived: true,
                        ..Default::default()
                    },
                )
                .unwrap(),
        );
        assert!(state.selected_session().is_none());
        state.adopt_listed_selection();
        assert_eq!(state.selected_session().unwrap().id, LIBRARY_A);
        // A listed selection is kept as it is.
        state.adopt_listed_selection();
        assert_eq!(state.selected.as_ref(), Some(&session_key(&a)));
    }

    #[test]
    fn an_unresumable_conversation_offers_no_command_to_copy() {
        let mut missing = session(SessionProvider::Codex, LIBRARY_A, "moved away");
        missing.cwd = PathBuf::from("/nonexistent-ember-command-fixture");
        let mut state = state_with(vec![missing.clone()]);
        state.sync_preflight();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(
            state.preflight.state(),
            SessionPreflightState::Unavailable(_)
        ) {
            state.preflight.poll();
            assert!(
                Instant::now() < deadline,
                "the readiness check did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(state.preflight.command(&missing), None);
        let ctx = egui::Context::default();
        let frame = |state: &mut AgentSessionsState, events| {
            let mut actions = SessionManagerActions {
                open: true,
                resume: false,
                cancel_confirmation: false,
                pin_change: None,
                details_change: None,
                archive_change: None,
                batch_change: None,
                open_tasks: false,
            };
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 2400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| show_session_details(ui, state, 0, &mut actions),
            );
            output.textures_delta.clear();
            output
        };
        let text = rendered_text(&frame(&mut state, Vec::new()));
        assert!(text
            .iter()
            .any(|(shown, _)| shown == "Unavailable · the readiness check above explains why"));
        let button = text
            .iter()
            .find(|(shown, _)| shown == "Copy command")
            .map(|(_, rect)| rect.center())
            .unwrap();
        for pressed in [true, false] {
            let output = frame(
                &mut state,
                vec![
                    egui::Event::PointerMoved(button),
                    egui::Event::PointerButton {
                        pos: button,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert!(!output
                .platform_output
                .commands
                .iter()
                .any(|command| matches!(command, egui::OutputCommand::CopyText(_))));
        }
        assert!(state
            .copy_receipt_remaining(&session_key(&missing), CopyKind::Command)
            .is_none());
    }
}
