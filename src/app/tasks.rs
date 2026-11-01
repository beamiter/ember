//! Experimental Tasks dashboard.
//!
//! Rendering takes owned row snapshots and only stages [`TaskId`] actions.
//! The action executor resolves the task and stable session ID again after the
//! egui closure, so a concurrent PTY exit or tab removal cannot redirect an
//! action to an unrelated index.

use crate::agent::{
    AgentProvider, AgentSessionOutcome, ApprovalDecision, ApprovalId, CodexAppServerApprovalKind,
    CodexAppServerPhase, CodexAppServerTurnHistory, CodexAppServerViewSnapshot,
    DashboardAttentionKind, DashboardRow, DashboardSnapshot, DashboardView, NativePromptPolicy,
    TaskBrief, TaskGuidance, TaskId, TaskInspector, TaskRuntimeKind, TaskStatus,
    TaskValidationRecipe, TaskValidationStatus, CODEX_APP_SERVER_LIVE_TURN_MAX,
    NATIVE_AGENT_FOLLOW_UP_MAX_BYTES, PTY_TASK_BRIEF_RELATIVE,
};
use crate::app::state::TerminalApp;
use crate::review_text::{sanitize_prompt_payload, visible_bounded, VisualSpoofDisposition};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_TASK_TITLE_DISPLAY_BYTES: usize = 160;
const MAX_TASK_BRANCH_DISPLAY_BYTES: usize = 120;
const MAX_TASK_DETAIL_DISPLAY_BYTES: usize = 320;
const MAX_NATIVE_AGENT_TEXT_DISPLAY_BYTES: usize = 64 * 1024;
const MAX_NATIVE_ITEM_DISPLAY_BYTES: usize = 8 * 1024;
// egui bounds this in Unicode scalar values, while the authority boundary
// below uses exact UTF-8 bytes. Keeping the larger scalar limit preserves the
// full ASCII budget; multi-byte text is still rejected once its byte counter
// exceeds the provider limit.
const MAX_NATIVE_FOLLOW_UP_CHARS: usize = NATIVE_AGENT_FOLLOW_UP_MAX_BYTES;

/// Visible before starting a print-mode provider; a worktree is not containment.
fn native_start_notice(provider: AgentProvider) -> Option<&'static str> {
    match provider {
        AgentProvider::Claude => Some("Claude native turns keep the CLI permission checks. If a tool requires approval, open the task in Terminal to review it."),
        AgentProvider::Kimi => Some("Kimi uses automatic tool permission and can run tools with your user account's file access. This worktree is not a sandbox."),
        AgentProvider::Codex | AgentProvider::OpenCode => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskSidebarAction {
    StartNative(TaskId),
    LoadHistory(TaskId, bool),
    StartTerminal(TaskId),
    StopCodex(TaskId),
    FollowUp(TaskId, String),
    FinishCodex(TaskId),
    Approve(TaskId, ApprovalId),
    Deny(TaskId, ApprovalId),
    FocusTerminal(TaskId),
    FocusValidation(TaskId),
    RunValidation(TaskId),
    Complete(TaskId),
    AcceptReviewed(TaskId),
    ReviewDiff(TaskId),
    Archive(TaskId),
    /// Return a hidden task to the lifecycle state it had when hidden.
    Restore(TaskId),
}

/// The user's own native action expects an immediate result even when the
/// runtime alone would poll slowly (an approval wait or an idle review point).
fn native_action_needs_prompt_poll(action: &TaskSidebarAction) -> bool {
    matches!(
        action,
        TaskSidebarAction::StartNative(_)
            | TaskSidebarAction::LoadHistory(..)
            | TaskSidebarAction::StopCodex(_)
            | TaskSidebarAction::FollowUp(..)
            | TaskSidebarAction::FinishCodex(_)
            | TaskSidebarAction::Approve(..)
            | TaskSidebarAction::Deny(..)
    )
}

/// How long the dashboard keeps polling at frame rate after such an action.
const NATIVE_ACTION_FAST_POLL: Duration = Duration::from_secs(2);

fn task_action_waits_for_recovery(action: &TaskSidebarAction, pending: &HashSet<TaskId>) -> bool {
    matches!(action, TaskSidebarAction::StartNative(id) | TaskSidebarAction::StartTerminal(id)
        | TaskSidebarAction::RunValidation(id) if pending.contains(id))
}

fn registration_marker_confirms(
    task: &crate::agent::AgentTask,
    record: &crate::agent::recovery::Record,
) -> bool {
    record.stage == crate::agent::recovery::Stage::Registered
        && record.registered_task_id == Some(task.id)
        && recovery_record_matches_task(task, record)
}

fn recovery_record_matches_task(
    task: &crate::agent::AgentTask,
    record: &crate::agent::recovery::Record,
) -> bool {
    record.path == task.worktree_path
        && record.repository == task.repo_root
        && record.branch == task.branch
        && record.provider == task.provider
        && record.base_commit.as_ref() == Some(&task.base_commit)
}

/// Whether a blocked registration can still be confirmed: a recovery record
/// for its worktree remains to confirm it or to retry from. Once the record is
/// dismissed, nothing can confirm the task again.
fn registration_confirmable(
    task: &crate::agent::AgentTask,
    records: &[crate::agent::recovery::Record],
) -> bool {
    records
        .iter()
        .any(|record| recovery_record_matches_task(task, record))
}

/// A hidden task whose Start still waits for this record, if any. Dismissing
/// the record would leave that task restorable for review but never startable.
fn hidden_task_waiting_on_record<'a>(
    manager: &'a crate::agent::TaskManager,
    hidden: &HashSet<TaskId>,
    record: &crate::agent::recovery::Record,
) -> Option<&'a crate::agent::AgentTask> {
    hidden
        .iter()
        .filter_map(|id| manager.get(*id))
        .find(|task| recovery_record_matches_task(task, record))
}

fn recovery_dismiss_blocked(
    manager: &crate::agent::TaskManager,
    pending: &HashSet<TaskId>,
    record: &crate::agent::recovery::Record,
) -> bool {
    pending.iter().any(|id| {
        manager
            .get(*id)
            .is_some_and(|task| recovery_record_matches_task(task, record))
    })
}

fn registration_may_autostart(
    manager: &crate::agent::TaskManager,
    id: TaskId,
    requested: bool,
) -> bool {
    requested
        && manager
            .get(id)
            .is_some_and(|task| task.status == TaskStatus::Created)
}

#[derive(Default)]
pub struct TaskSidebarState {
    pub selected: Option<TaskId>,
    pub pending_action: Option<TaskSidebarAction>,
    dashboard_query: String,
    dashboard_view: jterm_core::agent_task::DashboardView,
    dashboard_provider: Option<AgentProvider>,
    dashboard_project: Option<PathBuf>,
    dashboard_sort: jterm_core::agent_task::DashboardSort,
    dashboard_attention: Option<DashboardAttentionKind>,
    scroll_to_selected: bool,
    pending_creation: Option<PendingTaskCreation>,
    composer: TaskComposer,
    follow_up_drafts: HashMap<TaskId, String>,
    recovery_loaded: bool,
    recovery_records: Vec<crate::agent::recovery::Record>,
    recovery_error: Option<String>,
    recovery_job: Option<Receiver<Result<RecoveryResult, String>>>,
    dismiss_recovery: Option<uuid::Uuid>,
    unconfirmed_registrations: HashSet<TaskId>,
    /// Lists hidden (archived) tasks for restoration instead of the dashboard.
    show_hidden: bool,
    query_error: DashboardQueryErrorCache,
    /// Frame-rate polling after the user's own native action, see
    /// [`NATIVE_ACTION_FAST_POLL`].
    native_fast_poll_until: Option<Instant>,
    /// Sidebar state that `task:toggle` replaced when it opened the dashboard.
    toggle_return: Option<(bool, crate::sidebar::SidebarView)>,
    /// Hidden tasks whose recovery marker was still unconfirmed when hidden.
    /// Restore reinstates their Start block; hiding sets it aside so the
    /// recovery record can still be dismissed.
    hidden_unconfirmed_registrations: HashSet<TaskId>,
}

impl TaskSidebarState {
    pub(crate) fn open_workspace_task(&mut self, id: TaskId) {
        self.selected = Some(id);
        self.show_hidden = false;
        self.dashboard_query.clear();
        self.dashboard_view = Default::default();
        self.dashboard_provider = None;
        self.dashboard_project = None;
        self.dashboard_attention = None;
        self.scroll_to_selected = true;
    }
    /// Leave the Hidden list for the dashboard. Opening the dashboard and
    /// creating, registering or restoring a task all do this, so a task the
    /// user is about to act on is never out of sight in the Hidden list.
    fn show_dashboard(&mut self) {
        self.show_hidden = false;
    }
}

/// The Hidden list stays only while it has something to restore.
fn hidden_view_shown(show_hidden: bool, hidden_count: usize) -> bool {
    show_hidden && hidden_count > 0
}

/// The dashboard parses its query once for the projection; the explanation
/// shown beside the search field is recomputed only when the text changes.
#[derive(Default)]
struct DashboardQueryErrorCache {
    text: String,
    error: Option<String>,
}

impl DashboardQueryErrorCache {
    fn error(&mut self, text: &str) -> Option<&str> {
        if self.text != text {
            self.text.clear();
            self.text.push_str(text);
            self.error = jterm_core::agent_task::dashboard_query_error(text);
        }
        self.error.as_deref()
    }
}

/// A hidden task as shown in the Hidden view. Restoring is its only action
/// besides read-only diff review.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HiddenTaskRow {
    id: TaskId,
    title: String,
    provider: AgentProvider,
    project: String,
    branch: String,
    /// Recorded state to return to; `None` for older records, which cannot
    /// be restored without guessing.
    restores_to: Option<TaskStatus>,
    updated_at_ms: u64,
}

fn hidden_task_rows(manager: &crate::agent::TaskManager) -> Vec<HiddenTaskRow> {
    let mut rows: Vec<_> = manager
        .tasks()
        .iter()
        .filter(|task| task.status == TaskStatus::Archived)
        .map(|task| HiddenTaskRow {
            id: task.id,
            title: visible_bounded(&task.title, MAX_TASK_TITLE_DISPLAY_BYTES),
            provider: task.provider,
            project: visible_bounded(&task.repo_root.to_string_lossy(), 320),
            branch: visible_bounded(&task.branch, MAX_TASK_BRANCH_DISPLAY_BYTES),
            restores_to: task
                .archived_from
                .filter(|status| *status != TaskStatus::Archived),
            updated_at_ms: task.updated_at_ms,
        })
        .collect();
    // Most recently hidden first; the stable sort keeps creation order on ties.
    rows.sort_by_key(|row| std::cmp::Reverse(row.updated_at_ms));
    rows
}

/// Execute Restore against the task as it is now. Restoring never starts an
/// Agent, reruns validation or accepts work.
fn restore_hidden_task(
    manager: &mut crate::agent::TaskManager,
    task_id: TaskId,
) -> Result<String, String> {
    manager
        .restore(task_id)
        .map_err(|error| format!("Could not restore the task: {error}"))?;
    let status = manager
        .get(task_id)
        .map_or("unknown state", |task| task.status.label());
    Ok(format!("Task restored · {status}"))
}

/// What Restore reports. A registration that is still unconfirmed says how it
/// can be confirmed, or plainly that it no longer can.
fn restore_status(message: String, still_unconfirmed: bool, confirmable: bool) -> String {
    match (still_unconfirmed, confirmable) {
        (false, _) => message,
        (true, true) => format!(
            "{message}; its registration is still unconfirmed, so Start waits for Retry registration or Reload"
        ),
        (true, false) => format!(
            "{message}; its recovery record was dismissed, so its registration can no longer be confirmed and Start stays unavailable. Review its diff or create a new task instead"
        ),
    }
}

/// Move a task's unconfirmed-registration block between the visible and the
/// hidden set. Restoring a task therefore keeps Start blocked until its
/// recovery marker is confirmed, exactly as before it was hidden.
fn carry_unconfirmed_registration(
    from: &mut HashSet<TaskId>,
    to: &mut HashSet<TaskId>,
    task_id: TaskId,
) -> bool {
    let moved = from.remove(&task_id);
    if moved {
        to.insert(task_id);
    }
    moved
}

/// Why Hide is unavailable, from the same gate `archive` applies.
fn hide_task_blocked_reason(
    row: &TaskRowSnapshot,
    gate: Result<(), crate::agent::TaskError>,
) -> Option<&'static str> {
    match gate {
        Err(crate::agent::TaskError::UnknownTask(_)) => Some("This task is no longer available"),
        Err(_) if row.validation_status == TaskValidationStatus::Running => {
            Some("Wait for validation to finish before hiding this task")
        }
        Err(_) => Some("Stop or finish the Agent before hiding this task"),
        Ok(()) if row.native_preparing => {
            Some("Cancel the Agent preparation before hiding this task")
        }
        Ok(()) => None,
    }
}

/// Sidebar tab text, plus the hover explanation when tasks need attention.
#[cfg(test)]
pub(crate) fn tasks_tab_label(
    counts: jterm_core::agent_task::DashboardAttentionCounts,
) -> (String, Option<String>) {
    match counts.total() {
        0 => ("Tasks".into(), None),
        total => (format!("Tasks {total}"), counts.summary()),
    }
}

/// Label and hover text for the compact chrome indicator, shown only for an
/// enabled dashboard that is out of sight while tasks wait for the user.
pub(crate) fn task_attention_indicator(
    enabled: bool,
    sidebar_visible: bool,
    counts: jterm_core::agent_task::DashboardAttentionCounts,
) -> Option<(String, String)> {
    if !enabled || sidebar_visible {
        return None;
    }
    let summary = counts.summary()?;
    Some((
        format!("Tasks {}", counts.total()),
        format!("{summary}\nOpen the Tasks dashboard"),
    ))
}

pub(crate) fn task_attention_chip_width(ui: &egui::Ui, label: &str, font_size: f32) -> f32 {
    ui.painter()
        .layout_no_wrap(
            label.to_owned(),
            egui::FontId::proportional(font_size),
            egui::Color32::WHITE,
        )
        .size()
        .x
        + 16.0
}

/// Paint the indicator inside custom-drawn chrome and report a click. Like
/// the Vim9 launcher it never keeps keyboard focus, so terminal Space/Enter
/// cannot activate it.
pub(crate) fn task_attention_chip(
    ui: &egui::Ui,
    rect: egui::Rect,
    label: &str,
    hover: &str,
    font_size: f32,
) -> bool {
    let id = ui.id().with("task_attention_indicator");
    ui.memory_mut(|memory| memory.surrender_focus(id));
    let response = ui.interact(rect, id, egui::Sense::CLICK);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let warn = ui.visuals().warn_fg_color;
    ui.painter().rect_filled(
        rect,
        5.0,
        warn.gamma_multiply(if response.hovered() { 0.28 } else { 0.14 }),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(font_size),
        warn,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.on_hover_text(hover).clicked()
}

type SidebarPlacement = (bool, crate::sidebar::SidebarView);

/// `task:toggle`: open the dashboard and remember what it replaced, or close
/// it back to that state (hiding the sidebar when nothing was recorded).
fn tasks_toggle_target(
    current: SidebarPlacement,
    previous: Option<SidebarPlacement>,
) -> (SidebarPlacement, Option<SidebarPlacement>) {
    use crate::sidebar::SidebarView;
    if current.0 && current.1 == SidebarView::Tasks {
        (previous.unwrap_or((false, SidebarView::Tasks)), None)
    } else {
        ((true, SidebarView::Tasks), Some(current))
    }
}

struct PendingTaskCreation {
    receiver: Receiver<Result<PreparedTask, String>>,
    worker: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    authored: bool,
}

struct PreparedTask {
    context: Option<crate::agent::SemanticCommandContext>,
    brief: Option<TaskBrief>,
    autostart: bool,
    title: String,
    provider: AgentProvider,
    record: crate::agent::recovery::Record,
    journal: crate::agent::recovery::Journal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskComposer {
    open: bool,
    project: String,
    title: String,
    goal: String,
    provider: AgentProvider,
    with_validation: bool,
    validation_command: String,
    validation_shell: String,
    error: Option<String>,
}

impl Default for TaskComposer {
    fn default() -> Self {
        Self {
            open: false,
            project: String::new(),
            title: String::new(),
            goal: String::new(),
            provider: AgentProvider::Codex,
            with_validation: false,
            validation_command: String::new(),
            validation_shell: "/bin/sh".into(),
            error: None,
        }
    }
}

struct TaskCreationRequest {
    cwd: PathBuf,
    title: String,
    provider: AgentProvider,
    context: Option<crate::agent::SemanticCommandContext>,
    brief: Option<TaskBrief>,
    autostart: bool,
}

impl TaskComposer {
    fn request(&self) -> Result<TaskCreationRequest, String> {
        let title = self.title.trim();
        if title.is_empty() || title.len() > 256 || title.chars().any(char::is_control) {
            return Err(
                "Give the task a title of 1–256 UTF-8 bytes, without control characters.".into(),
            );
        }
        let project = self.project.trim();
        if project.len() > 4096
            || project.chars().any(char::is_control)
            || !std::path::Path::new(project).is_absolute()
        {
            return Err("Choose an absolute local project directory.".into());
        }
        let validation = self
            .with_validation
            .then(|| {
                TaskValidationRecipe::new(
                    self.validation_command.clone(),
                    self.validation_shell.clone(),
                )
            })
            .transpose()?;
        let brief = TaskBrief::new(self.goal.clone(), validation)?;
        Ok(TaskCreationRequest {
            cwd: project.into(),
            title: title.into(),
            provider: self.provider,
            context: None,
            brief: Some(brief),
            autostart: false,
        })
    }
}

fn task_composer_create_button(
    ui: &mut egui::Ui,
    draft: &TaskComposer,
    enabled: bool,
) -> egui::Response {
    // A held click authorizes the exact visible draft, never a replacement.
    ui.push_id(
        (
            "create_agent_task",
            &draft.project,
            &draft.title,
            &draft.goal,
            draft.provider.display_name(),
            draft.with_validation,
            &draft.validation_command,
            &draft.validation_shell,
        ),
        |ui| ui.add_enabled(enabled, egui::Button::new("Create isolated task")),
    )
    .inner
}

fn show_task_composer(
    ctx: &egui::Context,
    draft: &mut TaskComposer,
    projects: &[PathBuf],
    busy: bool,
) -> Option<TaskCreationRequest> {
    if !draft.open {
        return None;
    }
    let mut open = true;
    let mut request = None;
    egui::Window::new("New Agent task")
        .id(egui::Id::new("agent_task_composer"))
        .open(&mut open).collapsible(false).resizable(true)
        .default_size(egui::vec2(540.0, 600.0)).min_width(360.0).min_height(320.0).vscroll(true)
        .show(ctx, |ui| {
            ui.label("Describe work for an isolated Git worktree. Start the Agent separately when you are ready.");
            ui.add_enabled_ui(!busy, |ui| {
                ui.label("Project directory");
                ui.add(egui::TextEdit::singleline(&mut draft.project).id(egui::Id::new("task_composer_project")).desired_width(f32::INFINITY).char_limit(4096).hint_text("/absolute/path/to/project"));
                if !projects.is_empty() {
                    egui::ComboBox::from_id_salt("new_task_project")
                        .selected_text("Choose an existing project…")
                        .show_ui(ui, |ui| {
                            for project in projects {
                                let path = project.to_string_lossy();
                                if ui.selectable_label(draft.project == path, visible_bounded(&path, 240)).clicked() {
                                    draft.project = path.into_owned();
                                }
                            }
                        });
                }
                ui.label("Task title");
                ui.add(egui::TextEdit::singleline(&mut draft.title).id(egui::Id::new("task_composer_title")).desired_width(f32::INFINITY).char_limit(256).hint_text("Improve the project’s…"));
                ui.horizontal_wrapped(|ui| {
                    ui.label("Agent");
                    for provider in AgentProvider::ALL {
                        ui.selectable_value(&mut draft.provider, provider, provider.display_name());
                    }
                });
                ui.label("Goal and acceptance criteria");
                ui.add(egui::TextEdit::multiline(&mut draft.goal).id(egui::Id::new("task_composer_goal")).desired_width(f32::INFINITY).desired_rows(7).char_limit(16 * 1024).hint_text("What should change? What should a successful result do?"));
                ui.small(format!("{} / 16,384 UTF-8 bytes", draft.goal.len()));
                ui.checkbox(&mut draft.with_validation, "Require a validation command before completion");
                if draft.with_validation {
                    ui.label("Validation command · one line, run at the isolated worktree root");
                    ui.add(egui::TextEdit::singleline(&mut draft.validation_command).desired_width(f32::INFINITY).char_limit(16 * 1024).hint_text("cargo test"));
                    ui.label("Validation shell · absolute executable path");
                    ui.add(egui::TextEdit::singleline(&mut draft.validation_shell).desired_width(f32::INFINITY).char_limit(4096).hint_text("/bin/sh"));
                    ui.small("Validation runs only when you choose Run validation.");
                } else {
                    ui.small("Without a validation command, completion requires your explicit acceptance after reviewing the diff.");
                }
            });
            ui.small("After a restart, recovery preserves the worktree but does not restore the goal. Enter it again before starting a new goal task.");
            if let Some(error) = &draft.error {
                ui.colored_label(egui::Color32::LIGHT_RED, visible_bounded(error, 1024));
            }
            if busy {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Preparing the isolated task…"); });
            }
            if task_composer_create_button(ui, draft, !busy).clicked() {
                match draft.request() {
                    Ok(value) => { draft.error = None; request = Some(value); }
                    Err(error) => draft.error = Some(error),
                }
            }
        });
    draft.open = open;
    request
}

enum RecoveryResult {
    Loaded(Vec<crate::agent::recovery::Record>),
    Retry(
        Box<(
            crate::agent::recovery::Journal,
            crate::agent::recovery::Record,
        )>,
    ),
    Open(PathBuf, Box<crate::sidebar::FilesIntentContext>, u64),
    Registered(
        Vec<crate::agent::recovery::Record>,
        TaskId,
        AgentProvider,
        bool,
    ),
}

fn recovery_root() -> Result<PathBuf, String> {
    let root = dirs::data_local_dir()
        .ok_or("Cannot locate the per-user data directory")?
        .join("ember")
        .join("agent-tasks");
    match std::fs::canonicalize(&root) {
        Ok(root) => Ok(root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(root),
        Err(_) => Err("Cannot access the task worktree directory".into()),
    }
}

impl Drop for PendingTaskCreation {
    fn drop(&mut self) {
        // Cancellation is polled by the service between Git commands. Join to
        // prove no mutating child remains. Journal fsync may delay shutdown on
        // slow storage; cancellation never removes its recovery record.
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskRowSnapshot {
    id: TaskId,
    title: String,
    provider: AgentProvider,
    status: TaskStatus,
    runtime_kind: TaskRuntimeKind,
    branch: String,
    project: String,
    has_agent_terminal: bool,
    has_validation_terminal: bool,
    has_active_agent_stream: bool,
    native_preparing: bool,
    terminal_retry_available: bool,
    native_terminal_fallback_available: bool,
    validation_status: TaskValidationStatus,
    validation_attempt: u64,
    validation_detail: Option<String>,
    needs_attention: bool,
    status_detail: Option<String>,
}

impl TaskRowSnapshot {
    fn is_running(&self) -> bool {
        self.status.is_running()
            || self.validation_status == TaskValidationStatus::Running
            || self.has_active_agent_stream
            || self.native_preparing
    }
}

fn task_guidance(
    task: &crate::agent::AgentTask,
    row: &TaskRowSnapshot,
    registration_pending: bool,
    registration_orphaned: bool,
) -> TaskGuidance {
    let mut guidance = TaskGuidance::from_task(
        task,
        jterm_core::agent_task::DashboardActivity {
            has_agent_terminal: row.has_agent_terminal,
            has_validation_terminal: row.has_validation_terminal,
            has_active_agent_stream: row.has_active_agent_stream,
            native_preparing: row.native_preparing,
        },
    );
    if registration_pending && registration_orphaned {
        guidance.title = "Registration cannot be confirmed";
        guidance.detail = "Its recovery record was dismissed, so this task's registration can no longer be confirmed and Start stays blocked. Review its diff or hide it, and create a new task to continue the work.";
    } else if registration_pending {
        guidance.title = "Confirm task registration";
        guidance.detail = "Registration is awaiting its durable recovery marker. Start remains blocked until confirmation succeeds.";
    }
    guidance
}

fn render_task_guidance(ui: &mut egui::Ui, guidance: &TaskGuidance) {
    ui.group(|ui| {
        ui.strong(format!("Next step · {}", guidance.title));
        ui.label(guidance.detail);
        if let Some(reason) = &guidance.reason {
            egui::ScrollArea::vertical()
                .id_salt("task_current_blocker")
                .max_height(112.0)
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(reason).color(ui.visuals().warn_fg_color));
                });
        }
    });
}

fn render_task_inspector(ui: &mut egui::Ui, id: TaskId, inspector: &TaskInspector) {
    ui.group(|ui| {
        if let Some(goal) = &inspector.goal {
            egui::CollapsingHeader::new("Goal and acceptance criteria")
                .id_salt(("task_goal", id))
                .default_open(true)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(("task_goal_text", id))
                        .max_height(160.0)
                        .show(ui, |ui| {
                            ui.label(goal);
                        });
                });
        }
        egui::CollapsingHeader::new("Task inspector")
            .id_salt(("task_inspector", id))
            .show(ui, |ui| {
                for field in &inspector.fields {
                    ui.strong(field.label);
                    ui.add(egui::Label::new(&field.value).wrap());
                }
            });
    });
}

// A press on one hidden task, or on a task whose recorded state changed,
// can never be released as a Restore of something else.
fn hidden_task_restore_button(ui: &mut egui::Ui, row: &HiddenTaskRow) -> egui::Response {
    ui.push_id(
        (
            "restore_hidden_task",
            row.id,
            row.restores_to.map(|status| status as u8),
        ),
        |ui| ui.add_enabled(row.restores_to.is_some(), egui::Button::new("Restore")),
    )
    .inner
}

fn task_accept_button(
    ui: &mut egui::Ui,
    row: &TaskRowSnapshot,
    manual_review: bool,
    enabled: bool,
) -> egui::Response {
    ui.push_id(
        (
            "accept_agent_task",
            row.id,
            manual_review,
            row.status as u8,
            row.validation_status as u8,
            row.has_active_agent_stream,
            enabled,
        ),
        |ui| {
            ui.add_enabled(
                enabled,
                egui::Button::new(if manual_review {
                    "Accept reviewed work"
                } else {
                    "Mark complete"
                }),
            )
        },
    )
    .inner
}

fn task_action_scope<R>(
    ui: &mut egui::Ui,
    row: &TaskRowSnapshot,
    manual_review: bool,
    registration_pending: bool,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    ui.push_id(
        (
            "task_action_strip",
            row.id,
            row.status as u8,
            row.runtime_kind as u8,
            row.validation_status as u8,
            [
                row.has_agent_terminal,
                row.has_validation_terminal,
                row.has_active_agent_stream,
                row.native_preparing,
                row.terminal_retry_available,
                row.native_terminal_fallback_available,
                manual_review,
                registration_pending,
            ],
        ),
        contents,
    )
}

/// The shared projection owns ordering and filtering. Joining by stable ID
/// preserves frontend action facts without re-sorting the same list twice.
fn rows_in_dashboard_order(
    rows: Vec<TaskRowSnapshot>,
    dashboard: &jterm_core::agent_task::DashboardSnapshot,
) -> Vec<TaskRowSnapshot> {
    let mut by_id: HashMap<_, _> = rows.into_iter().map(|row| (row.id, row)).collect();
    dashboard
        .rows
        .iter()
        .filter_map(|row| by_id.remove(&row.id))
        .collect()
}

/// Queue navigation preserves the query and project/provider scope. Neither
/// selection nor a changing count creates an execution request.
fn dashboard_filter_chips<T: Copy>(
    ui: &mut egui::Ui,
    id: &'static str,
    choices: &[(T, String, bool)],
    help: impl Fn(T) -> String,
) -> Option<T> {
    let available = ui.available_width().max(1.0);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let natural_width = choices
        .iter()
        .map(|(_, label, _)| {
            ui.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(label.clone(), font.clone(), egui::Color32::WHITE)
                    .size()
                    .x
            })
        })
        .fold(0.0, f32::max);
    let width = (natural_width + 16.0).ceil().min(available);
    let gap = ui.spacing().item_spacing.x;
    let columns = ((available + gap) / (width + gap)).floor().max(1.0) as usize;
    let mut chosen = None;
    ui.push_id(id, |ui| {
        for (row, chunk) in choices.chunks(columns).enumerate() {
            ui.horizontal(|ui| {
                for (column, (value, label, selected)) in chunk.iter().enumerate() {
                    if ui
                        .push_id(row * columns + column, |ui| {
                            ui.add_sized(
                                [width, ui.spacing().interact_size.y],
                                egui::Button::selectable(
                                    *selected,
                                    egui::RichText::new(label).font(font.clone()),
                                )
                                .truncate(),
                            )
                            .on_hover_text(help(*value))
                        })
                        .inner
                        .clicked()
                    {
                        chosen = Some(*value);
                    }
                }
            });
        }
    });
    chosen
}

fn render_attention_queues(
    ui: &mut egui::Ui,
    search_matches: &DashboardSnapshot,
    attention: &mut Option<DashboardAttentionKind>,
    view: &mut DashboardView,
) {
    ui.label(
        egui::RichText::new("Attention queues · within this search")
            .small()
            .weak(),
    );
    let counts = search_matches.attention_counts();
    let choices = DashboardAttentionKind::ALL.map(|kind| {
        let count = counts.get(kind);
        (
            kind,
            format!("{} {count}", kind.label()),
            *attention == Some(kind),
        )
    });
    if let Some(kind) = dashboard_filter_chips(ui, "task_attention_queue", &choices, |kind| {
        format!(
            "Filter this search and project scope to {}. Search syntax: attention:{}",
            kind.label().to_lowercase(),
            kind.query_value()
        )
    }) {
        *attention = Some(kind);
        *view = DashboardView::All;
    }
}

#[cfg(test)]
fn filter_attention_queue(
    dashboard: &mut DashboardSnapshot,
    attention: Option<DashboardAttentionKind>,
) {
    if let Some(kind) = attention {
        dashboard
            .rows
            .retain(|row| row.attention_kind() == Some(kind));
    }
}

fn task_runtime_label(row: &TaskRowSnapshot) -> Option<&'static str> {
    if row.native_preparing {
        Some("Preparing Agent")
    } else if row.has_active_agent_stream {
        Some("Native session open")
    } else if row.has_agent_terminal {
        Some("Agent terminal available")
    } else if row.status.is_running() {
        Some("No attached terminal or native stream")
    } else {
        None
    }
}

fn task_validation_label(row: &TaskRowSnapshot) -> String {
    if row.validation_attempt == 0 {
        format!("Validation: {}", row.validation_status.label())
    } else {
        format!(
            "Validation #{}: {}",
            row.validation_attempt,
            row.validation_status.label(),
        )
    }
}

fn task_update_age(updated_at_ms: u64, now_ms: u64) -> String {
    if updated_at_ms == 0 {
        return "Update time unavailable".into();
    }
    let Some(elapsed) = now_ms.checked_sub(updated_at_ms) else {
        return "Update time is ahead of this clock".into();
    };
    match elapsed / 1_000 {
        0..60 => "Updated just now".into(),
        seconds @ 60..3_600 => format!("Updated {}m ago", seconds / 60),
        seconds @ 3_600..86_400 => format!("Updated {}h ago", seconds / 3_600),
        seconds => format!("Updated {}d ago", seconds / 86_400),
    }
}

fn task_row_marker(row: &TaskRowSnapshot) -> &'static str {
    if row.needs_attention {
        "!"
    } else if row.is_running() {
        "●"
    } else if row.status == TaskStatus::Completed {
        "✓"
    } else {
        "·"
    }
}

/// Lifecycle, validation, and an open runtime are separate facts. In
/// particular, a validation pass must not draw a completed-task checkmark.
fn task_row_button(
    ui: &mut egui::Ui,
    row: &TaskRowSnapshot,
    shared: &DashboardRow,
    selected: bool,
    now_ms: u64,
) -> egui::Response {
    ui.push_id(("task_overview", row.id), |ui| {
        let lifecycle = format!("{} · {}", row.provider.display_name(), row.status.label());
        let validation = task_validation_label(row);
        let runtime = task_runtime_label(row);
        let context = format!("{} · {}", row.project, row.branch);
        let age = task_update_age(shared.updated_at_ms, now_ms);
        let attention = shared.attention_kind();
        let freshness = match attention {
            Some(kind) => format!("{} · {age}", kind.label()),
            None => age,
        };
        let validation_and_runtime = match runtime {
            Some(runtime) => format!("{validation} · {runtime}"),
            None => validation.clone(),
        };
        let marker = task_row_marker(row);
        let accessible = format!(
            "{}. {lifecycle}. {validation_and_runtime}. {context}. {freshness}",
            row.title,
        );
        let row_spacing = 2.0;
        // Each laid-out line rounds up independently. Rounding only their
        // sum can crop the final descender when the user enlarges UI fonts.
        let card_height = (ui.text_style_height(&egui::TextStyle::Body).ceil()
            + 5.0 * ui.text_style_height(&egui::TextStyle::Small).ceil()
            + 5.0 * row_spacing
            + 14.0)
            .max(96.0);
        let response = ui.add_sized(
            [ui.available_width(), card_height],
            egui::Button::selectable(selected, ""),
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Button,
                ui.is_enabled(),
                selected,
                &accessible,
            )
        });
        let content = response.rect.shrink2(egui::vec2(8.0, 7.0));
        let detail_color = if selected {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().weak_text_color()
        };
        ui.scope_builder(
            egui::UiBuilder::new()
                .id(response.id.with("text"))
                .max_rect(content)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                ui.set_clip_rect(content.intersect(ui.clip_rect()));
                ui.spacing_mut().item_spacing.y = row_spacing;
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{marker} {}", row.title))
                            .strong()
                            .color(if selected {
                                detail_color
                            } else {
                                ui.visuals().text_color()
                            }),
                    )
                    .truncate(),
                );
                ui.add(
                    egui::Label::new(egui::RichText::new(&lifecycle).small().color(if selected {
                        detail_color
                    } else {
                        task_status_color(ui, row.status)
                    }))
                    .truncate(),
                );
                ui.add(
                    egui::Label::new(egui::RichText::new(&validation).small().color(if selected {
                        detail_color
                    } else {
                        task_validation_color(ui, row.validation_status)
                    }))
                    .truncate(),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(runtime.unwrap_or("No attached Agent runtime"))
                            .small()
                            .color(detail_color),
                    )
                    .truncate(),
                );
                ui.add(
                    egui::Label::new(egui::RichText::new(&context).small().color(detail_color))
                        .truncate(),
                );
                ui.add(
                    egui::Label::new(egui::RichText::new(&freshness).small().color(detail_color))
                        .truncate(),
                );
            },
        );
        response.on_hover_text(accessible)
    })
    .inner
}

fn clear_sent_follow_up(
    drafts: &mut HashMap<TaskId, String>,
    task_id: TaskId,
    accepted_text: &str,
) {
    if drafts
        .get(&task_id)
        .is_some_and(|draft| draft == accepted_text)
    {
        drafts.remove(&task_id);
    }
}

fn render_native_codex_view(
    ui: &mut egui::Ui,
    task_id: TaskId,
    provider: AgentProvider,
    view: &CodexAppServerViewSnapshot,
    approvals_enabled: bool,
    pending: &mut Option<TaskSidebarAction>,
    navigation: &mut Option<(TaskId, ApprovalId)>,
) {
    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!("Native {}", provider.display_name()))
                    .small()
                    .strong(),
            );
            ui.label(egui::RichText::new(view.phase.label()).small().weak());
            if view.dropped_updates > 0 {
                ui.label(
                    egui::RichText::new(format!("· This turn: {} updates omitted or compacted", view.dropped_updates))
                        .small()
                        .weak(),
                );
            }
            if let Some((kind, ordinal)) = native_flat_turn_heading(view) {
                ui.label(
                    egui::RichText::new(format!("· {kind} {ordinal}"))
                        .small()
                        .weak(),
                );
            }
        });

        if provider == AgentProvider::Codex {
            ui.horizontal_wrapped(|ui| {
                ui.strong("Saved conversation history");
                if ui.add_enabled(!view.history_loading, egui::Button::new("Refresh history")).clicked() {
                    *pending = Some(TaskSidebarAction::LoadHistory(task_id, true));
                }
                if view.history_cursor.is_some() && ui.add_enabled(!view.history_loading, egui::Button::new("Load earlier")).clicked() {
                    *pending = Some(TaskSidebarAction::LoadHistory(task_id, false));
                }
                if view.history_loading { ui.spinner(); }
            });
            for turn in view.persisted_history.iter() {
                egui::CollapsingHeader::new(format!("Saved turn · {}", turn.status))
                    .id_salt(("workspace-native-history", task_id, &turn.provider_turn_id))
                    .default_open(true).show(ui, |ui| {
                        if !turn.user_text.is_empty() { ui.weak("You"); ui.label(visible_bounded(&turn.user_text, 64 * 1024)); }
                        if !turn.agent_text.is_empty() { ui.weak("Agent"); ui.label(visible_bounded(&turn.agent_text, 128 * 1024)); }
                    });
            }
        }

        render_native_approvals(ui, task_id, view, approvals_enabled, pending, navigation);

        let history = visible_native_turn_history(view);
        if !history.is_empty() || view.dropped_turns > 0 {
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(format!("Completed turns ({})", history.len()))
                        .small()
                        .strong(),
                );
                if view.dropped_turns > 0 {
                    ui.label(
                        egui::RichText::new(format!(
                            "· {} earlier completed turn(s) no longer retained (history count/size limit)",
                            view.dropped_turns
                        ))
                        .small()
                        .weak(),
                    );
                }
            });
            for turn in history {
                render_native_turn_history(ui, task_id, turn);
            }
        }

        if let Some(feedback) = view.displayed_follow_up_feedback.as_deref() {
            ui.separator();
            egui::CollapsingHeader::new("Your feedback")
                .id_salt(("native-flat-feedback", task_id, view.displayed_turn_id))
                .default_open(true)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(("native-flat-feedback-scroll", task_id, view.displayed_turn_id))
                        .max_height(160.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(native_feedback_display(feedback)).small(),
                                )
                                .wrap(),
                            );
                        });
                });
        }

        if !view.agent_text.is_empty() {
            ui.separator();
            egui::CollapsingHeader::new("Agent response")
                .id_salt(("native-flat-response", task_id, view.displayed_turn_id))
                .default_open(true)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(visible_bounded(
                                &view.agent_text,
                                MAX_NATIVE_AGENT_TEXT_DISPLAY_BYTES,
                            ))
                            .small(),
                        )
                        .wrap(),
                    );
                    if view.agent_text_truncated {
                        ui.label(egui::RichText::new("Earlier text was compacted").small().weak());
                    }
                });
        }

        if !view.commands.is_empty() {
            egui::CollapsingHeader::new(format!("Commands ({})", view.commands.len()))
                .id_salt(("native-flat-commands", task_id, view.displayed_turn_id))
                .show(ui, |ui| {
                    let first = view.commands.len().saturating_sub(4);
                    for command in &view.commands[first..] {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {}",
                                command.status,
                                visible_bounded(&command.command, MAX_NATIVE_ITEM_DISPLAY_BYTES)
                            ))
                            .small()
                            .monospace(),
                        );
                    }
                });
        }

        if !view.file_changes.is_empty() {
            let change_count = view
                .file_changes
                .iter()
                .map(|item| item.changes.len())
                .sum::<usize>();
            ui.label(
                egui::RichText::new(format!("{} file changes reported", change_count))
                    .small()
                    .weak(),
            );
        }
        if let Some(error) = view.last_error.as_deref() {
            ui.label(
                egui::RichText::new(visible_bounded(error, MAX_NATIVE_ITEM_DISPLAY_BYTES))
                    .small()
                    .color(ui.visuals().error_fg_color),
            );
        }
    });
}

/// Navigation only: never stages an approval decision. The caller keeps this
/// strip outside the task-list ScrollArea and discards the target each frame.
fn native_approval_navigation(
    ui: &mut egui::Ui,
    task_id: TaskId,
    view: &CodexAppServerViewSnapshot,
) -> Option<(TaskId, ApprovalId)> {
    let first = view.pending_approvals.first()?;
    let mut target = None;
    // Include the first approval in identity so a replaced request cannot
    // inherit a pointer press that began on its predecessor's View button.
    ui.push_id(("native-approval-navigation", task_id, first.id), |ui| {
        ui.horizontal(|ui| {
            let response = ui.button("View");
            #[cfg(test)]
            record_approval_rect(ui, task_id, first.id, "view", &response);
            if response.clicked() {
                target = Some((task_id, first.id));
            }
            ui.add(
                egui::Label::new(format!(
                    "Selected task: {} pending approval(s)",
                    view.pending_approvals.len()
                ))
                .truncate(),
            );
        });
    });
    target
}

fn retain_approval_navigation(
    navigation: &mut Option<(TaskId, ApprovalId)>,
    selected: Option<TaskId>,
    view: Option<&CodexAppServerViewSnapshot>,
) {
    if navigation.is_some_and(|(task, approval)| {
        selected != Some(task)
            || !view.is_some_and(|view| {
                view.pending_approvals
                    .iter()
                    .any(|item| item.id == approval)
            })
    }) {
        *navigation = None;
    }
}

fn render_native_approvals(
    ui: &mut egui::Ui,
    task_id: TaskId,
    view: &CodexAppServerViewSnapshot,
    approvals_enabled: bool,
    pending: &mut Option<TaskSidebarAction>,
    navigation: &mut Option<(TaskId, ApprovalId)>,
) {
    for approval in &view.pending_approvals {
        ui.push_id(("native-approval", task_id, approval.id), |ui| {
            ui.separator();
            let heading = ui.label(
                egui::RichText::new(format!("{} approval", approval.kind.label()))
                    .small()
                    .strong()
                    .color(ui.visuals().warn_fg_color),
            );
            #[cfg(test)]
            record_approval_rect(ui, task_id, approval.id, "heading", &heading);
            if *navigation == Some((task_id, approval.id)) {
                heading.scroll_to_me(Some(egui::Align::Min));
                *navigation = None;
            }
            if let Some(command) = approval.command.as_deref() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(visible_bounded(command, MAX_NATIVE_ITEM_DISPLAY_BYTES))
                        .small()
                        .monospace(),
                    )
                    .wrap(),
                );
            }
            if let Some(cwd) = approval.cwd.as_deref() {
                ui.label(
                    egui::RichText::new(format!("cwd · {cwd}"))
                    .small()
                    .monospace()
                    .weak(),
                );
            }
            if let Some(reason) = approval.reason.as_deref() {
                ui.label(
                    egui::RichText::new(visible_bounded(
                        reason,
                        MAX_NATIVE_ITEM_DISPLAY_BYTES,
                    ))
                    .small(),
                );
            }
            if approval.kind == CodexAppServerApprovalKind::FileChange {
                ui.label(
                    egui::RichText::new("Exact patch requested by Codex")
                        .small()
                        .strong(),
                );
                egui::ScrollArea::vertical()
                    .id_salt((
                        "native-file-approval",
                        task_id,
                        view.displayed_turn_id,
                        approval.id,
                    ))
                    .max_height(280.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for change in &approval.file_changes {
                            ui.separator();
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · {}",
                                    change.kind, change.path
                                ))
                                .small()
                                .strong()
                                .monospace(),
                            );
                            if let Some(move_path) = change.move_path.as_deref() {
                                ui.label(
                                    egui::RichText::new(format!("moves to · {move_path}"))
                                        .small()
                                        .monospace(),
                                );
                            }
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&change.diff).small().monospace(),
                                )
                                .wrap(),
                            );
                        }
                    });
            }
            ui.label(
                egui::RichText::new(
                    "Allow is disabled: accepted provider actions cannot yet be bound to Ember's pinned workspace capability. Deny keeps the fixed workspace sandbox in force.",
                )
                .small()
                .color(ui.visuals().warn_fg_color),
            );
            ui.horizontal(|ui| {
                let allow = ui
                    .add_enabled(
                        native_approval_can_allow(approval, approvals_enabled),
                        egui::Button::new("Allow once"),
                    )
                    .on_hover_text("Approve only the exact patch displayed above")
                    .on_disabled_hover_text(
                        "Native approvals are display-and-deny only in this live session",
                    )
                    ;
                #[cfg(test)]
                record_approval_rect(ui, task_id, approval.id, "allow", &allow);
                if allow.clicked() {
                    *pending = Some(TaskSidebarAction::Approve(task_id, approval.id));
                }
                let deny = ui.add_enabled(approvals_enabled, egui::Button::new("Deny"));
                #[cfg(test)]
                record_approval_rect(ui, task_id, approval.id, "deny", &deny);
                if deny.clicked() {
                    *pending = Some(TaskSidebarAction::Deny(task_id, approval.id));
                }
            });
        });
    }
}

fn native_flat_turn_heading(view: &CodexAppServerViewSnapshot) -> Option<(&'static str, usize)> {
    let ordinal = view.displayed_turn_ordinal?;
    let kind = if view.completed_turns >= ordinal {
        "Latest turn"
    } else if matches!(
        view.phase,
        CodexAppServerPhase::Failed | CodexAppServerPhase::Ended
    ) {
        "Stopped turn"
    } else {
        "Current turn"
    };
    Some((kind, ordinal))
}

fn visible_native_turn_history(
    view: &CodexAppServerViewSnapshot,
) -> Vec<&CodexAppServerTurnHistory> {
    let mut seen = HashSet::with_capacity(view.turn_history.len());
    view.turn_history
        .iter()
        .filter(|turn| {
            Some(turn.local_turn_id) != view.displayed_turn_id && seen.insert(turn.local_turn_id)
        })
        .collect()
}

fn native_feedback_display(text: &str) -> String {
    sanitize_prompt_payload(
        text,
        NATIVE_AGENT_FOLLOW_UP_MAX_BYTES,
        VisualSpoofDisposition::Reject,
    )
    .map(|payload| payload.text)
    .unwrap_or_else(|_| visible_bounded(text, NATIVE_AGENT_FOLLOW_UP_MAX_BYTES))
}

fn render_native_turn_history(
    ui: &mut egui::Ui,
    task_id: TaskId,
    turn: &CodexAppServerTurnHistory,
) {
    let command_count = turn.commands.len();
    let file_count = turn.file_changes.iter().fold(0usize, |total, file| {
        total.saturating_add(file.change_count)
    });
    egui::CollapsingHeader::new(format!(
        "Turn {} · completed · {command_count} command(s) · {file_count} file change(s)",
        turn.ordinal
    ))
    .id_salt(("native-history-turn", task_id, turn.local_turn_id))
    .default_open(false)
    .show(ui, |ui| {
        if turn.dropped_updates > 0 {
            ui.label(
                egui::RichText::new(format!(
                    "This completed turn: {} update(s) omitted or compacted",
                    turn.dropped_updates
                ))
                .small()
                .weak(),
            );
        }
        if let Some(feedback) = turn.follow_up_feedback.as_deref() {
            ui.label(egui::RichText::new("Your feedback").small().strong());
            egui::ScrollArea::vertical()
                .id_salt(("native-history-feedback", task_id, turn.local_turn_id))
                .max_height(120.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(native_feedback_display(feedback)).small(),
                        )
                        .wrap(),
                    );
                });
        }
        if !turn.agent_text.is_empty() {
            ui.label(egui::RichText::new("Agent response").small().strong());
            egui::ScrollArea::vertical()
                .id_salt(("native-history-response", task_id, turn.local_turn_id))
                .max_height(180.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(visible_bounded(
                                &turn.agent_text,
                                MAX_NATIVE_AGENT_TEXT_DISPLAY_BYTES,
                            ))
                            .small(),
                        )
                        .wrap(),
                    );
                });
            if turn.agent_text_truncated {
                ui.label(
                    egui::RichText::new("Earlier response text was compacted")
                        .small()
                        .weak(),
                );
            }
        }
        if !turn.commands.is_empty() {
            ui.label(
                egui::RichText::new(format!("Commands ({command_count})"))
                    .small()
                    .strong(),
            );
            for command in &turn.commands {
                let omitted = if command.output_omitted {
                    " · output omitted"
                } else {
                    ""
                };
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {}{omitted}",
                        visible_bounded(&command.status, 64),
                        visible_bounded(&command.command, MAX_NATIVE_ITEM_DISPLAY_BYTES)
                    ))
                    .small()
                    .monospace(),
                );
            }
        }
        if !turn.file_changes.is_empty() {
            ui.label(
                egui::RichText::new(format!("File changes ({file_count})"))
                    .small()
                    .strong(),
            );
            for file in &turn.file_changes {
                let path = file
                    .path
                    .as_deref()
                    .map(|path| visible_bounded(path, MAX_NATIVE_ITEM_DISPLAY_BYTES))
                    .unwrap_or_else(|| "path unavailable".to_string());
                let compacted = if file.changes_truncated || file.path_truncated {
                    " · compacted"
                } else {
                    ""
                };
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {} change(s) · {path}{compacted}",
                        visible_bounded(&file.status, 64),
                        file.change_count
                    ))
                    .small()
                    .monospace(),
                );
            }
        }
    });
}

fn native_approval_can_allow(
    _approval: &crate::agent::CodexAppServerApproval,
    _runtime_accepts_decisions: bool,
) -> bool {
    false
}

fn native_follow_up_can_send(text: &str, completed_turns: usize) -> bool {
    !text
        .trim_matches(|character| matches!(character, ' ' | '\n' | '\t'))
        .is_empty()
        && text.len() <= NATIVE_AGENT_FOLLOW_UP_MAX_BYTES
        && completed_turns < CODEX_APP_SERVER_LIVE_TURN_MAX
}

impl TerminalApp {
    pub(crate) fn open_task_composer(&mut self) {
        let draft = &mut self.task_sidebar.composer;
        if draft.project.is_empty() {
            draft.project = self
                .task_sidebar
                .dashboard_project
                .as_ref()
                .or_else(|| {
                    self.task_sidebar
                        .selected
                        .and_then(|id| self.task_manager.get(id))
                        .map(|task| &task.repo_root)
                })
                .or_else(|| {
                    (!self.sidebar.location().is_remote()).then_some(&self.sidebar.current_dir)
                })
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        if draft.title.is_empty() && draft.goal.is_empty() {
            draft.provider = self.task_sidebar.dashboard_provider.unwrap_or_else(|| {
                AgentProvider::from_config_value(&self.config.preferred_fix_provider)
                    .unwrap_or(AgentProvider::Codex)
            });
            draft.validation_shell = std::env::var("EMBER_SHELL")
                .ok()
                .or(self.config.shell.clone())
                .filter(|path| std::path::Path::new(path).is_absolute())
                .unwrap_or_else(|| "/bin/sh".into());
        }
        draft.open = true;
    }

    pub(crate) fn begin_command_worktree_task(
        &mut self,
        context: crate::agent::SemanticCommandContext,
        provider: AgentProvider,
    ) -> Result<(), String> {
        provider
            .ensure_executable_available()
            .map_err(|error| error.to_string())?;
        let cwd = context
            .cwd
            .as_deref()
            .filter(|cwd| !cwd.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "source command has no working directory".to_string())?;
        let command = context.command.as_deref().unwrap_or("failed command");
        let title = format!("Fix {}", visible_bounded(command, 112));
        self.begin_worktree_task(TaskCreationRequest {
            cwd,
            title,
            provider,
            context: Some(context),
            brief: None,
            autostart: true,
        })
    }

    fn begin_worktree_task(&mut self, request: TaskCreationRequest) -> Result<(), String> {
        if self.task_sidebar.pending_creation.is_some() || self.task_sidebar.recovery_job.is_some()
        {
            return Err(
                "Another task worktree is still being created; your draft is preserved.".into(),
            );
        }
        let worktree_root = dirs::data_local_dir()
            .ok_or_else(|| "Cannot locate the per-user data directory".to_string())?
            .join("ember")
            .join("agent-tasks");
        let TaskCreationRequest {
            cwd,
            title,
            provider,
            context,
            brief,
            autostart,
        } = request;
        let authored = brief.is_some();
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker = std::thread::Builder::new()
            .name("ember-task-worktree".to_string())
            .spawn(move || {
                let result = (|| {
                    let service = crate::agent::WorktreeService::new(worktree_root)
                        .map_err(|_| "Cannot initialize the trusted worktree service; no task worktree was created.".to_string())?
                        .with_cancel_flag(worker_cancel);
                    let repository = service
                        .resolve_repository_root(&cwd)
                        .map_err(|_| "Cannot verify the source repository; no task worktree was created.".to_string())?;
                    let root = service.managed_root();
                    let mut journal = crate::agent::recovery::Journal::open(root)
                        .map_err(|error| crate::agent::recovery::error_message(&error))?;
                    let record = crate::agent::recovery::Record::requested(root, repository, provider);
                    // Write-ahead intent must be durable BEFORE Git can create anything.
                    journal.request(record.clone())
                        .map_err(|error| crate::agent::recovery::error_message(&error))?;
                    let request = crate::agent::CreateWorktreeRequest::new(
                        &record.repository, record.task_name(), &record.branch, "HEAD",
                    );
                    let worktree = service
                        .create(&request)
                        .map_err(|_| "Worktree creation did not finish. Inspect the recovery record; no automatic cleanup was attempted.".to_string())?;
                    let record = journal.prepared(record.id, &worktree)
                        .map_err(|error| crate::agent::recovery::error_message(&error))?;
                    Ok(PreparedTask {
                        context,
                        brief,
                        autostart,
                        title,
                        provider,
                        record,
                        journal,
                    })
                })();
                let _ = sender.send(result);
            })
            .map_err(|error| format!("could not start task worktree worker: {error}"))?;

        self.task_sidebar.pending_creation = Some(PendingTaskCreation {
            receiver,
            worker: Some(worker),
            cancel,
            authored,
        });
        // The new task and its "Creating…" progress belong on the dashboard.
        self.task_sidebar.show_dashboard();
        self.task_sidebar.recovery_error = None;
        if authored {
            self.task_sidebar.composer.error = None;
        }
        self.sidebar.note_files_user_intent();
        self.sidebar.visible = true;
        self.sidebar.view = crate::sidebar::SidebarView::Tasks;
        self.config.sidebar_view = crate::sidebar::SidebarView::Tasks;
        self.schedule_config_save();
        self.set_status(format!(
            "Creating an isolated Git worktree for {}…",
            provider.display_name()
        ));
        Ok(())
    }

    /// Poll before PTY routing snapshots are taken: a completed worktree may
    /// add and activate a task terminal later in the same frame.
    pub(crate) fn poll_task_creation(&mut self, ctx: &egui::Context) {
        self.poll_task_recovery(ctx);
        let authored = self
            .task_sidebar
            .pending_creation
            .as_ref()
            .is_some_and(|pending| pending.authored);
        let result = self
            .task_sidebar
            .pending_creation
            .as_ref()
            .map(|pending| pending.receiver.try_recv());
        match result {
            None => {}
            Some(Err(TryRecvError::Empty)) => {
                ctx.request_repaint_after(Duration::from_millis(75));
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.task_sidebar.pending_creation = None;
                self.task_sidebar.recovery_error = Some("Task worktree worker stopped unexpectedly. Inspect its recovery record; no worktree cleanup was attempted.".into());
                if authored {
                    self.task_sidebar.composer.error = self.task_sidebar.recovery_error.clone();
                }
                self.task_sidebar.recovery_loaded = false;
            }
            Some(Ok(result)) => {
                // The result is sent only after the mutating operation has
                // completed; dropping its finished handle is nonblocking.
                self.task_sidebar.pending_creation = None;
                match result {
                    Err(error) => {
                        if authored {
                            self.task_sidebar.composer.error = Some(error.clone());
                        }
                        self.task_sidebar.recovery_error = Some(error);
                        self.task_sidebar.recovery_loaded = false;
                    }
                    Ok(prepared) => {
                        let task = crate::agent::NewTask {
                            title: prepared.title,
                            provider: prepared.provider,
                            repo_root: prepared.record.repository.clone(),
                            worktree_path: prepared.record.path.clone(),
                            branch: prepared.record.branch.clone(),
                            base_commit: prepared.record.base_commit.clone().unwrap_or_default(),
                            source_context: prepared.context,
                        };
                        self.register_prepared_task(
                            prepared.journal,
                            prepared.record,
                            task,
                            prepared.brief,
                            prepared.autostart,
                        );
                    }
                }
            }
        }
    }

    fn start_recovery_job(
        &mut self,
        job: impl FnOnce() -> Result<RecoveryResult, String> + Send + 'static,
    ) {
        if self.task_sidebar.recovery_job.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("ember-task-recovery".into())
            .spawn(move || {
                let _ = sender.send(job());
            }) {
            Ok(_) => self.task_sidebar.recovery_job = Some(receiver),
            Err(_) => {
                self.task_sidebar.recovery_error = Some(
                    "Could not start recovery worker; no recovery action was performed.".into(),
                )
            }
        }
    }

    fn register_prepared_task(
        &mut self,
        mut journal: crate::agent::recovery::Journal,
        record: crate::agent::recovery::Record,
        task: crate::agent::NewTask,
        brief: Option<TaskBrief>,
        autostart: bool,
    ) {
        self.task_sidebar.recovery_records = journal.records().to_vec();
        let authored = brief.is_some();
        match crate::agent::recovery::register_with_brief(
            &mut self.task_manager,
            &record,
            task,
            brief,
        ) {
            Ok(id) => {
                self.task_sidebar.selected = Some(id);
                self.task_sidebar.scroll_to_selected = true;
                self.task_sidebar.show_dashboard();
                if authored {
                    self.task_sidebar.dashboard_query.clear();
                    self.task_sidebar.dashboard_view = jterm_core::agent_task::DashboardView::All;
                    self.task_sidebar.dashboard_provider = None;
                    self.task_sidebar.dashboard_project = None;
                    self.task_sidebar.composer.open = false;
                    self.task_sidebar.composer.title.clear();
                    self.task_sidebar.composer.goal.clear();
                    self.task_sidebar.composer.validation_command.clear();
                    self.task_sidebar.composer.error = None;
                }
                self.task_sidebar.unconfirmed_registrations.insert(id);
                self.start_recovery_job(move || {
                    // Core registration is in memory; this marker is a separate commit.
                    // No provider starts until the marker write succeeds.
                    journal
                        .registered(record.id, id)
                        .map_err(|e| crate::agent::recovery::error_message(&e))?;
                    Ok(RecoveryResult::Registered(
                        journal.records().to_vec(),
                        id,
                        record.provider,
                        autostart,
                    ))
                });
            }
            Err(error) => {
                self.task_sidebar.recovery_records = journal.records().to_vec();
                if authored {
                    self.task_sidebar.composer.error = Some(error.clone());
                }
                self.task_sidebar.recovery_error = Some(error);
            }
        }
    }

    fn poll_task_recovery(&mut self, ctx: &egui::Context) {
        if !self.task_sidebar.recovery_loaded
            && self.task_sidebar.recovery_job.is_none()
            && self.task_sidebar.pending_creation.is_none()
        {
            self.task_sidebar.recovery_loaded = true;
            self.start_recovery_job(|| {
                let journal = crate::agent::recovery::Journal::open(&recovery_root()?)
                    .map_err(|e| crate::agent::recovery::error_message(&e))?;
                Ok(RecoveryResult::Loaded(journal.records().to_vec()))
            });
        }
        let result = self
            .task_sidebar
            .recovery_job
            .as_ref()
            .map(|r| r.try_recv());
        match result {
            Some(Err(TryRecvError::Empty)) => ctx.request_repaint_after(Duration::from_millis(75)),
            Some(result) => {
                self.task_sidebar.recovery_job = None;
                let result = result.unwrap_or_else(|_| {
                    Err("Recovery worker stopped. Reload to inspect the durable record.".into())
                });
                match result {
                    Err(error) => {
                        self.task_sidebar.recovery_error = Some(error);
                        // Keep the message visible; Reload is explicit, not an endless I/O retry.
                    }
                    Ok(RecoveryResult::Loaded(records)) => {
                        let another_runtime_won = self
                            .task_sidebar
                            .unconfirmed_registrations
                            .iter()
                            .any(|id| {
                                self.task_manager.get(*id).is_some_and(|task| {
                                    records.iter().any(|record| {
                                        record.stage == crate::agent::recovery::Stage::Registered
                                            && recovery_record_matches_task(task, record)
                                            && record.registered_task_id != Some(task.id)
                                    })
                                })
                            });
                        if another_runtime_won {
                            self.task_sidebar.recovery_error = Some("Another app window confirmed this worktree's registration. Use that window or Open worktree; archive this window's unconfirmed task rather than starting a duplicate.".into());
                        }
                        let confirmed = |id: &TaskId| {
                            self.task_manager.get(*id).is_some_and(|task| {
                                records
                                    .iter()
                                    .any(|r| registration_marker_confirms(task, r))
                            })
                        };
                        self.task_sidebar
                            .unconfirmed_registrations
                            .retain(|id| !confirmed(id));
                        self.task_sidebar
                            .hidden_unconfirmed_registrations
                            .retain(|id| !confirmed(id));
                        self.task_sidebar.recovery_records = records;
                    }
                    Ok(RecoveryResult::Open(path, intent, epoch)) => {
                        if !self.sidebar.files_intent_is_current(&intent)
                            || self.sidebar.files_user_intent_generation() != epoch
                        {
                            // A later Files action superseded this read-only verification.
                            return;
                        }
                        if self.sidebar.location().is_remote() {
                            self.task_sidebar.recovery_error = Some("Switch Files to Local before opening this local worktree, or use Copy path.".into());
                        } else if let Some(error) = self.sidebar.set_current_dir(path) {
                            self.task_sidebar.recovery_error = Some(error);
                        } else {
                            self.sidebar.view = crate::sidebar::SidebarView::Files;
                            self.config.sidebar_view = crate::sidebar::SidebarView::Files;
                        }
                    }
                    Ok(RecoveryResult::Retry(prepared)) => {
                        let (journal, record) = *prepared;
                        match record.new_task() {
                            Ok(task) => {
                                self.register_prepared_task(journal, record, task, None, false)
                            }
                            Err(error) => self.task_sidebar.recovery_error = Some(error),
                        }
                    }
                    Ok(RecoveryResult::Registered(records, id, provider, autostart)) => {
                        self.task_sidebar.recovery_records = records;
                        self.task_sidebar.unconfirmed_registrations.remove(&id);
                        self.task_sidebar
                            .hidden_unconfirmed_registrations
                            .remove(&id);
                        self.task_sidebar.recovery_error = None;
                        self.set_status(if self.task_manager.get(id).is_some_and(|task| task.brief.is_some()) {
                            "Task created. Review its goal and choose Start when you are ready."
                        } else {
                            "Task registered. Recovered tasks require an explicit Start; command context was not saved."
                        });
                        if registration_may_autostart(&self.task_manager, id, autostart) {
                            match provider {
                                AgentProvider::Claude | AgentProvider::Kimi => {
                                    self.start_task_native_agent(id)
                                }
                                AgentProvider::OpenCode => self.start_task_agent_terminal(id),
                                AgentProvider::Codex => self.set_status(
                                    "Created an isolated Codex task; choose Start Codex",
                                ),
                            }
                        }
                    }
                }
            }
            None => {}
        }
    }

    fn render_task_recovery(&mut self, ui: &mut egui::Ui) {
        use crate::agent::recovery::{Journal, Stage};
        let busy = self.task_sidebar.pending_creation.is_some()
            || self.task_sidebar.recovery_job.is_some();
        let mut action = None;
        if self.task_sidebar.recovery_error.is_some()
            || !self.task_sidebar.recovery_records.is_empty()
        {
            egui::CollapsingHeader::new("Worktree recovery records").default_open(true).show(ui, |ui| {
                ui.label("These records preserve worktree locations, not task sessions or command context.");
                if let Some(error) = &self.task_sidebar.recovery_error {
                    let text = visible_bounded(error, 1024);
                    ui.colored_label(ui.visuals().warn_fg_color, &text);
                    if ui.button("Copy error").clicked() { ui.ctx().copy_text(text); }
                }
                if ui.add_enabled(!busy, egui::Button::new("Reload recovery records")).clicked() {
                    self.task_sidebar.recovery_loaded = false;
                    self.task_sidebar.recovery_error = None;
                }
                for record in &self.task_sidebar.recovery_records {
                    let dismiss_blocked = recovery_dismiss_blocked(&self.task_manager, &self.task_sidebar.unconfirmed_registrations, record);
                    ui.separator();
                    ui.label(visible_bounded(&record.path.to_string_lossy(), 4096));
                    ui.label(match record.stage {
                        Stage::Requested => "Creation interrupted; original base commit is unknown. Open to inspect; automatic retry is unsafe.",
                        Stage::Prepared => "Worktree prepared; task registration is unconfirmed. Retry reuses this worktree without recreating its branch.",
                        Stage::Registered => "Previously registered. Runtime sessions were not saved. Open to inspect; this record will not recreate a completed task.",
                    });
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!busy && record.stage == Stage::Prepared, egui::Button::new("Retry registration")).clicked() {
                            action = Some((record.id, 0));
                        }
                        if ui.add_enabled(!busy, egui::Button::new("Open worktree")).clicked() { action = Some((record.id, 1)); }
                        if ui.button("Copy path").clicked() { ui.ctx().copy_text(visible_bounded(&record.path.to_string_lossy(), 4096)); }
                        if ui.add_enabled(!busy && !dismiss_blocked, egui::Button::new("Dismiss record…")).clicked() { self.task_sidebar.dismiss_recovery = Some(record.id); }
                    });
                    if dismiss_blocked { ui.label("Retry registration or Reload before dismissing this task's confirmation record."); }
                    if self.task_sidebar.dismiss_recovery == Some(record.id) {
                        ui.label("Forget this recovery record? Worktree, branch and files stay untouched; this removes its recovery entry.");
                        if let Some(task) = hidden_task_waiting_on_record(&self.task_manager, &self.task_sidebar.hidden_unconfirmed_registrations, record) {
                            ui.colored_label(ui.visuals().warn_fg_color, format!(
                                "The hidden task “{}” still waits for this record to confirm its registration. After dismissing it, that task can be restored for review but never started.",
                                visible_bounded(&task.title, MAX_TASK_TITLE_DISPLAY_BYTES)
                            ));
                        }
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!busy && !dismiss_blocked, egui::Button::new("Dismiss record only")).clicked() { action = Some((record.id, 2)); self.task_sidebar.dismiss_recovery = None; }
                            if ui.button("Cancel").clicked() { self.task_sidebar.dismiss_recovery = None; }
                        });
                    }
                }
            });
        }
        if let Some((id, operation)) = action {
            if operation == 2
                && self
                    .task_sidebar
                    .recovery_records
                    .iter()
                    .find(|r| r.id == id)
                    .is_some_and(|r| {
                        recovery_dismiss_blocked(
                            &self.task_manager,
                            &self.task_sidebar.unconfirmed_registrations,
                            r,
                        )
                    })
            {
                self.task_sidebar.recovery_error = Some("Retry registration or Reload before dismissing this task's confirmation record.".into());
                return;
            }
            if operation == 1 {
                self.sidebar.note_files_user_intent();
            }
            let intent = self.sidebar.files_intent_context();
            let epoch = self.sidebar.files_user_intent_generation();
            self.start_recovery_job(move || {
                let root = recovery_root()?;
                let mut journal =
                    Journal::open(&root).map_err(|e| crate::agent::recovery::error_message(&e))?;
                let record = journal
                    .records()
                    .iter()
                    .find(|r| r.id == id)
                    .cloned()
                    .ok_or("Record changed; Reload before trying again")?;
                if operation == 2 {
                    journal
                        .dismiss(id)
                        .map_err(|e| crate::agent::recovery::error_message(&e))?;
                    return Ok(RecoveryResult::Loaded(journal.records().to_vec()));
                }
                record.verify(&root)?;
                if operation == 0 {
                    Ok(RecoveryResult::Retry(Box::new((journal, record))))
                } else {
                    Ok(RecoveryResult::Open(record.path, Box::new(intent), epoch))
                }
            });
        }
    }

    pub(crate) fn render_sidebar_tasks(&mut self, ui: &mut egui::Ui) {
        self.render_task_recovery(ui);
        let hidden_count = self.task_manager.archived_count();
        if !hidden_view_shown(self.task_sidebar.show_hidden, hidden_count) {
            // For example after restoring the last hidden task.
            self.task_sidebar.show_dashboard();
        }
        ui.horizontal_wrapped(|ui| {
            ui.strong("Agent tasks");
            if ui.button("New task").clicked() {
                self.open_task_composer();
            }
            if ui
                .button("Conversation library")
                .on_hover_text("Open the Agent Sessions library; it stays open on a second click")
                .clicked()
            {
                self.open_agent_sessions();
            }
            if (hidden_count > 0 || self.task_sidebar.show_hidden)
                && ui
                    .selectable_label(
                        self.task_sidebar.show_hidden,
                        format!("Hidden ({hidden_count})"),
                    )
                    .on_hover_text(if self.task_sidebar.show_hidden {
                        "Return to the task dashboard"
                    } else {
                        "Show hidden tasks. Restore returns a task to the state it had when hidden"
                    })
                    .clicked()
            {
                self.task_sidebar.show_hidden = !self.task_sidebar.show_hidden;
            }
        });
        let mut composer_projects: Vec<_> = self
            .task_manager
            .tasks()
            .iter()
            .map(|task| task.repo_root.clone())
            .collect();
        composer_projects.sort();
        composer_projects.dedup();
        let creation_busy = self.task_sidebar.pending_creation.is_some()
            || self.task_sidebar.recovery_job.is_some();
        if let Some(request) = show_task_composer(
            ui.ctx(),
            &mut self.task_sidebar.composer,
            &composer_projects,
            creation_busy,
        ) {
            if let Err(error) = self.begin_worktree_task(request) {
                self.task_sidebar.composer.error = Some(error);
            }
        }
        if self.task_sidebar.show_hidden {
            self.render_hidden_tasks(ui);
            return;
        }
        let rows: Vec<_> = self
            .task_manager
            .tasks()
            .iter()
            .filter(|task| {
                task.status != TaskStatus::Archived
                    && self
                        .agent_sessions
                        .workspace_task
                        .is_none_or(|id| id == task.id)
            })
            .map(|task| TaskRowSnapshot {
                id: task.id,
                title: visible_bounded(&task.title, MAX_TASK_TITLE_DISPLAY_BYTES),
                provider: task.provider,
                status: task.status,
                runtime_kind: task.runtime_kind,
                branch: visible_bounded(&task.branch, MAX_TASK_BRANCH_DISPLAY_BYTES),
                project: visible_bounded(&task.repo_root.to_string_lossy(), 320),
                has_agent_terminal: task
                    .terminal_session_id
                    .as_deref()
                    .is_some_and(|session_id| self.session_manager.index_of(session_id).is_some()),
                has_validation_terminal: task
                    .validation
                    .terminal_session_id
                    .as_deref()
                    .is_some_and(|session_id| self.session_manager.index_of(session_id).is_some()),
                has_active_agent_stream: self.task_manager.has_active_agent_event_stream(task.id),
                native_preparing: self.agent_runtime.has_preparing(task.id),
                terminal_retry_available: self
                    .task_manager
                    .terminal_retry_session_id(task.id)
                    .is_ok(),
                native_terminal_fallback_available: self
                    .task_manager
                    .native_terminal_fallback_eligible(task.id)
                    .is_ok()
                    && self.agent_runtime.can_continue_in_terminal(task.id),
                validation_status: task.validation.status,
                validation_attempt: task.validation.attempt,
                validation_detail: task
                    .validation
                    .status_detail
                    .as_deref()
                    .map(|detail| visible_bounded(detail, MAX_TASK_DETAIL_DISPLAY_BYTES)),
                needs_attention: self.task_manager.task_needs_attention(task.id),
                status_detail: task
                    .status_detail
                    .as_deref()
                    .map(|detail| visible_bounded(detail, MAX_TASK_DETAIL_DISPLAY_BYTES)),
            })
            .collect();
        let has_tasks = !rows.is_empty();
        let open_result = dashboard_search(
            ui,
            &mut self.task_sidebar.dashboard_query,
            &mut self.task_sidebar.query_error,
        );
        let mut projects: Vec<_> = self
            .task_manager
            .tasks()
            .iter()
            .filter(|task| task.status != TaskStatus::Archived)
            .map(|task| task.repo_root.clone())
            .collect();
        projects.sort();
        projects.dedup();
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("task_dashboard_provider")
                .selected_text(
                    self.task_sidebar
                        .dashboard_provider
                        .map_or("All providers", AgentProvider::display_name),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.task_sidebar.dashboard_provider,
                        None,
                        "All providers",
                    );
                    for provider in AgentProvider::ALL {
                        ui.selectable_value(
                            &mut self.task_sidebar.dashboard_provider,
                            Some(provider),
                            provider.display_name(),
                        );
                    }
                });
            egui::ComboBox::from_id_salt("task_dashboard_project")
                .selected_text(self.task_sidebar.dashboard_project.as_ref().map_or_else(
                    || "All projects".to_string(),
                    |path| visible_bounded(&path.to_string_lossy(), 120),
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.task_sidebar.dashboard_project,
                        None,
                        "All projects",
                    );
                    for project in &projects {
                        ui.selectable_value(
                            &mut self.task_sidebar.dashboard_project,
                            Some(project.clone()),
                            visible_bounded(&project.to_string_lossy(), 320),
                        );
                    }
                });
            egui::ComboBox::from_id_salt("task_dashboard_sort")
                .selected_text(self.task_sidebar.dashboard_sort.label())
                .show_ui(ui, |ui| {
                    for sort in jterm_core::agent_task::DashboardSort::ALL {
                        ui.selectable_value(
                            &mut self.task_sidebar.dashboard_sort,
                            sort,
                            sort.label(),
                        );
                    }
                });
        });
        let activity: HashMap<_, _> = rows
            .iter()
            .map(|row| {
                (
                    row.id,
                    jterm_core::agent_task::DashboardActivity {
                        has_agent_terminal: row.has_agent_terminal,
                        has_validation_terminal: row.has_validation_terminal,
                        has_active_agent_stream: row.has_active_agent_stream,
                        native_preparing: row.native_preparing,
                    },
                )
            })
            .collect();
        let search_matches = jterm_core::agent_task::DashboardSnapshot::project_unsorted(
            self.task_manager
                .tasks()
                .iter()
                .filter(|task| task.status != TaskStatus::Archived),
            |id| activity.get(&id).copied().unwrap_or_default(),
            jterm_core::agent_task::DashboardFilter {
                text: &self.task_sidebar.dashboard_query,
                provider: self.task_sidebar.dashboard_provider,
                repo_root: self.task_sidebar.dashboard_project.as_deref(),
                view: DashboardView::All,
                ..Default::default()
            },
        );
        let views = [
            DashboardView::All,
            DashboardView::NeedsAttention,
            DashboardView::Running,
            DashboardView::NotStarted,
            DashboardView::Review,
            DashboardView::Finished,
        ]
        .map(|view| {
            (
                view,
                dashboard_filter_label(view, &search_matches.counts),
                self.task_sidebar.dashboard_attention.is_none()
                    && self.task_sidebar.dashboard_view == view,
            )
        });
        if let Some(view) = dashboard_filter_chips(ui, "task_status_views", &views, |_| {
            "Filter tasks within the current search and project scope.".into()
        }) {
            self.task_sidebar.dashboard_view = view;
            self.task_sidebar.dashboard_attention = None;
        }
        render_attention_queues(
            ui,
            &search_matches,
            &mut self.task_sidebar.dashboard_attention,
            &mut self.task_sidebar.dashboard_view,
        );
        // Narrow the searched scope in place; counts stay those of the search.
        let mut dashboard = search_matches;
        dashboard.apply_view(
            self.task_sidebar.dashboard_view,
            self.task_sidebar.dashboard_attention,
            self.task_sidebar.dashboard_sort,
        );
        if open_result {
            if let Some(row) = dashboard.rows.first() {
                self.task_sidebar.selected = Some(row.id);
                self.task_sidebar.scroll_to_selected = true;
            }
        }
        ui.label(
            egui::RichText::new(format!(
                "{} shown · {} search matches · {} need you · {} awaiting approval",
                dashboard.rows.len(),
                dashboard.counts.total,
                dashboard.counts.attention,
                dashboard.counts.pending_approval,
            ))
            .small()
            .color(ui.visuals().weak_text_color()),
        );
        let selected_hidden = self.task_sidebar.selected.is_some_and(|id| {
            rows.iter().any(|row| row.id == id) && dashboard.selected_index(Some(id)).is_none()
        });
        let mut clear_filters = false;
        if !self.task_sidebar.dashboard_query.is_empty()
            || self.task_sidebar.dashboard_view != jterm_core::agent_task::DashboardView::All
            || self.task_sidebar.dashboard_provider.is_some()
            || self.task_sidebar.dashboard_project.is_some()
            || self.task_sidebar.dashboard_attention.is_some()
        {
            clear_filters |= ui.button("Clear filters").clicked();
        }
        if selected_hidden {
            ui.label("Selected task is hidden by filters.");
            clear_filters |= ui.button("Show selected task").clicked();
        }
        if clear_filters {
            self.task_sidebar.dashboard_query.clear();
            self.task_sidebar.dashboard_view = jterm_core::agent_task::DashboardView::All;
            self.task_sidebar.dashboard_provider = None;
            self.task_sidebar.dashboard_project = None;
            self.task_sidebar.dashboard_attention = None;
            self.task_sidebar.scroll_to_selected = true;
            ui.ctx().request_repaint();
        }
        ui.horizontal_wrapped(|ui| {
            let selected_project = self.task_sidebar.selected
                .filter(|id| dashboard.selected_index(Some(*id)).is_some())
                .and_then(|id| self.task_manager.get(id))
                .map(|task| task.repo_root.clone());
            if ui.add_enabled(selected_project.is_some(), egui::Button::new("Selected project"))
                .on_hover_text("Narrow to the selected task's exact project; keep the current search and provider")
                .clicked()
            {
                self.task_sidebar.dashboard_project = selected_project;
                self.task_sidebar.scroll_to_selected = true;
                ui.ctx().request_repaint();
            }
            let previous = dashboard.step_selection(self.task_sidebar.selected, false);
            let next = dashboard.step_selection(self.task_sidebar.selected, true);
            if ui
                .add_enabled(previous.is_some(), egui::Button::new("Previous"))
                .clicked()
            {
                self.task_sidebar.selected = previous;
                self.task_sidebar.scroll_to_selected = true;
            }
            if ui
                .add_enabled(next.is_some(), egui::Button::new("Next"))
                .clicked()
            {
                self.task_sidebar.selected = next;
                self.task_sidebar.scroll_to_selected = true;
            }
            let previous_attention = dashboard.previous_attention(self.task_sidebar.selected);
            if ui
                .add_enabled(
                    previous_attention.is_some(),
                    egui::Button::new("Previous needs you"),
                )
                .clicked()
            {
                self.task_sidebar.selected = previous_attention;
                self.task_sidebar.scroll_to_selected = true;
            }
            let attention = dashboard.next_attention(self.task_sidebar.selected);
            if ui
                .add_enabled(attention.is_some(), egui::Button::new("Next needs you"))
                .clicked()
            {
                self.task_sidebar.selected = attention;
                self.task_sidebar.scroll_to_selected = true;
            }
        });
        let rows = rows_in_dashboard_order(rows, &dashboard);
        let row_details: HashMap<_, _> = dashboard.rows.iter().map(|row| (row.id, row)).collect();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if has_tasks && rows.is_empty() {
            ui.label("No tasks match these filters.");
            return;
        }

        if rows.is_empty() {
            ui.add_space(8.0);
            if self.task_sidebar.pending_creation.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Creating isolated worktree…");
                });
                ui.ctx().request_repaint_after(Duration::from_millis(75));
                return;
            }
            if hidden_count > 0 {
                ui.label(egui::RichText::new("No visible Agent tasks").strong());
                ui.label(
                    egui::RichText::new(format!(
                        "{hidden_count} hidden. Choose Hidden to review or restore them, or New task to start another goal.",
                    ))
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
                return;
            }
            ui.label(egui::RichText::new("No Agent tasks yet").strong());
            ui.label(
                egui::RichText::new(
                    "Choose New task to describe a goal for any local Git project, or use Fix on a failed command block. Each task gets its own isolated worktree.",
                )
                .small()
                .color(ui.visuals().weak_text_color()),
            );
            return;
        }

        // Shared snapshots are reused until the provider view changes, so a
        // frame never deep-copies agent text, command output or diffs.
        let mut selected_native_view = self
            .task_sidebar
            .selected
            .filter(|id| rows.iter().any(|row| row.id == *id))
            .and_then(|id| {
                self.agent_runtime
                    .shared_snapshot(id)
                    .map(|view| (id, view))
            });
        let mut approval_navigation = selected_native_view
            .as_ref()
            .and_then(|(id, view)| native_approval_navigation(ui, *id, view));
        let mut pending = None;
        let native_ai_enabled = self.config.ai_enabled && self.config.ai_share_command_context;
        egui::ScrollArea::vertical()
            // An explicit approval jump must not leave an animated numeric
            // target that can outlive its exact task/request identity.
            .animated(approval_navigation.is_none())
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for row in &rows {
                    let selected = self.task_sidebar.selected == Some(row.id);
                    let Some(shared) = row_details.get(&row.id) else { continue; };
                    let response = task_row_button(ui, row, shared, selected, now_ms);
                    if selected && self.task_sidebar.scroll_to_selected {
                        response.scroll_to_me(Some(egui::Align::Min));
                        self.task_sidebar.scroll_to_selected = false;
                    }
                    if response.clicked() {
                        self.task_sidebar.selected = Some(row.id);
                    }

                    if let Some(reason) = self.task_manager.get(row.id)
                        .and_then(|task| TaskGuidance::from_task(task, shared.activity).reason) {
                        let preview = jterm_core::review_input::safe_inline_display(&reason, 1024);
                        ui.horizontal(|ui| {
                            ui.add_space(18.0);
                            ui.add(egui::Label::new(egui::RichText::new(preview).small().weak()).truncate())
                                .on_hover_text(&reason);
                        });
                    }

                    if selected {
                        ui.push_id(("selected_task_card", row.id), |ui| {
                        let task = self.task_manager.get(row.id);
                        let authored_task = task.is_some_and(|task| task.brief.is_some());
                        let manual_review = task.is_some_and(|task| task.requires_manual_review());
                        let has_input = task.is_some_and(|task| task.brief.is_some() || task.source_context.is_some());
                        let has_validation = task.is_some_and(|task| task.validation_command().is_some());
                        let registration_pending = self.task_sidebar.unconfirmed_registrations.contains(&row.id);
                        // Only a settled, successful record load can prove that
                        // no record is left to confirm this registration.
                        let records_settled = self.task_sidebar.recovery_loaded
                            && self.task_sidebar.recovery_job.is_none()
                            && self.task_sidebar.pending_creation.is_none()
                            && self.task_sidebar.recovery_error.is_none();
                        if let Some(task) = task {
                            let orphaned = registration_pending
                                && records_settled
                                && !registration_confirmable(task, &self.task_sidebar.recovery_records);
                            render_task_guidance(ui, &task_guidance(task, row, registration_pending, orphaned));
                        }
                        if let Some(notice) = native_start_notice(row.provider) {
                            ui.label(egui::RichText::new(notice).small().color(egui::Color32::YELLOW));
                        }
                        // Reuse the strip snapshot for this task. If selection changed
                        // during this frame, do not reuse another task's snapshot/target.
                        let native_view = if selected_native_view.as_ref().is_some_and(|(id, _)| *id == row.id) {
                            selected_native_view.take().map(|(_, view)| view)
                        } else {
                            self.agent_runtime.shared_snapshot(row.id)
                        };
                        retain_approval_navigation(&mut approval_navigation,
                            self.task_sidebar.selected.filter(|id| *id == row.id), native_view.as_deref());
                        let hide_blocked = hide_task_blocked_reason(row, self.task_manager.can_archive(row.id));
                        let native_idle = row.status == TaskStatus::ReadyForReview
                            && row.has_active_agent_stream
                            && native_view
                                .as_ref()
                                .is_some_and(|view| view.phase == CodexAppServerPhase::Ready);
                        task_action_scope(ui, row, manual_review, registration_pending, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            if self.agent_runtime.can_resume_codex(row.id) && ui.add_enabled(native_ai_enabled && !registration_pending, egui::Button::new("Resume native")).on_hover_text("Resume this task's exact saved Codex conversation and private profile").clicked() {
                                pending = Some(TaskSidebarAction::StartNative(row.id));
                            }
                            if row.status == TaskStatus::Created
                                && row.runtime_kind == TaskRuntimeKind::Unassigned
                                && !row.native_preparing
                            {
                                let start_label = format!("Start {}", row.provider.display_name());
                                if row.provider.supports_native_driver() {
                                    let native_hover = match row.provider {
                                        AgentProvider::Codex => {
                                            "Start a native Codex app-server session. Review points can continue on the same loaded thread; finish the session before validation. Agent tool writes are restricted to this worktree, while the current Codex sandbox may read other host files."
                                        }
                                        AgentProvider::Claude => {
                                            "Start a native Claude conversation with permission checks. Continue tool approval requests in Terminal."
                                        }
                                        AgentProvider::Kimi => {
                                            "Start a native Kimi Code print/stream-json session with automatic tool permission. No private home or cgroup containment is provided; the worktree is not a sandbox."
                                        }
                                        AgentProvider::OpenCode => {
                                            "Start the native provider session."
                                        }
                                    };
                                    if ui
                                        .add_enabled(
                                            native_ai_enabled && has_input && !registration_pending,
                                            egui::Button::new(&start_label),
                                        )
                                        .on_disabled_hover_text(if registration_pending {
                                            "Wait for durable registration confirmation before starting the Agent"
                                        } else if !has_input {
                                            "Recovery did not save the task goal or command evidence. Create a new goal task before native Start"
                                        } else {
                                            "Enable AI features and cloud command-context sharing in Settings → AI first"
                                        })
                                        .on_hover_text(native_hover)
                                        .clicked()
                                    {
                                        pending = Some(TaskSidebarAction::StartNative(row.id));
                                    }
                                    if ui
                                        .add_enabled(!registration_pending && (!authored_task || native_ai_enabled), egui::Button::new("Terminal fallback"))
                                        .on_disabled_hover_text(if registration_pending { "Wait for durable registration confirmation" } else { "Enable AI features and command-context sharing so the authored goal can be sent" })
                                        .on_hover_text(
                                            "Open the provider CLI in a PTY without Ember-native events or approval cards; the provider TUI owns its prompts",
                                        )
                                        .clicked()
                                    {
                                        pending = Some(TaskSidebarAction::StartTerminal(row.id));
                                    }
                                } else if ui
                                    .add_enabled(!registration_pending && (!authored_task || native_ai_enabled), egui::Button::new(&start_label))
                                    .on_disabled_hover_text(if registration_pending { "Wait for durable registration confirmation" } else { "Enable AI features and command-context sharing so the authored goal can be sent" })
                                    .on_hover_text(format!(
                                        "Open {} in a PTY inside this isolated worktree. Native structured events are not available for this provider yet; the CLI TUI owns its prompts and approvals.",
                                        row.provider.display_name()
                                    ))
                                    .clicked()
                                {
                                    pending = Some(TaskSidebarAction::StartTerminal(row.id));
                                }
                            }
                            if row.status == TaskStatus::Created
                                && matches!(
                                    row.runtime_kind,
                                    TaskRuntimeKind::Terminal | TaskRuntimeKind::TerminalFallback
                                )
                                && ui
                                    .button(if row.runtime_kind == TaskRuntimeKind::TerminalFallback {
                                        "Retry terminal fallback"
                                    } else {
                                        "Retry Agent terminal"
                                    })
                                    .on_hover_text(if row.runtime_kind
                                        == TaskRuntimeKind::TerminalFallback
                                    {
                                        "Retry only the provider CLI compatibility path; native one-shot authority remains consumed"
                                    } else {
                                        "Retry the provider CLI after its previous terminal exited"
                                    })
                                    .clicked()
                            {
                                pending = Some(TaskSidebarAction::StartTerminal(row.id));
                            }
                            if row.native_terminal_fallback_available
                                && ui
                                    .button(if row.status == TaskStatus::ReadyForReview {
                                        "Continue recovery in terminal"
                                    } else {
                                        "Continue in terminal"
                                    })
                                    .on_hover_text(
                                        "Keep the isolated worktree and continue through the provider CLI compatibility path; this permanently ends native authority for the task",
                                    )
                                    .clicked()
                            {
                                pending = Some(TaskSidebarAction::StartTerminal(row.id));
                            }
                            if row.status == TaskStatus::Failed
                                && matches!(
                                    row.runtime_kind,
                                    TaskRuntimeKind::Terminal | TaskRuntimeKind::TerminalFallback
                                )
                                && row.terminal_retry_available
                                && ui
                                    .button(if row.runtime_kind == TaskRuntimeKind::TerminalFallback {
                                        "Retry terminal fallback"
                                    } else {
                                        "Retry Agent terminal"
                                    })
                                    .on_hover_text(if row.runtime_kind
                                        == TaskRuntimeKind::TerminalFallback
                                    {
                                        "Start another provider CLI compatibility PTY; native one-shot authority remains consumed"
                                    } else {
                                        "Start another provider CLI PTY in the same isolated worktree"
                                    })
                                    .clicked()
                            {
                                pending = Some(TaskSidebarAction::StartTerminal(row.id));
                            }
                            if (row.native_preparing || row.has_active_agent_stream)
                                && !native_idle
                                && ui
                                    .button(if row.native_preparing {
                                        "Cancel preparation".to_string()
                                    } else {
                                        format!("Stop {}", row.provider.display_name())
                                    })
                                    .on_hover_text(if row.native_preparing {
                                        "Discard this background preparation; no provider process has started"
                                    } else {
                                        "Interrupt the turn, stop its process group, and wait for reap"
                                    })
                                    .clicked()
                                {
                                    pending = Some(TaskSidebarAction::StopCodex(row.id));
                                }
                            if ui
                                .add_enabled(
                                    row.has_agent_terminal,
                                    egui::Button::new("Agent terminal"),
                                )
                                .on_disabled_hover_text("Agent terminal is no longer available")
                                .clicked()
                            {
                                pending = Some(TaskSidebarAction::FocusTerminal(row.id));
                            }
                            if !manual_review && ui
                                    .add_enabled(
                                        row.status == TaskStatus::ReadyForReview
                                            && row.validation_status != TaskValidationStatus::Running
                                            && !row.has_active_agent_stream
                                            && has_validation && !registration_pending,
                                        egui::Button::new(
                                            if row.validation_attempt == 0 {
                                                "Run validation"
                                            } else {
                                                "Run again"
                                            },
                                        ),
                                    )
                                    .on_disabled_hover_text(if !has_validation {
                                        "No validation command is available in this recovered task".to_string()
                                    } else if registration_pending {
                                        "Wait for durable registration confirmation".to_string()
                                    } else if row.has_active_agent_stream {
                                        if native_idle {
                                            format!(
                                                "Finish {} to end the native session and unlock validation",
                                                row.provider.display_name()
                                            )
                                        } else {
                                            "Wait for the native Agent turn to reach review, then finish the session"
                                                .to_string()
                                        }
                                    } else if row.validation_status == TaskValidationStatus::Running {
                                        "Validation is already running".to_string()
                                    } else {
                                        "The Agent must reach Ready for review before validation".to_string()
                                    })
                                    .clicked()
                            {
                                pending = Some(TaskSidebarAction::RunValidation(row.id));
                            }
                            if ui
                                .add_enabled(
                                    row.has_validation_terminal,
                                    egui::Button::new("Validation output"),
                                )
                                .on_disabled_hover_text("No validation output is available")
                                .clicked()
                            {
                                pending = Some(TaskSidebarAction::FocusValidation(row.id));
                            }
                            if ui.button("Review diff").clicked() {
                                pending = Some(TaskSidebarAction::ReviewDiff(row.id));
                            }
                            let can_accept = row.status == TaskStatus::ReadyForReview
                                && !row.has_active_agent_stream && !registration_pending
                                && if manual_review { row.validation_status == TaskValidationStatus::NotRun }
                                   else { row.validation_status == TaskValidationStatus::Passed };
                            if task_accept_button(ui, row, manual_review, can_accept)
                                .on_disabled_hover_text(if row.has_active_agent_stream {
                                    "Finish the Agent session before accepting its work"
                                } else if manual_review {
                                    "Accept reviewed work becomes available when the Agent is ready for review"
                                } else {
                                    "Completion requires Ready for review and a passing validation result"
                                })
                                .on_hover_text(if manual_review { "Accept the diff you reviewed without an automated validation claim" } else { "Accept the reviewed task after its latest validation passed" })
                                .clicked() {
                                pending = Some(if manual_review { TaskSidebarAction::AcceptReviewed(row.id) } else { TaskSidebarAction::Complete(row.id) });
                            }
                            // The task manager's own gate decides; the reason
                            // names what must finish first.
                            if ui
                                .add_enabled(
                                    hide_blocked.is_none(),
                                    egui::Button::new("Hide task"),
                                )
                                .on_hover_text("Hide this task from the dashboard; its worktree stays in place and Hidden can restore it")
                                .on_disabled_hover_text(hide_blocked.unwrap_or_default())
                                .clicked()
                            {
                                pending = Some(TaskSidebarAction::Archive(row.id));
                            }
                        });
                        });
                        if let Some(task) = task {
                            render_task_inspector(ui, row.id, &TaskInspector::from_task(task));
                        }
                        if let Some(detail) = &row.status_detail {
                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                ui.label(egui::RichText::new(detail).small().weak());
                            });
                        }
                        if let Some(view) = native_view.as_ref() {
                            render_native_codex_view(
                                ui,
                                row.id,
                                row.provider,
                                view,
                                self.agent_runtime.has_running(row.id)
                                    && row.has_active_agent_stream,
                                &mut pending,
                                &mut approval_navigation,
                            );
                        }
                        if native_idle {
                            let provider_name = row.provider.display_name();
                            ui.group(|ui| {
                                if self.agent_runtime.can_prompt_native(row.id) {
                                    ui.label(
                                        egui::RichText::new("Review feedback")
                                            .small()
                                            .strong(),
                                    );
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "Send another turn on this loaded {provider_name} thread, or finish the session to unlock validation.",
                                        ))
                                        .small()
                                        .weak(),
                                    );
                                    let draft = self
                                        .task_sidebar
                                        .follow_up_drafts
                                        .entry(row.id)
                                        .or_default();
                                    let follow_up = ui.add_enabled(
                                        native_ai_enabled,
                                        egui::TextEdit::multiline(draft)
                                            .desired_rows(3)
                                            .char_limit(MAX_NATIVE_FOLLOW_UP_CHARS)
                                            .hint_text(format!(
                                                "Describe what {provider_name} should change next…"
                                            )),
                                    )
                                    .on_disabled_hover_text(
                                        "Enable AI features and command-context sharing before sending another cloud turn",
                                    );
                                    let send_shortcut = crate::ai_chat_panel::composer_submit_gesture(ui, &follow_up);
                                    ui.add_space(ui.text_style_height(&egui::TextStyle::Body));
                                    let can_send = native_ai_enabled
                                        && native_follow_up_can_send(
                                            draft.as_str(),
                                            native_view
                                                .as_ref()
                                                .map_or(0, |view| view.completed_turns),
                                        );
                                    let can_finish = draft
                                        .trim_matches(|character| {
                                            matches!(character, ' ' | '\n' | '\t')
                                        })
                                        .is_empty();
                                    ui.horizontal(|ui| {
                                        if (ui
                                            .add_enabled(
                                                can_send,
                                                egui::Button::new("Send follow-up"),
                                            )
                                            .on_disabled_hover_text(format!(
                                                "Feedback must be non-empty, at most {NATIVE_AGENT_FOLLOW_UP_MAX_BYTES} UTF-8 bytes, and sent before the {CODEX_APP_SERVER_LIVE_TURN_MAX}-turn session limit",
                                            ))
                                            .clicked() || send_shortcut) && can_send
                                        {
                                            pending = Some(TaskSidebarAction::FollowUp(
                                                row.id,
                                                draft.clone(),
                                            ));
                                        }
                                        if ui
                                            .add_enabled(
                                                can_finish,
                                                egui::Button::new(format!(
                                                    "Finish {provider_name}"
                                                )),
                                            )
                                            .on_hover_text(
                                                "End this idle native session; validation unlocks only after containment is empty and the provider is reaped",
                                            )
                                            .on_disabled_hover_text(format!(
                                                "Send or clear the draft before finishing {provider_name}"
                                            ))
                                            .clicked()
                                        {
                                            pending =
                                                Some(TaskSidebarAction::FinishCodex(row.id));
                                        }
                                        if !can_finish && ui.small_button("Clear").clicked() {
                                            draft.clear();
                                        }
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} / {} bytes · turn {} / {}",
                                                draft.len(),
                                                NATIVE_AGENT_FOLLOW_UP_MAX_BYTES,
                                                native_view
                                                    .as_ref()
                                                    .map_or(0, |view| view.completed_turns),
                                                CODEX_APP_SERVER_LIVE_TURN_MAX,
                                            ))
                                            .small()
                                            .weak(),
                                        );
                                    });
                                } else {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{provider_name} session"
                                        ))
                                        .small()
                                        .strong(),
                                    );
                                    ui.label(
                                        egui::RichText::new(
                                            "This native MVP is one-shot. Finish the session to unlock validation, or continue in Terminal fallback for interactive follow-up.",
                                        )
                                        .small()
                                        .weak(),
                                    );
                                    if ui
                                        .button(format!("Finish {provider_name}"))
                                        .on_hover_text(
                                            "End this idle native session; validation unlocks after the provider is reaped",
                                        )
                                        .clicked()
                                    {
                                        pending = Some(TaskSidebarAction::FinishCodex(row.id));
                                    }
                                }
                            });
                        }
                        if row.validation_attempt > 0 {
                            let validation_color =
                                task_validation_color(ui, row.validation_status);
                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Validation #{} · {}",
                                        row.validation_attempt,
                                        row.validation_status.label()
                                    ))
                                    .small()
                                    .color(validation_color),
                                );
                                if row.validation_status == TaskValidationStatus::Running {
                                    ui.spinner();
                                }
                            });
                            if let Some(detail) = &row.validation_detail {
                                ui.horizontal(|ui| {
                                    ui.add_space(18.0);
                                    ui.label(egui::RichText::new(detail).small().weak());
                                });
                            }
                        }
                        });
                    }
                    ui.separator();
                }
            });
        if pending.is_some() {
            self.task_sidebar.pending_action = pending;
        }
    }

    pub(crate) fn execute_pending_task_sidebar_action(&mut self, ctx: &egui::Context) {
        let Some(action) = self.task_sidebar.pending_action.take() else {
            return;
        };
        if task_action_waits_for_recovery(&action, &self.task_sidebar.unconfirmed_registrations) {
            self.task_sidebar.recovery_error = Some("Task registration is not durably confirmed. Retry registration or Reload recovery records before Start.".into());
            return;
        }
        if native_action_needs_prompt_poll(&action) {
            // The runtime may legitimately poll slowly while it waits for
            // this very decision; show its response without that delay.
            self.task_sidebar.native_fast_poll_until =
                Some(Instant::now() + NATIVE_ACTION_FAST_POLL);
            ctx.request_repaint();
        }
        match action {
            TaskSidebarAction::StartNative(task_id) => self.start_task_native_agent(task_id),
            TaskSidebarAction::LoadHistory(task_id, reset) => {
                if self.task_sidebar.selected == Some(task_id) {
                    if let Err(error) = self.agent_runtime.load_native_history(task_id, reset) {
                        self.set_status_for(error.to_string(), Duration::from_secs(6));
                    }
                }
            }
            TaskSidebarAction::StartTerminal(task_id) => self.start_task_agent_terminal(task_id),
            TaskSidebarAction::StopCodex(task_id) => match self.agent_runtime.cancel(task_id) {
                Ok(()) => {
                    if self.agent_runtime.has_running(task_id) {
                        self.set_status("Stopping Codex and waiting for process cleanup…");
                    } else {
                        self.set_status(
                            "Native Codex preparation cancelled; finishing background cleanup…",
                        );
                    }
                }
                Err(error) => {
                    self.set_status_for(error.to_string(), Duration::from_secs(6));
                }
            },
            TaskSidebarAction::FollowUp(task_id, text) => {
                let policy = NativePromptPolicy {
                    share_command_context: self.config.ai_enabled
                        && self.config.ai_share_command_context,
                    redact_secrets: self.config.ai_redact_secrets,
                };
                match self
                    .agent_runtime
                    .prompt_native(&self.task_manager, task_id, &text, policy)
                {
                    Ok(()) => {
                        clear_sent_follow_up(
                            &mut self.task_sidebar.follow_up_drafts,
                            task_id,
                            &text,
                        );
                        self.set_status("Follow-up queued on the existing agent conversation…");
                    }
                    Err(error) => {
                        self.set_status_for(error.to_string(), Duration::from_secs(6));
                    }
                }
            }
            TaskSidebarAction::FinishCodex(task_id) => {
                match self.agent_runtime.finish_codex(&self.task_manager, task_id) {
                    Ok(()) => {
                        self.task_sidebar.follow_up_drafts.remove(&task_id);
                        self.set_status(
                            "Finishing Codex and waiting for containment cleanup before validation…",
                        );
                    }
                    Err(error) => {
                        self.set_status_for(error.to_string(), Duration::from_secs(6));
                    }
                }
            }
            TaskSidebarAction::Approve(task_id, approval_id) => {
                self.decide_native_approval(task_id, approval_id, ApprovalDecision::Approve)
            }
            TaskSidebarAction::Deny(task_id, approval_id) => self.decide_native_approval(
                task_id,
                approval_id,
                ApprovalDecision::Deny { reason: None },
            ),
            TaskSidebarAction::FocusTerminal(task_id) => {
                let session_id = self
                    .task_manager
                    .get(task_id)
                    .and_then(|task| task.terminal_session_id.clone());
                let Some(session_id) = session_id else {
                    self.set_status("Agent terminal is no longer available");
                    return;
                };
                let Some(index) = self.session_manager.index_of(&session_id) else {
                    self.set_status("Agent terminal is no longer available");
                    return;
                };
                if !self.activate_session(index) {
                    self.set_status("Agent terminal is no longer available");
                }
            }
            TaskSidebarAction::FocusValidation(task_id) => {
                let session_id = self
                    .task_manager
                    .get(task_id)
                    .and_then(|task| task.validation.terminal_session_id.clone());
                let Some(session_id) = session_id else {
                    self.set_status("Validation output is no longer available");
                    return;
                };
                let Some(index) = self.session_manager.index_of(&session_id) else {
                    self.set_status("Validation output is no longer available");
                    return;
                };
                if !self.activate_session(index) {
                    self.set_status("Validation output is no longer available");
                }
            }
            TaskSidebarAction::RunValidation(task_id) => self.start_task_validation(task_id),
            TaskSidebarAction::Complete(task_id) => {
                match self.task_manager.complete_after_validation(task_id) {
                    Ok(()) => self.set_status("Task marked complete after passing validation"),
                    Err(error) => self.set_status_for(error.to_string(), Duration::from_secs(5)),
                }
            }
            TaskSidebarAction::AcceptReviewed(task_id) => {
                match self.task_manager.complete_after_manual_review(task_id) {
                    Ok(()) => self.set_status(
                        "Task accepted after manual review; no automated validation was claimed",
                    ),
                    Err(error) => self.set_status_for(error.to_string(), Duration::from_secs(5)),
                }
            }
            TaskSidebarAction::ReviewDiff(task_id) => {
                let task_review = self
                    .task_manager
                    .get(task_id)
                    .map(|task| (task.worktree_path.clone(), task.base_commit.clone()));
                let Some((worktree, base_commit)) = task_review else {
                    self.set_status("Task is no longer available");
                    return;
                };
                if let Err(error) = self.agent_diff.request_from(worktree, base_commit) {
                    self.set_status_for(
                        format!("Could not open task diff: {error}"),
                        Duration::from_secs(5),
                    );
                }
            }
            TaskSidebarAction::Archive(task_id) => match self.task_manager.archive(task_id) {
                Ok(()) => {
                    // Hiding is reversible: the retained native view (transcript,
                    // approvals, exit evidence) and any follow-up draft stay, so
                    // Restore shows the task exactly as it was.
                    carry_unconfirmed_registration(
                        &mut self.task_sidebar.unconfirmed_registrations,
                        &mut self.task_sidebar.hidden_unconfirmed_registrations,
                        task_id,
                    );
                    if self.task_sidebar.selected == Some(task_id) {
                        self.task_sidebar.selected = None;
                    }
                    self.set_status("Task hidden; worktree left in place. Restore it from Hidden.");
                }
                Err(error) => self.set_status_for(error.to_string(), Duration::from_secs(5)),
            },
            TaskSidebarAction::Restore(task_id) => {
                match restore_hidden_task(&mut self.task_manager, task_id) {
                    Ok(message) => {
                        let still_unconfirmed = carry_unconfirmed_registration(
                            &mut self.task_sidebar.hidden_unconfirmed_registrations,
                            &mut self.task_sidebar.unconfirmed_registrations,
                            task_id,
                        );
                        let confirmable = self.task_manager.get(task_id).is_some_and(|task| {
                            registration_confirmable(task, &self.task_sidebar.recovery_records)
                        });
                        // Selected and shown on the dashboard, where it now is.
                        self.task_sidebar.selected = Some(task_id);
                        self.task_sidebar.scroll_to_selected = true;
                        self.task_sidebar.show_dashboard();
                        self.set_status(restore_status(message, still_unconfirmed, confirmable));
                    }
                    Err(error) => self.set_status_for(error, Duration::from_secs(6)),
                }
            }
        }
    }

    /// The Hidden view: archived tasks with Restore and read-only diff review.
    /// No other task action is offered for a hidden task.
    fn render_hidden_tasks(&mut self, ui: &mut egui::Ui) {
        let rows = hidden_task_rows(&self.task_manager);
        ui.label(
            egui::RichText::new(format!(
                "{} hidden · Restore returns a task to its state when hidden; it never starts an Agent",
                rows.len()
            ))
            .small()
            .color(ui.visuals().weak_text_color()),
        );
        if rows.is_empty() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("No hidden tasks").strong());
            ui.label(
                egui::RichText::new(
                    "Hide task removes a finished task from the dashboard and leaves its worktree in place. Choose Hidden again to return.",
                )
                .small()
                .color(ui.visuals().weak_text_color()),
            );
            return;
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let mut pending = None;
        egui::ScrollArea::vertical()
            .id_salt("hidden_agent_tasks")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for row in &rows {
                    ui.push_id(("hidden_task", row.id), |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&row.title).strong()).truncate(),
                        )
                        .on_hover_text(&row.title);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(format!(
                                    "{} · hidden while {}",
                                    row.provider.display_name(),
                                    row.restores_to
                                        .map_or("in an unrecorded state", TaskStatus::label)
                                ))
                                .small(),
                            )
                            .truncate(),
                        );
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("{} · {}", row.project, row.branch))
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            )
                            .truncate(),
                        );
                        ui.label(
                            egui::RichText::new(task_update_age(row.updated_at_ms, now_ms))
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                        ui.horizontal_wrapped(|ui| {
                            if hidden_task_restore_button(ui, row)
                                .on_hover_text(format!(
                                    "Return this task to {} on the dashboard. Its worktree, validation record and Agent transcript stay as they were; nothing starts.",
                                    row.restores_to.map_or("its previous state", TaskStatus::label)
                                ))
                                .on_disabled_hover_text(
                                    "This task was hidden without a recorded state to return to. Create a new task for its worktree.",
                                )
                                .clicked()
                            {
                                pending = Some(TaskSidebarAction::Restore(row.id));
                            }
                            if ui
                                .button("Review diff")
                                .on_hover_text("Read-only view of the worktree's changes")
                                .clicked()
                            {
                                pending = Some(TaskSidebarAction::ReviewDiff(row.id));
                            }
                        });
                    });
                    ui.separator();
                }
            });
        if pending.is_some() {
            self.task_sidebar.pending_action = pending;
        }
    }

    /// Reveal the Tasks dashboard and remember it as the sidebar view.
    pub(crate) fn open_tasks_view(&mut self) {
        self.ai_chat_panel.close();
        self.agent_sessions.workspace_task = None;
        self.task_sidebar.show_dashboard();
        self.sidebar.note_files_user_intent();
        self.sidebar.visible = true;
        self.sidebar.view = crate::sidebar::SidebarView::Tasks;
        if self.config.sidebar_view != crate::sidebar::SidebarView::Tasks {
            self.config.sidebar_view = crate::sidebar::SidebarView::Tasks;
            self.schedule_config_save();
        }
    }

    /// `task:toggle` (Ctrl+Alt+Shift+T): open the dashboard, or close it back
    /// to the sidebar state it replaced. Explains how to enable it when off.
    pub(crate) fn toggle_tasks_view(&mut self) {
        if !self.config.experimental_task_sidebar {
            self.set_status_for(
                "The Tasks dashboard is off. Enable Settings → AI → “Show experimental Tasks dashboard” (experimental_task_sidebar = true) to use it.",
                Duration::from_secs(6),
            );
            return;
        }
        let ((visible, view), previous) = tasks_toggle_target(
            (self.sidebar.visible, self.sidebar.view),
            self.task_sidebar.toggle_return.take(),
        );
        self.task_sidebar.toggle_return = previous;
        if visible && view == crate::sidebar::SidebarView::Tasks {
            self.task_sidebar.show_dashboard();
        }
        self.sidebar.note_files_user_intent();
        self.sidebar.visible = visible;
        if self.sidebar.view != view {
            self.sidebar.view = view;
            self.config.sidebar_view = view;
            self.schedule_config_save();
        }
        if visible && view == crate::sidebar::SidebarView::Files {
            if let Some(error) = self.sidebar.refresh() {
                self.set_status(format!("Files refresh failed: {error}"));
            }
        }
    }

    /// The compact attention indicator for chrome that stays visible while
    /// the sidebar is hidden.
    pub(crate) fn chrome_task_attention(&self) -> Option<(String, String)> {
        task_attention_indicator(
            self.config.experimental_task_sidebar,
            self.sidebar.visible,
            self.task_manager.attention_counts(),
        )
    }

    fn start_task_native_agent(&mut self, task_id: TaskId) {
        let provider = self.task_manager.get(task_id).map(|task| task.provider);
        let Some(provider) = provider else {
            self.set_status("Task is no longer available");
            return;
        };
        let policy = NativePromptPolicy {
            share_command_context: self.config.ai_enabled && self.config.ai_share_command_context,
            redact_secrets: self.config.ai_redact_secrets,
        };
        let result = match provider {
            AgentProvider::Codex => {
                self.agent_runtime
                    .start_codex(&mut self.task_manager, task_id, policy)
            }
            AgentProvider::Claude => {
                self.agent_runtime
                    .start_claude(&mut self.task_manager, task_id, policy)
            }
            AgentProvider::Kimi => {
                self.agent_runtime
                    .start_kimi(&mut self.task_manager, task_id, policy)
            }
            AgentProvider::OpenCode => {
                self.set_status_for(
                    format!(
                        "{} has no native driver yet; use Start {}",
                        provider.display_name(),
                        provider.display_name()
                    ),
                    Duration::from_secs(6),
                );
                return;
            }
        };
        match result {
            Ok(()) => self.set_status(format!(
                "Preparing native {} prerequisites in the background…",
                provider.display_name()
            )),
            Err(error) => self.set_status_for(
                format!(
                    "Could not start native {}: {error}",
                    provider.display_name()
                ),
                Duration::from_secs(8),
            ),
        }
    }

    fn decide_native_approval(
        &mut self,
        task_id: TaskId,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    ) {
        let label = if matches!(&decision, ApprovalDecision::Approve) {
            "Approval sent to Codex"
        } else {
            "Denial sent to Codex"
        };
        match self
            .agent_runtime
            .decide_approval(task_id, approval_id, decision)
        {
            Ok(()) => self.set_status(label),
            Err(error) => self.set_status_for(error.to_string(), Duration::from_secs(6)),
        }
    }

    /// Drain only already-buffered native events. The runtime applies its own
    /// global/per-task frame budgets and never waits for provider I/O here.
    pub(crate) fn poll_native_agent_runtime(&mut self, ctx: &egui::Context) {
        let current_policy = NativePromptPolicy {
            share_command_context: self.config.ai_enabled && self.config.ai_share_command_context,
            redact_secrets: self.config.ai_redact_secrets,
        };
        let report = self
            .agent_runtime
            .poll(&mut self.task_manager, current_policy);
        if let Some(issue) = report.issues.last() {
            self.set_status_for(
                format!("Native Agent issue: {}", issue.detail),
                Duration::from_secs(7),
            );
        } else if let Some(message) = native_completion_feedback(
            &self.task_manager,
            report.completions.iter().map(|completion| {
                (
                    completion.task_id,
                    completion.outcome,
                    completion.detail.as_deref(),
                )
            }),
        ) {
            self.set_status_for(message, Duration::from_secs(8));
        } else if report.preparations_started > 0 {
            self.set_status(if report.preparations_started == 1 {
                "Native Agent prerequisites verified; starting provider…".to_string()
            } else {
                format!(
                    "{} native Agent sessions finished preparation and are starting…",
                    report.preparations_started
                )
            });
        }
        // Approval waits and idle review points poll slowly by design; the
        // user's own action keeps frame-rate polling briefly instead.
        let user_action_window = self
            .task_sidebar
            .native_fast_poll_until
            .is_some_and(|until| Instant::now() < until);
        if !user_action_window {
            self.task_sidebar.native_fast_poll_until = None;
        }
        if self.agent_runtime.needs_fast_poll() || report.budget_exhausted || user_action_window {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.agent_runtime.has_any_activity() {
            ctx.request_repaint_after(Duration::from_millis(250));
        } else if report.made_progress() {
            ctx.request_repaint();
        }
    }

    fn start_task_agent_terminal(&mut self, task_id: TaskId) {
        if self.agent_runtime.has_preparing(task_id) {
            self.set_status("Cancel native Codex preparation before starting a terminal");
            return;
        }
        let failed_terminal_retry = self
            .task_manager
            .terminal_retry_session_id(task_id)
            .ok()
            .map(str::to_owned);
        let native_recovery = failed_terminal_retry.is_none()
            && self.agent_runtime.can_continue_in_terminal(task_id)
            && self
                .task_manager
                .native_terminal_fallback_eligible(task_id)
                .is_ok();
        let launch_bits = self.task_manager.get(task_id).and_then(|task| {
            ((task.status == TaskStatus::Created && task.terminal_session_id.is_none())
                || (native_recovery && task.terminal_session_id.is_none())
                || failed_terminal_retry
                    .as_deref()
                    .is_some_and(|old| task.terminal_session_id.as_deref() == Some(old)))
            .then(|| {
                (
                    task.provider,
                    task.title.clone(),
                    task.repo_root.clone(),
                    task.worktree_path.clone(),
                    task.clone(),
                )
            })
        });
        let Some((provider, title, repository, worktree, task_snapshot)) = launch_bits else {
            self.set_status("Task is no longer waiting for an Agent terminal");
            return;
        };
        let policy = NativePromptPolicy {
            share_command_context: self.config.ai_enabled && self.config.ai_share_command_context,
            redact_secrets: self.config.ai_redact_secrets,
        };
        if task_snapshot.brief.is_some() && !policy.share_command_context {
            self.set_status_for("Enable AI features and command-context sharing in Settings → AI before starting this authored task. Its goal has not been sent.", Duration::from_secs(8));
            return;
        }
        let brief_written = match crate::agent::write_pty_task_brief(&task_snapshot, policy) {
            Ok(path) => {
                if task_snapshot.brief.is_some() && path.is_none() {
                    self.set_status_for("The task goal could not be prepared. No Agent was started; check AI sharing settings and retry.", Duration::from_secs(8));
                    return;
                }
                path.is_some()
            }
            Err(error) => {
                if task_snapshot.brief.is_some() {
                    self.set_status_for(
                        format!("Could not prepare the task goal; no Agent was started: {error}"),
                        Duration::from_secs(8),
                    );
                    return;
                }
                // Missing/disabled context is expected; IO failures should surface.
                if !matches!(
                    error,
                    crate::agent::NativePromptError::SharingDisabled
                        | crate::agent::NativePromptError::MissingContext
                        | crate::agent::NativePromptError::MissingCommand
                        | crate::agent::NativePromptError::CommandNotExact
                        | crate::agent::NativePromptError::CommandTruncated
                        | crate::agent::NativePromptError::OutputUnavailable
                ) {
                    self.set_status_for(
                        format!("Could not write task brief: {error}"),
                        Duration::from_secs(6),
                    );
                }
                false
            }
        };
        let launch = match crate::agent::AgentLaunchSpec::resolve(provider, &repository, &worktree)
        {
            Ok(launch) => {
                if brief_written {
                    let instruction = if task_snapshot.brief.is_some() {
                        "carry out the user-authored goal described there"
                    } else {
                        "fix the failed command described there"
                    };
                    launch.with_pty_seed_prompt(&format!("Read {PTY_TASK_BRIEF_RELATIVE} and {instruction}. Stay inside this worktree."))
                } else {
                    launch
                }
            }
            Err(error) => {
                if failed_terminal_retry.is_none() && !native_recovery {
                    // update_status preserves TerminalFallback provenance, so
                    // a failed compatibility launch remains terminal-only.
                    let _ = self.task_manager.update_status(
                        task_id,
                        TaskStatus::Created,
                        Some(error.to_string()),
                    );
                }
                self.set_status_for(error.to_string(), Duration::from_secs(6));
                return;
            }
        };
        if failed_terminal_retry.is_none() && !native_recovery {
            let _ = self
                .task_manager
                .update_status(task_id, TaskStatus::Starting, None);
        }

        let (cols, rows) = crate::terminal::clamp_terminal_dimensions(self.cols, self.rows);
        let session_name = format!(
            "{} · {}",
            provider.display_name(),
            visible_bounded(&title, 96)
        );
        let created = match self.session_manager.new_command_session_in_cwd(
            session_name,
            launch.argv,
            &worktree,
            cols,
            rows,
            self.config.scrollback_lines,
        ) {
            Ok(created) => created,
            Err(error) => {
                if failed_terminal_retry.is_none() && !native_recovery {
                    let _ = self.task_manager.update_status(
                        task_id,
                        TaskStatus::Created,
                        Some(error.clone()),
                    );
                }
                self.set_status_for(
                    format!("Could not start {}: {error}", provider.display_name()),
                    Duration::from_secs(6),
                );
                return;
            }
        };

        let binding = if let Some(old_session) = failed_terminal_retry.as_deref() {
            self.task_manager.bind_terminal_retry_session(
                task_id,
                old_session,
                created.session_id.clone(),
            )
        } else if native_recovery {
            self.task_manager
                .bind_native_terminal_fallback_session(task_id, created.session_id.clone())
        } else {
            self.task_manager
                .bind_terminal_session(task_id, created.session_id.clone())
        };
        if let Err(error) = binding {
            // The session was inserted but has not entered TabManager yet;
            // removing it immediately restores the original index layout.
            let _ = self.session_manager.close_session(created.session_index);
            self.block_bookmarks.remove_session(&created.session_id);
            if failed_terminal_retry.is_none() && !native_recovery {
                let _ = self.task_manager.update_status(
                    task_id,
                    // The PTY was closed before it gained task authority. Keep
                    // the selected runtime family retryable; in particular a
                    // consumed native one-shot remains TerminalFallback and can
                    // never expose Start Codex again.
                    TaskStatus::Created,
                    Some(error.to_string()),
                );
            }
            self.set_status_for(error.to_string(), Duration::from_secs(6));
            return;
        }

        if native_recovery {
            self.agent_runtime.clear_retained(task_id);
        }

        self.tabs.on_session_inserted(created.session_index);
        self.tabs.insert_tab_after_active(created.session_index);
        self.activate_session(created.session_index);
        self.schedule_session_save();
        self.set_status(if brief_written {
            format!(
                "Opened {} in an isolated task terminal; failed-command brief is at {PTY_TASK_BRIEF_RELATIVE}",
                provider.display_name()
            )
        } else {
            format!(
                "Opened {} in an isolated task terminal; enable AI context sharing to attach a brief",
                provider.display_name()
            )
        });
    }

    /// Apply the authoritative exit before composing passive, task-keyed feedback.
    pub(crate) fn handle_task_terminal_exit(
        &mut self,
        session_id: &str,
        exit_code: Option<i32>,
    ) -> bool {
        let (handled, message) =
            apply_task_terminal_exit(&mut self.task_manager, session_id, exit_code);
        if let Some(message) = message {
            self.set_status_for(message, Duration::from_secs(8));
        }
        handled
    }

    fn start_task_validation(&mut self, task_id: TaskId) {
        let next_attempt = match self.task_manager.next_validation_attempt(task_id) {
            Ok(attempt) => attempt,
            Err(error) => {
                self.set_status_for(error.to_string(), Duration::from_secs(6));
                return;
            }
        };
        let prepared = {
            let Some(task) = self.task_manager.get(task_id) else {
                self.set_status("Task is no longer available");
                return;
            };
            match crate::agent::prepare_task_validation(task) {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.set_status_for(
                        format!("Could not prepare validation: {error}"),
                        Duration::from_secs(7),
                    );
                    return;
                }
            }
        };
        let argv = match self
            .session_manager
            .validation_command_argv(&prepared.source_shell, &prepared.command)
        {
            Ok(argv) => argv,
            Err(error) => {
                self.set_status_for(
                    format!("Could not resolve validation shell: {error}"),
                    Duration::from_secs(6),
                );
                return;
            }
        };
        let task_title = match self.task_manager.get(task_id) {
            Some(task) => task.title.clone(),
            None => {
                self.set_status("Task is no longer available");
                return;
            }
        };
        let (cols, rows) = crate::terminal::clamp_terminal_dimensions(self.cols, self.rows);
        let session_name = format!(
            "Validate #{} · {}",
            next_attempt,
            visible_bounded(&task_title, 88)
        );
        let created = match self.session_manager.new_validation_session_in_cwd(
            session_name,
            argv,
            &prepared.cwd,
            prepared.pinned_cwd,
            cols,
            rows,
            self.config.scrollback_lines,
        ) {
            Ok(created) => created,
            Err(error) => {
                self.set_status_for(
                    format!("Could not start validation: {error}"),
                    Duration::from_secs(6),
                );
                return;
            }
        };

        if let Err(error) = self
            .task_manager
            .bind_validation_session(task_id, created.session_id.clone())
        {
            let _ = self.session_manager.close_session(created.session_index);
            self.block_bookmarks.remove_session(&created.session_id);
            self.set_status_for(error.to_string(), Duration::from_secs(6));
            return;
        }

        self.tabs.on_session_inserted(created.session_index);
        self.tabs.insert_tab_after_active(created.session_index);
        self.activate_session(created.session_index);
        self.schedule_session_save();
        self.set_status(format!(
            "Validation #{} is running in the isolated task worktree",
            next_attempt
        ));
    }
}

fn task_status_color(ui: &egui::Ui, status: TaskStatus) -> egui::Color32 {
    match status {
        TaskStatus::WaitingForApproval | TaskStatus::WaitingForHuman => ui.visuals().warn_fg_color,
        TaskStatus::Failed => ui.visuals().error_fg_color,
        TaskStatus::ReadyForReview | TaskStatus::Completed => egui::Color32::from_rgb(90, 190, 120),
        TaskStatus::Starting | TaskStatus::Working => egui::Color32::from_rgb(90, 150, 230),
        TaskStatus::Created | TaskStatus::Cancelled | TaskStatus::Archived => {
            ui.visuals().weak_text_color()
        }
    }
}

/// Counts describe current search matches before the selected quick view.
fn dashboard_filter_label(
    view: jterm_core::agent_task::DashboardView,
    counts: &jterm_core::agent_task::DashboardCounts,
) -> String {
    use jterm_core::agent_task::DashboardView;
    let (label, count) = match view {
        DashboardView::All => ("All", counts.total),
        DashboardView::NeedsAttention => ("Needs you", counts.attention),
        DashboardView::Running => ("Running", counts.running),
        DashboardView::NotStarted => ("Not started", counts.created),
        DashboardView::Review => ("Review", counts.review),
        DashboardView::Finished => ("Finished", counts.done + counts.failed + counts.cancelled),
    };
    format!("{label} {count}")
}

/// Use TextEdit's own Enter/focus handling; this is not a terminal shortcut.
fn dashboard_search(
    ui: &mut egui::Ui,
    query: &mut String,
    query_error: &mut DashboardQueryErrorCache,
) -> bool {
    let previous = query.clone();
    let rejected_input = egui::Id::new("agent-task-query-rejected-input");
    let search = ui.add(
        egui::TextEdit::singleline(query)
            .id(egui::Id::new("agent-task-search"))
            .hint_text("Search tasks · provider:codex attention:review"),
    ).on_hover_text("Enter selects the first matching task. Search never starts an Agent or confirms an operation.");
    // Truncating a pasted query can drop its final exclusion and silently
    // broaden the result set. Preserve input through the core's byte budget;
    // reject an excessive editor update atomically instead of truncating it.
    let input_rejected = query.len() > 4096;
    if input_rejected {
        *query = previous;
        ui.data_mut(|data| data.insert_temp(rejected_input, query.clone()));
        ui.ctx().request_repaint();
    } else if search.changed() {
        ui.data_mut(|data| data.remove::<String>(rejected_input));
    }
    if ui
        .data(|data| data.get_temp::<String>(rejected_input))
        .as_deref()
        == Some(query.as_str())
    {
        ui.colored_label(
            ui.visuals().error_fg_color,
            "Search input exceeds 4 KiB. The previous query was kept in full.",
        );
    }
    if let Some(error) = query_error.error(query) {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    egui::CollapsingHeader::new("Search help")
        .id_salt("task_dashboard_query_help")
        .show(ui, |ui| {
            ui.label("Combine words, quoted phrases and field filters. Prefix a term with - to exclude it. Enumerated fields such as provider or status accept comma-separated alternatives; commas in text fields stay literal.");
            ui.label("Fields: title, provider, status, runtime, validation, project, branch, id, is, attention.");
            for example in [
                "provider:codex attention:approval,review",
                "project:ember -validation:passed",
                "status:failed,cancelled \"release plan\"",
                "is:running",
            ] {
                ui.monospace(example);
            }
            ui.label("Queries are limited to 256 UTF-8 bytes. An invalid query shows no tasks; your selected task and editor drafts are kept.");
        });
    // Single-line TextEdit surrenders focus on its native Enter handling.
    // Do not treat an IME commit in the same frame as an open gesture.
    !input_rejected
        && search.lost_focus()
        && ui.input(|input| {
            input.key_pressed(egui::Key::Enter)
                && input.modifiers.is_none()
                && !input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Ime(_)))
        })
}

fn task_validation_color(ui: &egui::Ui, status: TaskValidationStatus) -> egui::Color32 {
    match status {
        TaskValidationStatus::Running => egui::Color32::from_rgb(90, 150, 230),
        TaskValidationStatus::Passed => egui::Color32::from_rgb(90, 190, 120),
        TaskValidationStatus::Failed => ui.visuals().error_fg_color,
        TaskValidationStatus::Inconclusive | TaskValidationStatus::Cancelled => {
            ui.visuals().warn_fg_color
        }
        TaskValidationStatus::NotRun => ui.visuals().weak_text_color(),
    }
}

#[cfg(test)]
mod management_v3_tests {
    use super::*;

    fn fixture(
        status: TaskStatus,
        validation: TaskValidationStatus,
    ) -> (crate::agent::AgentTask, TaskRowSnapshot, DashboardRow) {
        let mut manager = crate::agent::TaskManager::new();
        let id = manager
            .create_with_brief(
                crate::agent::NewTask {
                    title: "Review the release changes".into(),
                    provider: AgentProvider::Codex,
                    repo_root: "/work/ember".into(),
                    worktree_path: "/work/tasks/release-review".into(),
                    branch: "task/release-review".into(),
                    base_commit: "a".repeat(40),
                    source_context: None,
                },
                TaskBrief::new("Improve task review and keep validation explicit", None).unwrap(),
            )
            .unwrap();
        let mut task = manager.get(id).unwrap().clone();
        task.status = status;
        task.validation.status = validation;
        task.validation.attempt = u64::from(validation != TaskValidationStatus::NotRun);
        task.updated_at_ms = 1_000_000;
        let shared = DashboardRow::from_task(&task, Default::default());
        let row = TaskRowSnapshot {
            id,
            title: task.title.clone(),
            provider: task.provider,
            status,
            runtime_kind: TaskRuntimeKind::Unassigned,
            branch: task.branch.clone(),
            project: task.repo_root.to_string_lossy().into_owned(),
            has_agent_terminal: false,
            has_validation_terminal: false,
            has_active_agent_stream: false,
            native_preparing: false,
            terminal_retry_available: false,
            native_terminal_fallback_available: false,
            validation_status: validation,
            validation_attempt: task.validation.attempt,
            validation_detail: None,
            needs_attention: task.needs_attention(),
            status_detail: None,
        };
        (task, row, shared)
    }

    #[test]
    fn attention_queue_preserves_hidden_selection_and_pre_filter_counts() {
        let (_, _, approval) =
            fixture(TaskStatus::WaitingForApproval, TaskValidationStatus::Failed);
        let (_, _, validation) = fixture(TaskStatus::ReadyForReview, TaskValidationStatus::Failed);
        let (_, _, review) = fixture(TaskStatus::ReadyForReview, TaskValidationStatus::NotRun);
        let selected = Some(review.id);
        let query = "provider:codex project:ember";
        let mut dashboard = DashboardSnapshot::filter_rows(
            &[approval, validation.clone(), review],
            query,
            DashboardView::All,
        );
        filter_attention_queue(&mut dashboard, Some(DashboardAttentionKind::Validation));
        assert_eq!(dashboard.counts.total, 3);
        assert_eq!(dashboard.rows.len(), 1);
        assert_eq!(dashboard.rows[0].id, validation.id);
        assert_eq!(dashboard.selected_index(selected), None);
        assert_eq!(dashboard.next_attention(selected), Some(validation.id));
        assert_eq!(dashboard.previous_attention(selected), Some(validation.id));
        let mut invalid = DashboardSnapshot::filter_rows(
            &dashboard.rows,
            "provider:not-a-provider",
            DashboardView::All,
        );
        filter_attention_queue(&mut invalid, Some(DashboardAttentionKind::Validation));
        assert!(invalid.rows.is_empty());
        assert_eq!(invalid.next_attention(selected), None);
        assert_eq!(invalid.previous_attention(selected), None);
    }

    #[test]
    fn validation_pass_does_not_show_task_completion_and_runtime_is_independent() {
        let (_, mut row, _) = fixture(TaskStatus::ReadyForReview, TaskValidationStatus::Passed);
        assert_ne!(task_row_marker(&row), "✓");
        assert_eq!(task_validation_label(&row), "Validation #1: Passed");
        row.has_active_agent_stream = true;
        assert_eq!(task_runtime_label(&row), Some("Native session open"));
        assert_eq!(row.status, TaskStatus::ReadyForReview);
        row.native_preparing = true;
        assert_eq!(task_runtime_label(&row), Some("Preparing Agent"));
        row.native_preparing = false;
        row.has_active_agent_stream = false;
        row.status = TaskStatus::Completed;
        row.needs_attention = false;
        assert_eq!(task_row_marker(&row), "✓");
    }

    #[test]
    fn registration_guidance_precedes_shared_review_without_changing_authority() {
        let (task, mut row, _) = fixture(TaskStatus::ReadyForReview, TaskValidationStatus::NotRun);
        row.has_active_agent_stream = true;
        let guidance = task_guidance(&task, &row, false, false);
        assert_eq!(guidance.title, "Review or continue the live session");
        let pending = task_guidance(&task, &row, true, false);
        assert!(pending.detail.contains("durable recovery marker"));
        // Once no record is left to confirm it, the card says so instead of
        // promising a confirmation that can never happen.
        let orphaned = task_guidance(&task, &row, true, true);
        assert_eq!(orphaned.title, "Registration cannot be confirmed");
        assert!(orphaned.detail.contains("recovery record was dismissed"));
        assert_eq!(
            task_guidance(&task, &row, false, true).title,
            guidance.title,
            "only a blocked registration can be orphaned"
        );
        assert_eq!(task.status, TaskStatus::ReadyForReview);
        assert!(row.has_active_agent_stream);
    }

    #[test]
    fn follow_up_receipt_preserves_newer_text_and_other_task_drafts() {
        let task = TaskId::new();
        let other = TaskId::new();
        let mut drafts = HashMap::from([
            (task, "Newer feedback".to_string()),
            (other, "Other task feedback".to_string()),
        ]);
        clear_sent_follow_up(&mut drafts, task, "Previously sent feedback");
        assert_eq!(drafts[&task], "Newer feedback");
        clear_sent_follow_up(&mut drafts, task, "Newer feedback");
        assert!(!drafts.contains_key(&task));
        assert_eq!(drafts[&other], "Other task feedback");
    }

    #[test]
    fn update_age_does_not_invent_recent_activity_from_missing_or_future_time() {
        assert_eq!(task_update_age(0, 100_000), "Update time unavailable");
        assert_eq!(
            task_update_age(110_000, 100_000),
            "Update time is ahead of this clock"
        );
        assert_eq!(task_update_age(100_000, 159_999), "Updated just now");
        assert_eq!(task_update_age(100_000, 160_000), "Updated 1m ago");
    }

    #[test]
    fn task_row_pointer_release_cannot_select_a_replacement_after_filtering() {
        let ctx = egui::Context::default();
        let (_, first, first_shared) =
            fixture(TaskStatus::ReadyForReview, TaskValidationStatus::Passed);
        let (_, second, second_shared) =
            fixture(TaskStatus::WaitingForApproval, TaskValidationStatus::NotRun);
        let frame = |row: &TaskRowSnapshot, shared: &DashboardRow, events| {
            let mut response = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    response = Some(task_row_button(ui, row, shared, false, 1_060_000));
                },
            );
            output.textures_delta.clear();
            response.unwrap()
        };
        let pos = frame(&first, &first_shared, vec![]).rect.center();
        let pointer = |pressed| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        assert!(!frame(&first, &first_shared, pointer(true)).clicked());
        assert!(!frame(&second, &second_shared, pointer(false)).clicked());
        assert!(!frame(&second, &second_shared, pointer(true)).clicked());
        assert!(frame(&second, &second_shared, pointer(false)).clicked());
    }

    #[test]
    fn long_search_paste_is_never_silently_truncated_or_submitted() {
        for (initial, pasted, rejected) in [
            (
                "",
                format!("provider:codex project:{} -status:failed", "x".repeat(300)),
                false,
            ),
            (
                "provider:codex",
                format!("{} -status:failed", "x".repeat(5000)),
                true,
            ),
        ] {
            let ctx = egui::Context::default();
            let mut query = initial.to_string();
            let mut frame = |events| {
                ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("agent-task-search")));
                let mut submitted = false;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        submitted = dashboard_search(
                            ui,
                            &mut query,
                            &mut DashboardQueryErrorCache::default(),
                        );
                    },
                );
                output.textures_delta.clear();
                submitted
            };
            assert!(!frame(vec![]));
            let mut events = vec![egui::Event::Paste(pasted.clone())];
            if rejected {
                events.push(egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: Some(egui::Key::Enter),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            assert!(!frame(events));
            if rejected {
                assert_eq!(
                    query, initial,
                    "oversized edit retains the complete old query"
                );
            } else {
                assert_eq!(
                    query, pasted,
                    "the core must see the full trailing exclusion"
                );
                assert!(jterm_core::agent_task::dashboard_query_error(&query).is_some());
            }
        }
    }

    #[test]
    fn enlarged_fonts_keep_all_task_card_facts_inside_the_card() {
        fn text_bounds(shape: &egui::Shape, bounds: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Text(shape) if !shape.galley.text().trim().is_empty() => {
                    bounds.push(shape.galley.rect.translate(shape.pos.to_vec2()))
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text_bounds(shape, bounds);
                    }
                }
                _ => (),
            }
        }
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| {
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(30.0));
            style
                .text_styles
                .insert(egui::TextStyle::Small, egui::FontId::proportional(24.0));
        });
        let (_, mut row, mut shared) =
            fixture(TaskStatus::ReadyForReview, TaskValidationStatus::Passed);
        row.has_active_agent_stream = true;
        shared.activity.has_active_agent_stream = true;
        let mut capture = super::super::visual_test_support::OffscreenCapture::default();
        for pass in 0..3 {
            let mut card = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(pass as f64 * 0.25),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(480.0, 360.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    card = Some(task_row_button(ui, &row, &shared, true, 1_060_000));
                },
            );
            let card = card.unwrap();
            let mut bounds = Vec::new();
            for shape in &output.shapes {
                text_bounds(&shape.shape, &mut bounds);
            }
            if let Some(path) = std::env::var_os("EMBER_AGENT_UI_SNAPSHOT_DIR").map(PathBuf::from) {
                capture.save(
                    &ctx,
                    &mut output,
                    [480, 360],
                    &path.join("task-card-scaled-fonts.png"),
                );
            } else {
                output.textures_delta.clear();
            }
            assert!(
                card.rect.height() > 120.0,
                "card grows with the configured fonts"
            );
            assert_eq!(bounds.len(), 6);
            for text in bounds {
                assert!(
                    text.bottom() <= card.rect.bottom() - 6.0,
                    "text {text:?} must not be clipped by the card {card:?}"
                );
            }
        }
    }

    #[test]
    fn command_center_queue_and_cards_render_at_narrow_and_wide_sizes() {
        fn text_shapes(shape: &egui::Shape, text: &mut String) {
            match shape {
                egui::Shape::Text(shape) => {
                    text.push_str(shape.galley.text());
                    text.push('\n');
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text_shapes(shape, text);
                    }
                }
                _ => (),
            }
        }
        let destination = std::env::var_os("EMBER_AGENT_UI_SNAPSHOT_DIR").map(PathBuf::from);
        for (name, width, height) in [
            ("task-queues-narrow", 320, 800),
            ("task-queues-wide", 760, 800),
        ] {
            let ctx = egui::Context::default();
            let (task, mut row, mut shared) =
                fixture(TaskStatus::ReadyForReview, TaskValidationStatus::Passed);
            row.has_active_agent_stream = true;
            shared.activity.has_active_agent_stream = true;
            let dashboard = DashboardSnapshot::filter_rows(
                std::slice::from_ref(&shared),
                "",
                DashboardView::All,
            );
            let mut capture = super::super::visual_test_support::OffscreenCapture::default();
            for pass in 0..3 {
                let mut query = "provider:codex".to_string();
                let mut view = DashboardView::All;
                let mut attention = None;
                let mut queue_height = 0.0;
                let mut queue_right = 0.0;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        time: Some(pass as f64 * 0.25),
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width as f32, height as f32),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        ui.strong("Agent command center");
                        assert!(!dashboard_search(
                            ui,
                            &mut query,
                            &mut DashboardQueryErrorCache::default()
                        ));
                        let queue_rect = ui
                            .scope(|ui| {
                                render_attention_queues(ui, &dashboard, &mut attention, &mut view);
                            })
                            .response
                            .rect;
                        queue_height = queue_rect.height();
                        queue_right = queue_rect.right();
                        assert!(!task_row_button(ui, &row, &shared, true, 1_060_000).clicked());
                        render_task_guidance(ui, &task_guidance(&task, &row, false, false));
                        render_task_inspector(ui, row.id, &TaskInspector::from_task(&task));
                    },
                );
                let mut rendered_text = String::new();
                for shape in &output.shapes {
                    text_shapes(&shape.shape, &mut rendered_text);
                }
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
                assert!(
                    rendered_text.contains("Ready for review"),
                    "lifecycle remains visible: {rendered_text}"
                );
                assert!(rendered_text.contains("Validation #1: Passed"));
                assert!(rendered_text.contains("Native session open"));
                assert_eq!(query, "provider:codex");
                assert_eq!(view, DashboardView::All);
                assert_eq!(attention, None);
                assert!(queue_height <= 100.0, "filter chips must wrap as whole labels at {width}px, got height {queue_height}");
                assert!(queue_right <= width as f32, "all queue controls must remain reachable inside {width}px, right edge was {queue_right}");
            }
        }
    }
}

#[cfg(test)]
mod management_v2_tests {
    use super::*;

    fn composer() -> TaskComposer {
        TaskComposer {
            open: true,
            project: "/work/terminal".into(),
            title: "Improve the Agent workspace".into(),
            goal: "Add task creation and clear review guidance.\nKeep approval decisions explicit and preserve editor drafts.".into(),
            with_validation: true,
            validation_command: "cargo test --lib".into(),
            ..Default::default()
        }
    }

    fn row(id: TaskId) -> TaskRowSnapshot {
        TaskRowSnapshot {
            id,
            title: "Review the new task manager".into(),
            provider: AgentProvider::Codex,
            status: TaskStatus::ReadyForReview,
            runtime_kind: TaskRuntimeKind::Unassigned,
            branch: "ember/task-review".into(),
            project: "/work/terminal".into(),
            has_agent_terminal: false,
            has_validation_terminal: false,
            has_active_agent_stream: false,
            native_preparing: false,
            terminal_retry_available: false,
            native_terminal_fallback_available: false,
            validation_status: TaskValidationStatus::NotRun,
            validation_attempt: 0,
            validation_detail: None,
            needs_attention: true,
            status_detail: None,
        }
    }

    #[test]
    fn composer_creates_authored_requests_without_starting_any_provider() {
        let mut draft = composer();
        for provider in AgentProvider::ALL {
            draft.provider = provider;
            let request = draft.request().unwrap();
            assert!(!request.autostart);
            assert!(request.context.is_none());
            assert_eq!(request.provider, provider);
            assert_eq!(request.cwd, PathBuf::from("/work/terminal"));
            let brief = request.brief.unwrap();
            assert_eq!(brief.goal, draft.goal);
            assert_eq!(brief.validation.unwrap().shell, "/bin/sh");
        }
        draft.validation_command = "cargo test\nrm -f marker".into();
        assert!(draft.request().is_err());
        draft.with_validation = false;
        assert!(draft.request().unwrap().brief.unwrap().validation.is_none());
        draft.project = "relative/path".into();
        assert!(draft.request().is_err());
    }

    #[test]
    fn composer_pointer_release_cannot_submit_a_replaced_project_or_goal() {
        let original = composer();
        let mut replacement = original.clone();
        replacement.project = "/work/other-project".into();
        replacement.goal = "A different goal".into();
        let ctx = egui::Context::default();
        let frame = |draft: &TaskComposer, events| {
            let mut response = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    response = Some(task_composer_create_button(ui, draft, true));
                },
            );
            output.textures_delta.clear();
            response.unwrap()
        };
        let position = frame(&original, vec![]).rect.center();
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
        assert!(!frame(&original, pointer(true)).clicked());
        assert!(!frame(&replacement, pointer(false)).clicked());
        assert!(!frame(&replacement, pointer(true)).clicked());
        assert!(frame(&replacement, pointer(false)).clicked());
    }

    #[test]
    fn composer_editor_enter_keeps_the_goal_draft_without_creating_a_task() {
        let ctx = egui::Context::default();
        let mut draft = composer();
        for focused in [false, true] {
            if focused {
                ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("task_composer_goal")));
            }
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 900.0),
                    )),
                    events: if focused {
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
                |_ui| {
                    assert!(show_task_composer(&ctx, &mut draft, &[], false).is_none());
                },
            );
            output.textures_delta.clear();
        }
        assert!(draft.open);
        assert!(draft.goal.contains("Keep approval decisions explicit"));
        assert!(draft.error.is_none());
    }

    #[test]
    fn task_action_strip_pointer_state_tracks_identity_and_lifecycle() {
        let first = row(TaskId::new());
        let mut changed_state = first.clone();
        changed_state.status = TaskStatus::Created;
        for replacement in [row(TaskId::new()), changed_state] {
            let ctx = egui::Context::default();
            let frame = |task: &TaskRowSnapshot, events| {
                let mut response = None;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        response = Some(
                            task_action_scope(ui, task, true, false, |ui| ui.button("Task action"))
                                .inner,
                        );
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
            assert!(!frame(&replacement, pointer(false)).clicked());
            assert!(!frame(&replacement, pointer(true)).clicked());
            assert!(frame(&replacement, pointer(false)).clicked());
        }
    }

    #[test]
    fn manual_accept_pointer_release_cannot_accept_a_different_task() {
        let ctx = egui::Context::default();
        let first = row(TaskId::new());
        let second = row(TaskId::new());
        let frame = |row: &TaskRowSnapshot, events| {
            let mut response = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    response = Some(task_accept_button(ui, row, true, true));
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
        assert!(!frame(&second, pointer(false)).clicked());
        assert!(!frame(&second, pointer(true)).clicked());
        assert!(frame(&second, pointer(false)).clicked());
    }

    #[test]
    fn task_composer_and_inspector_render_without_staging_execution() {
        let destination = std::env::var_os("EMBER_AGENT_UI_SNAPSHOT_DIR").map(PathBuf::from);
        for (name, width, height) in [
            ("task-composer-wide", 1180, 820),
            ("task-composer-narrow", 700, 800),
        ] {
            let ctx = egui::Context::default();
            let mut draft = composer();
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
                        assert!(show_task_composer(
                            &ctx,
                            &mut draft,
                            &[PathBuf::from("/work/terminal")],
                            false
                        )
                        .is_none());
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
        }
        let mut manager = crate::agent::TaskManager::new();
        let id = manager
            .create_with_brief(
                crate::agent::NewTask {
                    title: "Improve the Agent workspace".into(),
                    provider: AgentProvider::Codex,
                    repo_root: "/work/terminal".into(),
                    worktree_path: "/work/tasks/task-review".into(),
                    branch: "ember/task-review".into(),
                    base_commit: "a".repeat(40),
                    source_context: None,
                },
                TaskBrief::new(composer().goal, None).unwrap(),
            )
            .unwrap();
        let mut reviewed = manager.get(id).unwrap().clone();
        reviewed.status = TaskStatus::ReadyForReview;
        let inspector = TaskInspector::from_task(&reviewed);
        let ctx = egui::Context::default();
        let mut capture = super::super::visual_test_support::OffscreenCapture::default();
        for pass in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(pass as f64 * 0.25),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(720.0, 640.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    render_task_guidance(ui, &task_guidance(&reviewed, &row(id), false, false));
                    render_task_inspector(ui, id, &inspector);
                    assert!(!task_accept_button(ui, &row(id), true, true).clicked());
                },
            );
            if let Some(path) = &destination {
                capture.save(
                    &ctx,
                    &mut output,
                    [720, 640],
                    &path.join("task-inspector.png"),
                );
            } else {
                output.textures_delta.clear();
            }
        }
        assert_eq!(manager.get(id).unwrap().status, TaskStatus::Created);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dashboard_filter_labels_map_activity_and_finished_counts() {
        use jterm_core::agent_task::{DashboardCounts, DashboardView};
        let counts = DashboardCounts {
            total: 20,
            attention: 7,
            running: 9,
            created: 3,
            review: 4,
            done: 2,
            failed: 5,
            cancelled: 1,
            archived: 99,
            ..Default::default()
        };
        for (view, expected) in [
            (DashboardView::All, "All 20"),
            (DashboardView::NeedsAttention, "Needs you 7"),
            (DashboardView::Running, "Running 9"),
            (DashboardView::NotStarted, "Not started 3"),
            (DashboardView::Review, "Review 4"),
            (DashboardView::Finished, "Finished 8"),
        ] {
            assert_eq!(super::dashboard_filter_label(view, &counts), expected);
        }
        assert_eq!(
            super::dashboard_filter_label(DashboardView::Running, &DashboardCounts::default()),
            "Running 0"
        );
    }

    #[test]
    fn print_provider_notice_discloses_permissions_without_promising_a_sandbox() {
        use super::{native_start_notice, AgentProvider};
        let claude = native_start_notice(AgentProvider::Claude).unwrap();
        assert!(claude.contains("keep the CLI permission checks"));
        assert!(claude.contains("Terminal"));
        let kimi = native_start_notice(AgentProvider::Kimi).unwrap();
        assert!(kimi.contains("automatic tool permission"));
        assert!(kimi.contains("user account's file access"));
        assert!(kimi.contains("not a sandbox"));
        assert_eq!(native_start_notice(AgentProvider::Codex), None);
        assert_eq!(native_start_notice(AgentProvider::OpenCode), None);
    }
    use super::*;

    #[test]
    fn dashboard_search_enter_is_local_and_ime_commits_are_not_submissions() {
        let context = egui::Context::default();
        let mut query = "review".to_string();
        let mut frame = |events: Vec<egui::Event>, focused: bool| {
            if focused {
                context
                    .memory_mut(|memory| memory.request_focus(egui::Id::new("agent-task-search")));
            }
            let mut submitted = false;
            let mut output = context.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    submitted =
                        dashboard_search(ui, &mut query, &mut DashboardQueryErrorCache::default());
                },
            );
            output.textures_delta.clear();
            submitted
        };
        let enter = || egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(
            !frame(vec![enter()], false),
            "terminal/global Enter is not a search action"
        );
        assert!(!frame(vec![], true), "focus and typing are not submission");
        assert!(frame(vec![enter()], true));
        assert!(!frame(
            vec![
                egui::Event::Ime(egui::ImeEvent::Commit("结果".into())),
                enter()
            ],
            true
        ));
    }

    #[test]
    fn dashboard_rows_follow_shared_order_and_filter_by_stable_id() {
        use crate::agent::{NewTask, TaskManager};
        use jterm_core::agent_task::{
            DashboardActivity, DashboardFilter, DashboardSnapshot, DashboardView,
        };
        let mut manager = TaskManager::new();
        for title in ["done", "preparing", "working", "failed-old", "waiting-new"] {
            manager
                .create(NewTask {
                    title: title.into(),
                    provider: AgentProvider::Codex,
                    repo_root: "/fixture".into(),
                    worktree_path: "/fixture/worktree".into(),
                    branch: "test/dashboard".into(),
                    base_commit: "a".repeat(40),
                    source_context: None,
                })
                .unwrap();
        }
        let mut tasks = manager.tasks().to_vec();
        for (task, (status, timestamp)) in tasks.iter_mut().zip([
            (TaskStatus::Completed, 50),
            (TaskStatus::Created, 30),
            (TaskStatus::Working, 10),
            (TaskStatus::Failed, 1),
            (TaskStatus::WaitingForHuman, 20),
        ]) {
            task.status = status;
            task.updated_at_ms = timestamp;
        }
        let make_rows = || {
            tasks
                .iter()
                .map(|task| TaskRowSnapshot {
                    id: task.id,
                    title: task.title.clone(),
                    provider: task.provider,
                    status: task.status,
                    runtime_kind: task.runtime_kind,
                    branch: task.branch.clone(),
                    project: task.repo_root.to_string_lossy().into_owned(),
                    has_agent_terminal: false,
                    has_validation_terminal: false,
                    has_active_agent_stream: false,
                    native_preparing: task.title == "preparing",
                    terminal_retry_available: false,
                    native_terminal_fallback_available: false,
                    validation_status: TaskValidationStatus::NotRun,
                    validation_attempt: 0,
                    validation_detail: None,
                    needs_attention: task.needs_attention(),
                    status_detail: None,
                })
                .collect::<Vec<_>>()
        };
        let activity = |id| DashboardActivity {
            native_preparing: id == tasks[1].id,
            ..Default::default()
        };
        let all = DashboardSnapshot::project(&tasks, activity, DashboardFilter::default());
        let ordered = rows_in_dashboard_order(make_rows(), &all);
        assert_eq!(
            ordered
                .iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            ["waiting-new", "failed-old", "preparing", "working", "done"]
        );
        let filtered = DashboardSnapshot::project(
            &tasks,
            activity,
            DashboardFilter {
                view: DashboardView::NeedsAttention,
                ..Default::default()
            },
        );
        let mut reversed = make_rows();
        reversed.reverse();
        let ordered = rows_in_dashboard_order(reversed, &filtered);
        assert_eq!(
            ordered.iter().map(|row| row.id).collect::<Vec<_>>(),
            [tasks[4].id, tasks[3].id]
        );
        assert!(rows_in_dashboard_order(Vec::new(), &filtered).is_empty());
    }

    #[test]
    fn native_approvals_remain_deny_only_even_for_a_frozen_patch() {
        let base = crate::agent::CodexAppServerApproval {
            id: ApprovalId::new(),
            kind: CodexAppServerApprovalKind::Command,
            item_id: "item-1".into(),
            command: Some("cargo test".into()),
            cwd: Some("/worktree".into()),
            reason: None,
            file_paths: Vec::new(),
            file_changes: Vec::new(),
        };
        assert!(!native_approval_can_allow(&base, true));

        let file = crate::agent::CodexAppServerApproval {
            kind: CodexAppServerApprovalKind::FileChange,
            file_paths: vec!["src/main.rs".into()],
            file_changes: vec![crate::agent::CodexAppServerApprovalFileChange {
                path: "src/main.rs".into(),
                kind: "{\"type\":\"update\"}".into(),
                diff: "@@ -1 +1 @@\n-old\n+new".into(),
                move_path: None,
            }],
            ..base
        };
        assert!(!native_approval_can_allow(&file, true));
        assert!(!native_approval_can_allow(&file, false));
    }

    #[test]
    fn display_boundary_neutralizes_bidi_and_bounds_text() {
        let rendered = visible_bounded("safe\u{202e}spoof", 32);
        assert_eq!(rendered, "safe\\u{202E}spoof");
        assert!(visible_bounded(&"x".repeat(500), 20).len() <= 20);
        assert_eq!(native_feedback_display("first\nsecond"), "first\nsecond");
        let hostile_feedback = native_feedback_display("safe\u{202e}\0spoof");
        assert!(!hostile_feedback.contains('\u{202e}'));
        assert!(!hostile_feedback.contains('\0'));
        assert!(hostile_feedback.contains("\\u{202E}"));
    }

    #[test]
    fn follow_up_send_predicate_uses_the_exact_utf8_byte_budget() {
        assert!(!native_follow_up_can_send(" \n\t", 0));
        assert!(native_follow_up_can_send(
            "Please fix the remaining test",
            1
        ));
        assert!(native_follow_up_can_send(
            &"界".repeat(NATIVE_AGENT_FOLLOW_UP_MAX_BYTES / "界".len()),
            1,
        ));
        assert!(!native_follow_up_can_send(
            &"界".repeat(NATIVE_AGENT_FOLLOW_UP_MAX_BYTES / "界".len() + 1),
            1,
        ));
        assert!(native_follow_up_can_send(
            &"x".repeat(NATIVE_AGENT_FOLLOW_UP_MAX_BYTES),
            CODEX_APP_SERVER_LIVE_TURN_MAX - 1,
        ));
        assert!(!native_follow_up_can_send(
            "one turn too many",
            CODEX_APP_SERVER_LIVE_TURN_MAX,
        ));
    }

    #[test]
    fn native_history_projection_is_ordered_deduplicated_and_excludes_the_flat_turn() {
        fn turn(
            ordinal: usize,
            local_turn_id: crate::agent::AgentTurnId,
        ) -> CodexAppServerTurnHistory {
            CodexAppServerTurnHistory {
                ordinal,
                local_turn_id,
                follow_up_feedback: (ordinal > 1).then(|| format!("feedback-{ordinal}")),
                agent_text: format!("answer-{ordinal}"),
                agent_text_truncated: false,
                commands: Vec::new(),
                file_changes: Vec::new(),
                dropped_updates: 0,
            }
        }

        let first = crate::agent::AgentTurnId::new();
        let latest = crate::agent::AgentTurnId::new();
        let mut view = CodexAppServerViewSnapshot {
            phase: CodexAppServerPhase::Ready,
            displayed_turn_id: Some(latest),
            displayed_turn_ordinal: Some(2),
            completed_turns: 2,
            turn_history: Arc::from([turn(1, first), turn(1, first), turn(2, latest)]),
            ..CodexAppServerViewSnapshot::default()
        };

        let visible = visible_native_turn_history(&view);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].local_turn_id, first);
        assert_eq!(visible[0].ordinal, 1);
        assert_eq!(native_flat_turn_heading(&view), Some(("Latest turn", 2)));

        view.phase = CodexAppServerPhase::Running;
        view.displayed_turn_ordinal = Some(3);
        assert_eq!(native_flat_turn_heading(&view), Some(("Current turn", 3)));
        view.phase = CodexAppServerPhase::Failed;
        assert_eq!(native_flat_turn_heading(&view), Some(("Stopped turn", 3)));
        view.phase = CodexAppServerPhase::Ended;
        assert_eq!(native_flat_turn_heading(&view), Some(("Stopped turn", 3)));
        view.completed_turns = 3;
        assert_eq!(native_flat_turn_heading(&view), Some(("Latest turn", 3)));
    }
}

/// Return feedback only for a newly observed authoritative transition.
fn apply_task_terminal_exit(
    manager: &mut crate::agent::TaskManager,
    session_id: &str,
    exit_code: Option<i32>,
) -> (bool, Option<String>) {
    let before = manager
        .task_and_role_for_terminal_session(session_id)
        .map(|(task, role)| {
            (
                task.id,
                role,
                task.status,
                task.validation.status,
                task.validation.attempt,
            )
        });
    let handled = manager
        .handle_terminal_session_exit(session_id, exit_code)
        .is_some();
    let Some((id, role, status_before, validation_before, attempt)) = before else {
        return (handled, None);
    };
    let message = match role {
        crate::agent::TaskTerminalRole::Validation => manager
            .get(id)
            .filter(|task| {
                validation_before == TaskValidationStatus::Running
                    && task.validation.attempt == attempt
                    && task.validation.terminal_session_id.as_deref() == Some(session_id)
            })
            .and_then(|_| task_validation_feedback(manager, id, session_id)),
        crate::agent::TaskTerminalRole::Agent => manager.get(id).and_then(|task| {
            if task.status == status_before
                || task.terminal_session_id.as_deref() != Some(session_id)
            {
                return None;
            }
            let outcome = match task.status {
                TaskStatus::ReadyForReview => AgentSessionOutcome::Clean,
                TaskStatus::Cancelled => AgentSessionOutcome::Cancelled,
                TaskStatus::Failed => AgentSessionOutcome::Failed,
                _ => return None,
            };
            task_completion_feedback(manager, [(id, outcome)])
        }),
    };
    (handled, message)
}

/// A bounded, redacted title and a stable ID prefix; never use prompt/output text.
fn feedback_identity(manager: &crate::agent::TaskManager, id: TaskId) -> String {
    let full = id.to_string();
    let peers: Vec<_> = manager
        .tasks()
        .iter()
        .filter(|task| task.id != id)
        .map(|task| task.id.to_string())
        .collect();
    let length = feedback_id_prefix_length(&full, &peers);
    let title = manager
        .get(id)
        .map(|task| feedback_title(&task.title))
        .unwrap_or_else(|| "Task no longer available".into());
    format!("{title} [{}]", &full[..length])
}

fn feedback_id_prefix_length(full: &str, peers: &[String]) -> usize {
    let mut length = 8;
    while length < full.len() && peers.iter().any(|peer| peer.starts_with(&full[..length])) {
        length = (length + 4).min(full.len());
    }
    length
}

fn feedback_title(title: &str) -> String {
    // Redact before bounding so a secret cannot evade detection at the cut point.
    let redacted = jterm_core::redact::redact_secrets(title);
    visible_bounded(
        &redacted.split_whitespace().collect::<Vec<_>>().join(" "),
        64,
    )
}

/// Failed native workers can finish after their task has been hidden. Keep a
/// bounded diagnostic here even when the Tasks dashboard cannot show that row.
fn native_completion_feedback<'a>(
    manager: &crate::agent::TaskManager,
    completions: impl IntoIterator<Item = (TaskId, AgentSessionOutcome, Option<&'a str>)>,
) -> Option<String> {
    let completions: Vec<_> = completions.into_iter().collect();
    let mut message = task_completion_feedback(
        manager,
        completions.iter().map(|(id, outcome, _)| (*id, *outcome)),
    )?;
    if let Some((id, _, detail)) = completions
        .iter()
        .rev()
        .find(|(_, outcome, _)| *outcome == AgentSessionOutcome::Failed)
    {
        let detail = detail
            .filter(|text| !text.trim().is_empty())
            .unwrap_or("provider session did not complete");
        // Redact the complete diagnostic before truncation, as for task titles.
        let redacted = jterm_core::redact::redact_secrets(detail);
        let detail = visible_bounded(
            &redacted.split_whitespace().collect::<Vec<_>>().join(" "),
            112,
        );
        // Put the diagnostic before batch context so the status-line byte cap
        // cannot erase the only remaining failure explanation.
        let failure = format!(
            "{}: Agent session failed: {detail}",
            feedback_identity(manager, *id)
        );
        message = if completions.len() > 1 {
            format!("{failure}; {message}")
        } else {
            failure
        };
    }
    Some(message)
}

pub(crate) fn task_completion_feedback(
    manager: &crate::agent::TaskManager,
    completions: impl IntoIterator<Item = (TaskId, crate::agent::AgentSessionOutcome)>,
) -> Option<String> {
    use crate::agent::AgentSessionOutcome;
    let completions: Vec<_> = completions.into_iter().collect();
    let (id, outcome) = *completions.first()?;
    if completions.len() > 1 {
        let failed = completions
            .iter()
            .filter(|(_, outcome)| *outcome == AgentSessionOutcome::Failed)
            .count();
        let cancelled = completions
            .iter()
            .filter(|(_, outcome)| *outcome == AgentSessionOutcome::Cancelled)
            .count();
        return Some(format!("{} agent sessions stopped: {} clean, {cancelled} cancelled, {failed} failed; open Tasks for details", completions.len(), completions.len() - failed - cancelled));
    }
    let outcome = match outcome {
        AgentSessionOutcome::Clean => {
            "Agent session stopped cleanly; review changes, then validate"
        }
        AgentSessionOutcome::Cancelled => "Agent session cancelled and stopped",
        AgentSessionOutcome::Failed => "Agent session failed; open Tasks for details",
    };
    Some(format!("{}: {outcome}", feedback_identity(manager, id)))
}

pub(crate) fn task_validation_feedback(
    manager: &crate::agent::TaskManager,
    id: TaskId,
    session: &str,
) -> Option<String> {
    use crate::agent::TaskValidationStatus;
    let task = manager.get(id)?;
    let validation = &task.validation;
    if validation.terminal_session_id.as_deref() != Some(session) {
        return None;
    }
    let outcome = match validation.status {
        TaskValidationStatus::NotRun | TaskValidationStatus::Running => return None,
        TaskValidationStatus::Passed => "passed".into(),
        TaskValidationStatus::Failed => validation
            .exit_code
            .map(|code| format!("failed (exit {code})"))
            .unwrap_or_else(|| "failed; open Tasks for details".into()),
        TaskValidationStatus::Cancelled => "cancelled".into(),
        TaskValidationStatus::Inconclusive => {
            "ended without a confirmed result; open Tasks for details".into()
        }
    };
    Some(format!(
        "{}: Validation #{} {outcome}",
        feedback_identity(manager, id),
        validation.attempt
    ))
}

#[cfg(test)]
mod feedback_tests {
    use super::*;
    use crate::agent::{AgentSessionOutcome, NewTask, TaskManager, TaskStatus};

    fn add_task(manager: &mut TaskManager, title: &str) -> TaskId {
        manager
            .create(NewTask {
                title: title.into(),
                provider: AgentProvider::Codex,
                repo_root: "/fixture".into(),
                worktree_path: "/fixture/worktree".into(),
                branch: "test/feedback".into(),
                base_commit: "a".repeat(40),
                source_context: None,
            })
            .unwrap()
    }

    #[test]
    fn terminal_feedback_is_once_only_and_rejects_old_validation_sessions() {
        let mut manager = TaskManager::new();
        let id = add_task(&mut manager, "Originating task");
        manager.bind_terminal_session(id, "agent".into()).unwrap();
        let (handled, message) = apply_task_terminal_exit(&mut manager, "agent", Some(0));
        assert!(handled);
        assert!(message.unwrap().contains("session stopped cleanly"));
        assert!(apply_task_terminal_exit(&mut manager, "agent", Some(0))
            .1
            .is_none());
        manager
            .bind_validation_session(id, "validation-one".into())
            .unwrap();
        assert!(
            apply_task_terminal_exit(&mut manager, "validation-one", None)
                .1
                .unwrap()
                .contains("without a confirmed result")
        );
        assert!(
            apply_task_terminal_exit(&mut manager, "validation-one", Some(0))
                .1
                .is_none()
        );
        manager
            .bind_validation_session(id, "validation-two".into())
            .unwrap();
        assert_eq!(
            apply_task_terminal_exit(&mut manager, "validation-one", Some(0)),
            (false, None)
        );
        assert!(
            apply_task_terminal_exit(&mut manager, "validation-two", Some(0))
                .1
                .unwrap()
                .ends_with("Validation #2 passed")
        );
        assert!(
            apply_task_terminal_exit(&mut manager, "validation-two", Some(7))
                .1
                .is_none()
        );
    }

    #[test]
    fn native_failed_detail_survives_missing_or_archived_task_safely() {
        let mut manager = TaskManager::new();
        let archived = add_task(&mut manager, "Hidden task");
        manager.archive(archived).unwrap();
        let missing = TaskId::new();
        let secret = format!("sk-{}", "a".repeat(32));
        let detail = format!("worker join failed\n{secret} \u{202e}{}", "界".repeat(400));
        for id in [archived, missing] {
            let text = native_completion_feedback(
                &manager,
                [(id, AgentSessionOutcome::Failed, Some(detail.as_str()))],
            )
            .unwrap();
            assert!(text.contains(&id.to_string()[..8]));
            assert!(text.contains("worker join failed"));
            assert!(
                crate::review_text::bound_toast_text(text.clone()).contains("worker join failed")
            );
            assert!(!text.contains(&secret));
            assert!(!text.chars().any(char::is_control));
            assert!(!text.contains('\u{202e}'));
            assert!(text.len() < 512);
        }
        let batch = native_completion_feedback(
            &manager,
            [
                (archived, AgentSessionOutcome::Failed, Some("join failed")),
                (missing, AgentSessionOutcome::Clean, None),
            ],
        )
        .unwrap();
        assert!(batch.contains(&archived.to_string()[..8]));
        assert!(batch.contains("join failed"));
        assert!(native_completion_feedback(
            &manager,
            [(missing, AgentSessionOutcome::Failed, None)]
        )
        .unwrap()
        .contains("provider session did not complete"));
    }

    #[test]
    fn completion_feedback_identifies_same_title_tasks_and_missing_task() {
        let mut manager = TaskManager::new();
        let a = add_task(&mut manager, "Same title");
        let b = add_task(&mut manager, "Same title");
        let message =
            |id| task_completion_feedback(&manager, [(id, AgentSessionOutcome::Failed)]).unwrap();
        assert_ne!(message(a), message(b));
        assert!(message(a).contains(&a.to_string()[..8]));
        assert!(message(b).contains(&b.to_string()[..8]));
        let missing = TaskId::new();
        assert!(message(missing).contains("Task no longer available"));
        assert!(message(missing).contains(&missing.to_string()[..8]));
        assert!(task_completion_feedback(&manager, []).is_none());
        let batch = task_completion_feedback(
            &manager,
            [
                (a, AgentSessionOutcome::Clean),
                (b, AgentSessionOutcome::Cancelled),
                (a, AgentSessionOutcome::Failed),
            ],
        )
        .unwrap();
        assert_eq!(
            batch,
            "3 agent sessions stopped: 1 clean, 1 cancelled, 1 failed; open Tasks for details"
        );
        assert!(!batch.contains("3 tasks"));
    }

    #[test]
    fn feedback_id_prefix_extends_for_live_collisions() {
        let a = "12345678-1111-4111-8111-111111111111";
        let b = "12345678-2222-4222-8222-222222222222";
        assert_eq!(feedback_id_prefix_length(a, &[]), 8);
        let length = feedback_id_prefix_length(a, &[b.into()]);
        assert_eq!(length, 12);
        assert_ne!(&a[..length], &b[..length]);
    }

    #[test]
    fn feedback_title_is_redacted_single_line_and_utf8_bounded() {
        let secret = format!("sk-{}", "a".repeat(32));
        assert!(!feedback_title(&format!("Fix {secret}")).contains(&secret));
        let hostile = feedback_title("one\n\t\u{202e}two\u{2066}\u{1b}[31m");
        assert!(!hostile.chars().any(char::is_control));
        assert!(!hostile.contains('\u{202e}'));
        assert!(!hostile.contains('\u{2066}'));
        assert!(hostile.contains("one "));
        let long = feedback_title(&"界🙂".repeat(80));
        assert!(long.len() <= 64);
        assert!(long.is_char_boundary(long.len()));
        assert!(long.ends_with('…'));
    }

    #[test]
    fn validation_feedback_uses_bound_attempt_and_stored_result() {
        let mut manager = TaskManager::new();
        let a = add_task(&mut manager, "Same title");
        let b = add_task(&mut manager, "Same title");
        for (id, session) in [(a, "agent-a"), (b, "agent-b")] {
            manager.bind_terminal_session(id, session.into()).unwrap();
            manager
                .update_status(id, TaskStatus::ReadyForReview, None)
                .unwrap();
        }
        manager
            .bind_validation_session(a, "validation-a".into())
            .unwrap();
        manager
            .bind_validation_session(b, "validation-b".into())
            .unwrap();
        assert!(task_validation_feedback(&manager, a, "validation-a").is_none());
        manager.handle_terminal_session_exit("validation-a", Some(7));
        assert_eq!(
            manager.handle_terminal_session_closed("validation-a"),
            Some(a)
        );
        let failed = task_validation_feedback(&manager, a, "validation-a").unwrap();
        assert!(failed.contains(&a.to_string()[..8]));
        assert!(failed.ends_with("Validation #1 failed (exit 7)"));
        assert!(task_validation_feedback(&manager, b, "validation-a").is_none());
        manager
            .bind_validation_session(a, "validation-a-retry".into())
            .unwrap();
        assert!(manager
            .handle_terminal_session_exit("validation-a", Some(0))
            .is_none());
        assert!(task_validation_feedback(&manager, a, "validation-a").is_none());
        assert!(task_validation_feedback(&manager, a, "validation-a-retry").is_none());
        manager.handle_terminal_session_exit("validation-a-retry", Some(0));
        assert_eq!(
            manager.handle_terminal_session_closed("validation-a-retry"),
            Some(a)
        );
        assert!(task_validation_feedback(&manager, b, "validation-b").is_none());
        assert!(task_validation_feedback(&manager, a, "validation-a-retry")
            .unwrap()
            .ends_with("Validation #2 passed"));
        assert!(task_validation_feedback(&TaskManager::new(), a, "validation-a-retry").is_none());
    }
}

#[cfg(test)]
mod creation_recovery_ui_tests {
    use super::*;

    #[test]
    fn creation_recovery_start_guard_uses_exact_task_id_and_reconciles() {
        let first = TaskId::new();
        let second = TaskId::new();
        let mut pending = HashSet::from([first]);
        for action in [
            TaskSidebarAction::StartNative(first),
            TaskSidebarAction::StartTerminal(first),
            TaskSidebarAction::RunValidation(first),
        ] {
            assert!(task_action_waits_for_recovery(&action, &pending));
        }
        assert!(!task_action_waits_for_recovery(
            &TaskSidebarAction::StartNative(second),
            &pending
        ));
        assert!(!task_action_waits_for_recovery(
            &TaskSidebarAction::Archive(first),
            &pending
        ));
        pending.remove(&first);
        assert!(!task_action_waits_for_recovery(
            &TaskSidebarAction::StartNative(first),
            &pending
        ));
    }
    #[test]
    fn creation_recovery_marker_reconciliation_and_late_autostart_are_identity_safe() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/task-recovery/stages.json"
        ))
        .unwrap();
        let mut record: crate::agent::recovery::Record =
            serde_json::from_value(value["records"][1].clone()).unwrap();
        let mut manager = crate::agent::TaskManager::new();
        let id = manager.create(record.new_task().unwrap()).unwrap();
        assert!(!registration_marker_confirms(
            manager.get(id).unwrap(),
            &record
        ));
        record.stage = crate::agent::recovery::Stage::Registered;
        record.registered_task_id = Some(id);
        assert!(registration_marker_confirms(
            manager.get(id).unwrap(),
            &record
        ));
        assert!(registration_may_autostart(&manager, id, true));
        assert!(
            !registration_may_autostart(&manager, id, false),
            "recovery is never automatic Start"
        );
        let mut replaced = record.clone();
        replaced.provider = AgentProvider::Claude;
        assert!(!registration_marker_confirms(
            manager.get(id).unwrap(),
            &replaced
        ));
        replaced = record.clone();
        replaced.path.push("unrelated");
        assert!(!registration_marker_confirms(
            manager.get(id).unwrap(),
            &replaced
        ));
        manager.archive(id).unwrap();
        assert!(
            !registration_may_autostart(&manager, id, true),
            "late worker must not launch archived task"
        );
        assert!(!registration_may_autostart(&manager, TaskId::new(), true));
    }
    #[test]
    fn creation_recovery_dismiss_cannot_orphan_pending_runtime_confirmation() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/task-recovery/stages.json"
        ))
        .unwrap();
        let mut record: crate::agent::recovery::Record =
            serde_json::from_value(value["records"][1].clone()).unwrap();
        let mut manager = crate::agent::TaskManager::new();
        let id = manager.create(record.new_task().unwrap()).unwrap();
        let pending = HashSet::from([id]);
        assert!(recovery_dismiss_blocked(&manager, &pending, &record));
        record.stage = crate::agent::recovery::Stage::Registered;
        assert!(
            recovery_dismiss_blocked(&manager, &pending, &record),
            "uncertain published marker still needs Reload"
        );
        assert!(!recovery_dismiss_blocked(
            &manager,
            &HashSet::new(),
            &record
        ));
        let other: crate::agent::recovery::Record =
            serde_json::from_value(value["records"][2].clone()).unwrap();
        assert!(!recovery_dismiss_blocked(&manager, &pending, &other));
        record.provider = AgentProvider::Claude;
        assert!(!recovery_dismiss_blocked(&manager, &pending, &record));
    }
    #[test]
    fn creation_recovery_registered_marker_unblocks_only_winning_runtime_task() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/task-recovery/stages.json"
        ))
        .unwrap();
        let mut record: crate::agent::recovery::Record =
            serde_json::from_value(value["records"][1].clone()).unwrap();
        let mut first = crate::agent::TaskManager::new();
        let mut second = crate::agent::TaskManager::new();
        let first_id = first.create(record.new_task().unwrap()).unwrap();
        // First window's marker failed, so second window can recover Prepared.
        let winner = second.create(record.new_task().unwrap()).unwrap();
        assert_ne!(first_id, winner);
        record.stage = crate::agent::recovery::Stage::Registered;
        record.registered_task_id = Some(winner);
        assert!(!registration_marker_confirms(
            first.get(first_id).unwrap(),
            &record
        ));
        assert!(registration_marker_confirms(
            second.get(winner).unwrap(),
            &record
        ));
        assert!(
            record.new_task().is_err(),
            "restart never recreates a registered task"
        );
    }
}

#[cfg(test)]
fn record_approval_rect(
    ui: &egui::Ui,
    task: TaskId,
    approval: ApprovalId,
    kind: &'static str,
    response: &egui::Response,
) {
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("approval-test-rect", task, approval, kind)),
            response.rect,
        );
    });
}

#[cfg(test)]
mod approval_navigation_tests {
    use super::*;

    fn fixture() -> CodexAppServerViewSnapshot {
        CodexAppServerViewSnapshot {
            pending_approvals: vec![crate::agent::CodexAppServerApproval {
                id: ApprovalId::new(),
                kind: CodexAppServerApprovalKind::Command,
                item_id: "presentation-only".into(),
                command: Some("cargo test".into()),
                cwd: None,
                reason: None,
                file_paths: Vec::new(),
                file_changes: Vec::new(),
            }],
            ..Default::default()
        }
    }

    fn rect(
        ctx: &egui::Context,
        task: TaskId,
        approval: ApprovalId,
        kind: &'static str,
    ) -> egui::Rect {
        ctx.data(|data| {
            data.get_temp(egui::Id::new(("approval-test-rect", task, approval, kind)))
                .unwrap()
        })
    }

    struct Frame {
        action: Option<TaskSidebarAction>,
        offset: f32,
        clip: egui::Rect,
        navigation_left: Option<(TaskId, ApprovalId)>,
    }

    // Calls the production navigation, full native renderer and guard logic.
    // No TerminalApp construction, providers, sockets, PTYs or GUI backend.
    fn frame(
        ctx: &egui::Context,
        task: TaskId,
        view: &CodexAppServerViewSnapshot,
        events: Vec<egui::Event>,
        offset: Option<f32>,
        enabled: bool,
        stale: Option<(TaskId, ApprovalId)>,
    ) -> Frame {
        let mut result = Frame {
            action: None,
            offset: 0.0,
            clip: egui::Rect::NOTHING,
            navigation_left: None,
        };
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(340.0, 440.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                let mut navigation = native_approval_navigation(ui, task, view).or(stale);
                retain_approval_navigation(&mut navigation, Some(task), Some(view));
                let mut scroll = egui::ScrollArea::vertical()
                    .id_salt("approval-headless-list")
                    .animated(navigation.is_none());
                if let Some(offset) = offset {
                    scroll = scroll.vertical_scroll_offset(offset);
                }
                let shown = scroll.show(ui, |ui| {
                    ui.add_space(600.0); // Earlier task rows, not a provider fixture.
                    render_native_codex_view(
                        ui,
                        task,
                        AgentProvider::Codex,
                        view,
                        enabled,
                        &mut result.action,
                        &mut navigation,
                    );
                    ui.add_space(1600.0);
                });
                result.offset = shown.state.offset.y;
                result.clip = shown.inner_rect;
                result.navigation_left = navigation;
            },
        );
        let _ = ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        // This CPU-only harness deliberately has no renderer/texture consumer.
        output.drop_without_applying_deltas();
        result
    }

    fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn native_approval_view_click_navigates_without_deciding_and_streaming_does_not_scroll() {
        let ctx = egui::Context::default();
        let task = TaskId::new();
        let mut view = fixture();
        view.dropped_turns = 3;
        view.dropped_updates = 7;
        view.turn_history = Arc::from(
            (1..=8)
                .map(|ordinal| CodexAppServerTurnHistory {
                    ordinal,
                    local_turn_id: crate::agent::AgentTurnId::new(),
                    follow_up_feedback: Some("history feedback".into()),
                    agent_text: "Long historical output ".repeat(3000),
                    agent_text_truncated: false,
                    commands: Vec::new(),
                    file_changes: Vec::new(),
                    dropped_updates: 0,
                })
                .collect::<Vec<_>>(),
        );
        let approval = view.pending_approvals[0].id;
        for _ in 0..3 {
            frame(&ctx, task, &view, vec![], Some(1300.0), true, None);
        }
        let pos = rect(&ctx, task, approval, "view").center();
        assert!(pos.y < 80.0, "navigation must remain above the scroller");
        frame(&ctx, task, &view, pointer(pos, true), None, true, None);
        let clicked = frame(&ctx, task, &view, pointer(pos, false), None, true, None);
        assert_eq!(clicked.action, None);
        assert_eq!(clicked.navigation_left, None);
        let settled = frame(&ctx, task, &view, vec![], None, true, None);
        assert!(settled
            .clip
            .intersects(rect(&ctx, task, approval, "heading")));
        let before = frame(&ctx, task, &view, vec![], Some(1000.0), true, None).offset;
        view.agent_text = "Current output ".repeat(2000);
        let after = frame(&ctx, task, &view, vec![], None, true, None);
        assert!(
            (after.offset - before).abs() < 1.0,
            "output must not request a jump"
        );
        assert!(after.action.is_none());
    }

    #[test]
    fn native_approval_buttons_keep_exact_authority_and_deny_only_guard() {
        let ctx = egui::Context::default();
        let task = TaskId::new();
        let view = fixture();
        let approval = view.pending_approvals[0].id;
        for _ in 0..3 {
            frame(&ctx, task, &view, vec![], Some(600.0), true, None);
        }
        let allow = rect(&ctx, task, approval, "allow").center();
        frame(&ctx, task, &view, pointer(allow, true), None, true, None);
        assert!(
            frame(&ctx, task, &view, pointer(allow, false), None, true, None)
                .action
                .is_none()
        );
        let deny = rect(&ctx, task, approval, "deny").center();
        frame(&ctx, task, &view, pointer(deny, true), None, true, None);
        assert_eq!(
            frame(&ctx, task, &view, pointer(deny, false), None, true, None).action,
            Some(TaskSidebarAction::Deny(task, approval))
        );
        frame(&ctx, task, &view, pointer(deny, true), None, false, None);
        assert!(
            frame(&ctx, task, &view, pointer(deny, false), None, false, None)
                .action
                .is_none()
        );
    }

    #[test]
    fn native_approval_pointer_release_cannot_retarget_changed_task_or_request() {
        for change_task in [false, true] {
            let ctx = egui::Context::default();
            let task = TaskId::new();
            let mut view = fixture();
            let old = view.pending_approvals[0].id;
            for _ in 0..3 {
                frame(&ctx, task, &view, vec![], Some(600.0), true, None);
            }
            let pos = rect(&ctx, task, old, "deny").center();
            frame(&ctx, task, &view, pointer(pos, true), None, true, None);
            let next_task = if change_task { TaskId::new() } else { task };
            if !change_task {
                view.pending_approvals[0].id = ApprovalId::new();
            }
            assert!(frame(
                &ctx,
                next_task,
                &view,
                pointer(pos, false),
                None,
                true,
                Some((task, old))
            )
            .action
            .is_none());
        }
    }
    #[test]
    fn native_approval_navigation_discards_removed_hidden_and_replaced_targets() {
        let task = TaskId::new();
        let view = fixture();
        let approval = view.pending_approvals[0].id;
        for (selected, snapshot) in [(None, Some(&view)), (Some(task), None)] {
            let mut target = Some((task, approval));
            retain_approval_navigation(&mut target, selected, snapshot);
            assert_eq!(target, None);
        }
        let replacement = fixture();
        let mut target = Some((task, approval));
        retain_approval_navigation(&mut target, Some(task), Some(&replacement));
        assert_eq!(target, None);
        let mut target = Some((task, approval));
        retain_approval_navigation(&mut target, Some(task), Some(&view));
        assert_eq!(target, Some((task, approval)));

        let ctx = egui::Context::default();
        for _ in 0..3 {
            frame(&ctx, task, &view, vec![], Some(1300.0), true, None);
        }
        let pos = rect(&ctx, task, approval, "view").center();
        frame(&ctx, task, &view, pointer(pos, true), None, true, None);
        let after = frame(
            &ctx,
            task,
            &replacement,
            pointer(pos, false),
            None,
            true,
            None,
        );
        assert_eq!(after.navigation_left, None);
        assert_eq!(after.action, None);
        assert!((after.offset - 1300.0).abs() < 1.0);
    }
}

#[cfg(test)]
mod hidden_and_attention_tests {
    use super::*;
    use crate::agent::{NewTask, TaskError, TaskManager};
    use crate::sidebar::SidebarView;

    fn add_task(manager: &mut TaskManager, title: &str) -> TaskId {
        manager
            .create(NewTask {
                title: title.into(),
                provider: AgentProvider::Codex,
                repo_root: "/fixture".into(),
                worktree_path: format!("/fixture/worktrees/{title}").into(),
                branch: format!("task/{title}"),
                base_commit: "a".repeat(40),
                source_context: None,
            })
            .unwrap()
    }

    fn row(manager: &TaskManager, id: TaskId) -> TaskRowSnapshot {
        let task = manager.get(id).unwrap();
        TaskRowSnapshot {
            id,
            title: task.title.clone(),
            provider: task.provider,
            status: task.status,
            runtime_kind: task.runtime_kind,
            branch: task.branch.clone(),
            project: task.repo_root.to_string_lossy().into_owned(),
            has_agent_terminal: false,
            has_validation_terminal: false,
            has_active_agent_stream: false,
            native_preparing: false,
            terminal_retry_available: false,
            native_terminal_fallback_available: false,
            validation_status: task.validation.status,
            validation_attempt: task.validation.attempt,
            validation_detail: None,
            needs_attention: task.needs_attention(),
            status_detail: None,
        }
    }

    #[test]
    fn hidden_tasks_leave_the_dashboard_and_restore_to_their_recorded_state() {
        let mut manager = TaskManager::new();
        let visible = add_task(&mut manager, "visible");
        let hidden = add_task(&mut manager, "reviewed");
        manager
            .update_status(hidden, TaskStatus::ReadyForReview, None)
            .unwrap();
        manager.archive(hidden).unwrap();
        assert_eq!(manager.archived_count(), 1);

        let dashboard = DashboardSnapshot::project_unsorted(
            manager
                .tasks()
                .iter()
                .filter(|task| task.status != TaskStatus::Archived),
            |_| Default::default(),
            jterm_core::agent_task::DashboardFilter::default(),
        );
        assert_eq!(
            dashboard.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            [visible],
            "the normal dashboard keeps excluding hidden tasks"
        );
        let rows = hidden_task_rows(&manager);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, hidden);
        assert_eq!(rows[0].restores_to, Some(TaskStatus::ReadyForReview));
        assert_eq!(rows[0].title, "reviewed");

        // Restore and Hide never wait for recovery and never count as a
        // native action; Restore re-resolves the task when it executes.
        let action = TaskSidebarAction::Restore(hidden);
        assert!(!task_action_waits_for_recovery(
            &action,
            &HashSet::from([hidden])
        ));
        assert!(!native_action_needs_prompt_poll(&action));
        assert_eq!(
            restore_hidden_task(&mut manager, hidden).unwrap(),
            "Task restored · Ready for review"
        );
        let restored = manager.get(hidden).unwrap();
        assert_eq!(restored.status, TaskStatus::ReadyForReview);
        assert_eq!(restored.runtime_kind, TaskRuntimeKind::Unassigned);
        assert_eq!(restored.validation.status, TaskValidationStatus::NotRun);
        assert_eq!(manager.archived_count(), 0);
        assert!(hidden_task_rows(&manager).is_empty());
        assert!(restore_hidden_task(&mut manager, hidden)
            .unwrap_err()
            .contains("not hidden"));
        assert!(restore_hidden_task(&mut manager, TaskId::new())
            .unwrap_err()
            .starts_with("Could not restore the task"));
        assert_eq!(manager.get(visible).unwrap().status, TaskStatus::Created);
    }

    #[test]
    fn restore_keeps_start_blocked_until_registration_is_confirmed() {
        let task = TaskId::new();
        let other = TaskId::new();
        let mut unconfirmed = HashSet::from([task, other]);
        let mut hidden = HashSet::new();
        // Hiding sets the block aside, so the recovery record can be dismissed.
        assert!(carry_unconfirmed_registration(
            &mut unconfirmed,
            &mut hidden,
            task
        ));
        assert_eq!(unconfirmed, HashSet::from([other]));
        assert!(!task_action_waits_for_recovery(
            &TaskSidebarAction::StartNative(task),
            &unconfirmed
        ));
        // Restoring reinstates it: Start waits for confirmation again.
        assert!(carry_unconfirmed_registration(
            &mut hidden,
            &mut unconfirmed,
            task
        ));
        assert!(hidden.is_empty());
        for action in [
            TaskSidebarAction::StartNative(task),
            TaskSidebarAction::StartTerminal(task),
            TaskSidebarAction::RunValidation(task),
        ] {
            assert!(task_action_waits_for_recovery(&action, &unconfirmed));
        }
        // A confirmed task carries nothing.
        let confirmed = TaskId::new();
        assert!(!carry_unconfirmed_registration(
            &mut unconfirmed,
            &mut hidden,
            confirmed
        ));
        assert!(!carry_unconfirmed_registration(
            &mut hidden,
            &mut unconfirmed,
            confirmed
        ));
        assert!(!unconfirmed.contains(&confirmed));
    }

    fn record_for(manager: &TaskManager, id: TaskId) -> crate::agent::recovery::Record {
        let task = manager.get(id).unwrap();
        crate::agent::recovery::Record {
            id: uuid::Uuid::new_v4(),
            repository: task.repo_root.clone(),
            path: task.worktree_path.clone(),
            branch: task.branch.clone(),
            provider: task.provider,
            stage: crate::agent::recovery::Stage::Prepared,
            base_commit: Some(task.base_commit.clone()),
            directory_identity: None,
            registered_task_id: None,
        }
    }

    #[test]
    fn restore_after_the_record_was_dismissed_says_start_can_never_be_confirmed() {
        let mut manager = TaskManager::new();
        let id = add_task(&mut manager, "unconfirmed");
        let record = record_for(&manager, id);
        let mut records = vec![record.clone()];
        let hidden = HashSet::from([id]);
        // While hidden, dismissing its record is allowed but warned about.
        assert_eq!(
            hidden_task_waiting_on_record(&manager, &hidden, &record).map(|task| task.id),
            Some(id)
        );
        assert!(hidden_task_waiting_on_record(&manager, &HashSet::new(), &record).is_none());
        assert!(registration_confirmable(manager.get(id).unwrap(), &records));
        assert!(restore_status("Task restored · Created".into(), true, true)
            .contains("Start waits for Retry registration or Reload"));
        // After the record is dismissed, nothing can confirm it any more.
        records.clear();
        assert!(!registration_confirmable(
            manager.get(id).unwrap(),
            &records
        ));
        let status = restore_status("Task restored · Created".into(), true, false);
        assert!(status.starts_with("Task restored · Created; its recovery record was dismissed"));
        assert!(!status.contains("Retry registration or Reload"));
        assert_eq!(
            restore_status("Task restored · Ready for review".into(), false, false),
            "Task restored · Ready for review"
        );
        // A record for another worktree confirms nothing.
        let mut other = record;
        other.path = "/fixture/other-worktree".into();
        assert!(!registration_confirmable(
            manager.get(id).unwrap(),
            &[other]
        ));
    }

    #[test]
    fn the_hidden_list_gives_way_to_the_dashboard() {
        let mut state = TaskSidebarState {
            show_hidden: true,
            ..Default::default()
        };
        assert!(hidden_view_shown(state.show_hidden, 2));
        assert!(
            !hidden_view_shown(state.show_hidden, 0),
            "nothing left to restore"
        );
        assert!(!hidden_view_shown(false, 2));
        // Opening the dashboard, creating, registering or restoring a task.
        state.show_dashboard();
        assert!(!state.show_hidden);
    }

    #[test]
    fn hide_is_gated_by_the_manager_and_explains_what_must_finish() {
        let mut manager = TaskManager::new();
        let id = add_task(&mut manager, "gated");
        let mut snapshot = row(&manager, id);
        assert_eq!(
            hide_task_blocked_reason(&snapshot, manager.can_archive(id)),
            None
        );
        snapshot.native_preparing = true;
        assert!(hide_task_blocked_reason(&snapshot, manager.can_archive(id))
            .unwrap()
            .contains("preparation"));
        snapshot.native_preparing = false;
        manager
            .bind_terminal_session(id, "agent-terminal".into())
            .unwrap();
        let gate = manager.can_archive(id);
        assert_eq!(gate, Err(TaskError::CannotArchiveRunning(id)));
        assert!(hide_task_blocked_reason(&snapshot, gate)
            .unwrap()
            .contains("Stop or finish the Agent"));
        snapshot.validation_status = TaskValidationStatus::Running;
        assert!(
            hide_task_blocked_reason(&snapshot, Err(TaskError::CannotArchiveRunning(id)))
                .unwrap()
                .contains("validation")
        );
        let missing = TaskId::new();
        assert!(
            hide_task_blocked_reason(&snapshot, manager.can_archive(missing))
                .unwrap()
                .contains("no longer available")
        );
        assert_eq!(
            manager.archive(id),
            Err(TaskError::CannotArchiveRunning(id))
        );
    }

    #[test]
    fn attention_badge_counts_every_queue_and_explains_itself() {
        let mut manager = TaskManager::new();
        let reply = add_task(&mut manager, "reply");
        let failed = add_task(&mut manager, "failed");
        let hidden = add_task(&mut manager, "hidden");
        manager
            .update_status(reply, TaskStatus::WaitingForHuman, None)
            .unwrap();
        manager
            .update_status(failed, TaskStatus::Failed, None)
            .unwrap();
        manager
            .update_status(hidden, TaskStatus::ReadyForReview, None)
            .unwrap();
        manager.archive(hidden).unwrap();
        let counts = manager.attention_counts();
        assert_eq!(counts.total(), 2, "hidden tasks never ask for attention");
        assert_eq!(
            tasks_tab_label(counts),
            (
                "Tasks 2".to_string(),
                Some("2 tasks need you: 1 reply, 1 failed".to_string())
            )
        );
        assert_eq!(
            tasks_tab_label(Default::default()),
            ("Tasks".to_string(), None)
        );
        assert!(task_attention_indicator(false, false, counts).is_none());
        assert!(
            task_attention_indicator(true, true, counts).is_none(),
            "the sidebar tab already shows the count"
        );
        assert!(task_attention_indicator(true, false, Default::default()).is_none());
        let (label, hover) = task_attention_indicator(true, false, counts).unwrap();
        assert_eq!(label, "Tasks 2");
        assert!(hover.starts_with("2 tasks need you: 1 reply, 1 failed"));
        assert!(hover.contains("Open the Tasks dashboard"));
    }

    #[test]
    fn task_toggle_opens_the_dashboard_and_returns_to_what_it_replaced() {
        let (opened, previous) = tasks_toggle_target((true, SidebarView::Files), None);
        assert_eq!(opened, (true, SidebarView::Tasks));
        assert_eq!(previous, Some((true, SidebarView::Files)));
        assert_eq!(
            tasks_toggle_target(opened, previous),
            ((true, SidebarView::Files), None)
        );
        let (opened, previous) = tasks_toggle_target((false, SidebarView::Sessions), None);
        assert_eq!(opened, (true, SidebarView::Tasks));
        assert_eq!(
            tasks_toggle_target(opened, previous),
            ((false, SidebarView::Sessions), None)
        );
        assert_eq!(
            tasks_toggle_target((true, SidebarView::Tasks), None),
            ((false, SidebarView::Tasks), None),
            "without a record, closing hides the sidebar"
        );
        assert_eq!(
            tasks_toggle_target(
                (false, SidebarView::Tasks),
                Some((true, SidebarView::Files))
            )
            .0,
            (true, SidebarView::Tasks),
            "a hidden Tasks view opens again"
        );
    }

    #[test]
    fn user_native_actions_poll_promptly_while_navigation_does_not() {
        let task = TaskId::new();
        let approval = ApprovalId::new();
        for action in [
            TaskSidebarAction::StartNative(task),
            TaskSidebarAction::StopCodex(task),
            TaskSidebarAction::FollowUp(task, "next".into()),
            TaskSidebarAction::FinishCodex(task),
            TaskSidebarAction::Approve(task, approval),
            TaskSidebarAction::Deny(task, approval),
        ] {
            assert!(native_action_needs_prompt_poll(&action), "{action:?}");
        }
        for action in [
            TaskSidebarAction::ReviewDiff(task),
            TaskSidebarAction::Archive(task),
            TaskSidebarAction::Restore(task),
            TaskSidebarAction::FocusTerminal(task),
        ] {
            assert!(!native_action_needs_prompt_poll(&action), "{action:?}");
        }
        assert!(NATIVE_ACTION_FAST_POLL >= Duration::from_secs(1));
    }

    #[test]
    fn query_error_cache_matches_the_shared_parser_across_edits() {
        let mut cache = DashboardQueryErrorCache::default();
        for text in [
            "",
            "provider:codex",
            "status:nonsense",
            "status:nonsense",
            "\"unclosed",
            "provider:codex attention:review",
            "",
        ] {
            assert_eq!(
                cache.error(text).map(str::to_owned),
                jterm_core::agent_task::dashboard_query_error(text),
                "{text:?}"
            );
        }
    }

    #[test]
    fn restore_release_cannot_retarget_another_hidden_task_or_unknown_state() {
        let hidden = |restores_to| HiddenTaskRow {
            id: TaskId::new(),
            title: "hidden".into(),
            provider: AgentProvider::Codex,
            project: "/fixture".into(),
            branch: "task/hidden".into(),
            restores_to,
            updated_at_ms: 1,
        };
        let first = hidden(Some(TaskStatus::ReadyForReview));
        let mut changed_state = first.clone();
        changed_state.restores_to = Some(TaskStatus::Failed);
        for replacement in [hidden(Some(TaskStatus::ReadyForReview)), changed_state] {
            let ctx = egui::Context::default();
            let frame = |row: &HiddenTaskRow, events| {
                let mut response = None;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |ui| response = Some(hidden_task_restore_button(ui, row)),
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
            assert!(!frame(&replacement, pointer(false)).clicked());
            assert!(!frame(&replacement, pointer(true)).clicked());
            assert!(frame(&replacement, pointer(false)).clicked());
        }
        let ctx = egui::Context::default();
        let mut enabled = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            enabled = Some(hidden_task_restore_button(ui, &hidden(None)).enabled());
        });
        output.textures_delta.clear();
        assert_eq!(
            enabled,
            Some(false),
            "an unknown prior state is never guessed"
        );
    }
}
