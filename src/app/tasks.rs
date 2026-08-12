//! Experimental Tasks dashboard.
//!
//! Rendering takes owned row snapshots and only stages [`TaskId`] actions.
//! The action executor resolves the task and stable session ID again after the
//! egui closure, so a concurrent PTY exit or tab removal cannot redirect an
//! action to an unrelated index.

use crate::agent::{AgentProvider, TaskId, TaskStatus, TaskValidationStatus};
use crate::app::state::TerminalApp;
use crate::review_text::visible_bounded;
use eframe::egui;
use std::cmp::Reverse;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

const MAX_TASK_TITLE_DISPLAY_BYTES: usize = 160;
const MAX_TASK_BRANCH_DISPLAY_BYTES: usize = 120;
const MAX_TASK_DETAIL_DISPLAY_BYTES: usize = 320;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskSidebarAction {
    StartAgent(TaskId),
    FocusTerminal(TaskId),
    FocusValidation(TaskId),
    RunValidation(TaskId),
    Complete(TaskId),
    ReviewDiff(TaskId),
    Archive(TaskId),
}

#[derive(Default)]
pub struct TaskSidebarState {
    pub selected: Option<TaskId>,
    pub pending_action: Option<TaskSidebarAction>,
    pending_creation: Option<PendingTaskCreation>,
}

struct PendingTaskCreation {
    receiver: Receiver<Result<PreparedTask, String>>,
    worker: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
}

struct PreparedTask {
    context: crate::agent::SemanticCommandContext,
    title: String,
    provider: AgentProvider,
    worktree: crate::agent::ManagedWorktree,
}

impl Drop for PendingTaskCreation {
    fn drop(&mut self) {
        // Cancellation is polled by the service's nonblocking pipe supervisor
        // and between Git commands. Joining therefore proves no mutating child
        // remains while adding only one short poll interval to shutdown.
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
    branch: String,
    updated_at_ms: u64,
    has_agent_terminal: bool,
    has_validation_terminal: bool,
    has_active_agent_stream: bool,
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
    }

    fn group_rank(&self) -> u8 {
        if self.needs_attention {
            0
        } else if self.is_running() {
            1
        } else {
            2
        }
    }
}

fn sort_rows(rows: &mut [TaskRowSnapshot]) {
    rows.sort_by_key(|row| {
        (
            row.group_rank(),
            Reverse(row.updated_at_ms),
            row.id.to_string(),
        )
    });
}

impl TerminalApp {
    pub(crate) fn begin_command_worktree_task(
        &mut self,
        context: crate::agent::SemanticCommandContext,
        provider: AgentProvider,
    ) -> Result<(), String> {
        if self.task_sidebar.pending_creation.is_some() {
            return Err("another task worktree is still being created".to_string());
        }
        let worktree_root = dirs::data_local_dir()
            .ok_or_else(|| "cannot locate the per-user data directory".to_string())?
            .join("ember")
            .join("agent-tasks");
        let cwd = context
            .cwd
            .as_deref()
            .filter(|cwd| !cwd.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "source command has no working directory".to_string())?;
        let command = context.command.as_deref().unwrap_or("failed command");
        let title = format!("Fix {}", visible_bounded(command, 112));
        let token = uuid::Uuid::new_v4().simple().to_string();
        let task_name = format!("task-{token}");
        let branch = format!("ember/{task_name}");
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker = std::thread::Builder::new()
            .name("ember-task-worktree".to_string())
            .spawn(move || {
                let result = (|| {
                    let service = crate::agent::WorktreeService::new(worktree_root)
                        .map_err(|error| error.to_string())?
                        .with_cancel_flag(worker_cancel);
                    let repository = service
                        .resolve_repository_root(&cwd)
                        .map_err(|error| error.to_string())?;
                    let request = crate::agent::CreateWorktreeRequest::new(
                        repository, task_name, branch, "HEAD",
                    );
                    let worktree = service
                        .create(&request)
                        .map_err(|error| error.to_string())?;
                    Ok(PreparedTask {
                        context,
                        title,
                        provider,
                        worktree,
                    })
                })();
                let _ = sender.send(result);
            })
            .map_err(|error| format!("could not start task worktree worker: {error}"))?;

        self.task_sidebar.pending_creation = Some(PendingTaskCreation {
            receiver,
            worker: Some(worker),
            cancel,
        });
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
                self.set_status_for(
                    "Task worktree worker stopped unexpectedly",
                    Duration::from_secs(6),
                );
            }
            Some(Ok(result)) => {
                // The result is sent only after the mutating operation has
                // completed; dropping its finished handle is nonblocking.
                self.task_sidebar.pending_creation = None;
                match result {
                    Err(error) => self.set_status_for(
                        format!("Could not create task worktree: {error}"),
                        Duration::from_secs(8),
                    ),
                    Ok(prepared) => {
                        let provider_name = prepared.provider.display_name();
                        let worktree = prepared.worktree;
                        let task = crate::agent::NewTask {
                            title: prepared.title,
                            provider: prepared.provider,
                            repo_root: worktree.repository,
                            worktree_path: worktree.path,
                            branch: worktree.branch,
                            base_commit: worktree.head,
                            source_context: Some(prepared.context),
                        };
                        match self.task_manager.create(task) {
                            Ok(task_id) => {
                                self.task_sidebar.selected = Some(task_id);
                                self.set_status(format!(
                                    "Created an isolated {provider_name} task; choose Open agent"
                                ));
                            }
                            Err(error) => self.set_status_for(
                                format!(
                                    "Worktree was preserved, but task registration failed: {error}"
                                ),
                                Duration::from_secs(8),
                            ),
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn render_sidebar_tasks(&mut self, ui: &mut egui::Ui) {
        let mut rows: Vec<_> = self
            .task_manager
            .tasks()
            .iter()
            .filter(|task| task.status != TaskStatus::Archived)
            .map(|task| TaskRowSnapshot {
                id: task.id,
                title: visible_bounded(&task.title, MAX_TASK_TITLE_DISPLAY_BYTES),
                provider: task.provider,
                status: task.status,
                branch: visible_bounded(&task.branch, MAX_TASK_BRANCH_DISPLAY_BYTES),
                updated_at_ms: task.updated_at_ms,
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
                validation_status: task.validation.status,
                validation_attempt: task.validation.attempt,
                validation_detail: task
                    .validation
                    .status_detail
                    .as_deref()
                    .map(|detail| visible_bounded(detail, MAX_TASK_DETAIL_DISPLAY_BYTES)),
                needs_attention: task.needs_attention()
                    && !self.task_manager.has_active_agent_event_stream(task.id),
                status_detail: task
                    .status_detail
                    .as_deref()
                    .map(|detail| visible_bounded(detail, MAX_TASK_DETAIL_DISPLAY_BYTES)),
            })
            .collect();
        sort_rows(&mut rows);

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
            ui.label(egui::RichText::new("No Agent tasks yet").strong());
            ui.label(
                egui::RichText::new(
                    "Create one from a failed command block. Each task gets its own Git worktree and Agent terminal.",
                )
                .small()
                .weak(),
            );
            return;
        }

        let mut pending = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for row in &rows {
                    let selected = self.task_sidebar.selected == Some(row.id);
                    let attention = row.needs_attention;
                    let marker = if attention {
                        "!"
                    } else if row.is_running() {
                        "●"
                    } else if row.status == TaskStatus::Completed
                        || row.validation_status == TaskValidationStatus::Passed
                    {
                        "✓"
                    } else {
                        "·"
                    };
                    let color = if row.validation_status == TaskValidationStatus::NotRun {
                        task_status_color(ui, row.status)
                    } else {
                        task_validation_color(ui, row.validation_status)
                    };
                    let response = ui
                        .horizontal(|ui| {
                            ui.colored_label(color, marker);
                            ui.add(
                                egui::Label::new(egui::RichText::new(&row.title).strong())
                                    .truncate(),
                            );
                        })
                        .response
                        .interact(egui::Sense::click());
                    if response.clicked() {
                        self.task_sidebar.selected = Some(row.id);
                    }

                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        let activity = if row.validation_attempt == 0 {
                            format!("{} · {}", row.provider.display_name(), row.status.label())
                        } else {
                            format!(
                                "{} · Validation #{} {}",
                                row.provider.display_name(),
                                row.validation_attempt,
                                row.validation_status.label().to_lowercase()
                            )
                        };
                        ui.label(egui::RichText::new(activity).small().color(color));
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        ui.add(
                            egui::Label::new(egui::RichText::new(&row.branch).small().weak())
                                .truncate(),
                        );
                    });

                    if selected {
                        ui.horizontal_wrapped(|ui| {
                            if row.status == TaskStatus::Created
                                && ui.button("Open agent").clicked()
                            {
                                pending = Some(TaskSidebarAction::StartAgent(row.id));
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
                            if row.status == TaskStatus::ReadyForReview
                                && ui
                                    .add_enabled(
                                        row.validation_status != TaskValidationStatus::Running
                                            && !row.has_active_agent_stream,
                                        egui::Button::new(
                                            if row.validation_attempt == 0 {
                                                "Run validation"
                                            } else {
                                                "Run again"
                                            },
                                        ),
                                    )
                                    .on_disabled_hover_text(if row.has_active_agent_stream {
                                        "Wait for the native Agent session to end"
                                    } else {
                                        "Validation is already running"
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
                            if row.status == TaskStatus::ReadyForReview
                                && row.validation_status == TaskValidationStatus::Passed
                                && ui
                                    .button("Mark complete")
                                    .on_hover_text(
                                        "Accept the reviewed task after its latest validation passed",
                                    )
                                    .clicked()
                            {
                                pending = Some(TaskSidebarAction::Complete(row.id));
                            }
                            if ui
                                .add_enabled(
                                    !row.is_running(),
                                    egui::Button::new("Hide task"),
                                )
                                .on_hover_text("Hide task metadata; leave its worktree in place")
                                .clicked()
                            {
                                pending = Some(TaskSidebarAction::Archive(row.id));
                            }
                        });
                        if let Some(detail) = &row.status_detail {
                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                ui.label(egui::RichText::new(detail).small().weak());
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
                    }
                    ui.separator();
                }
            });
        if pending.is_some() {
            self.task_sidebar.pending_action = pending;
        }
    }

    pub(crate) fn execute_pending_task_sidebar_action(&mut self) {
        let Some(action) = self.task_sidebar.pending_action.take() else {
            return;
        };
        match action {
            TaskSidebarAction::StartAgent(task_id) => self.start_task_agent_terminal(task_id),
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
                    if self.task_sidebar.selected == Some(task_id) {
                        self.task_sidebar.selected = None;
                    }
                    self.set_status("Task hidden; worktree left in place");
                }
                Err(error) => self.set_status_for(error.to_string(), Duration::from_secs(5)),
            },
        }
    }

    fn start_task_agent_terminal(&mut self, task_id: TaskId) {
        let launch = self.task_manager.get(task_id).and_then(|task| {
            (task.status == TaskStatus::Created && task.terminal_session_id.is_none()).then(|| {
                (
                    task.provider,
                    task.title.clone(),
                    task.repo_root.clone(),
                    task.worktree_path.clone(),
                )
            })
        });
        let Some((provider, title, repository, worktree)) = launch else {
            self.set_status("Task is no longer waiting for an Agent terminal");
            return;
        };
        let launch = match crate::agent::AgentLaunchSpec::resolve(provider, &repository, &worktree)
        {
            Ok(launch) => launch,
            Err(error) => {
                let _ = self.task_manager.update_status(
                    task_id,
                    TaskStatus::Created,
                    Some(error.to_string()),
                );
                self.set_status_for(error.to_string(), Duration::from_secs(6));
                return;
            }
        };
        let _ = self
            .task_manager
            .update_status(task_id, TaskStatus::Starting, None);

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
                let _ = self.task_manager.update_status(
                    task_id,
                    TaskStatus::Created,
                    Some(error.clone()),
                );
                self.set_status_for(
                    format!("Could not start {}: {error}", provider.display_name()),
                    Duration::from_secs(6),
                );
                return;
            }
        };

        if let Err(error) = self
            .task_manager
            .bind_terminal_session(task_id, created.session_id.clone())
        {
            // The session was inserted but has not entered TabManager yet;
            // removing it immediately restores the original index layout.
            let _ = self.session_manager.close_session(created.session_index);
            let _ = self.task_manager.update_status(
                task_id,
                TaskStatus::Failed,
                Some(error.to_string()),
            );
            self.set_status_for(error.to_string(), Duration::from_secs(6));
            return;
        }

        self.tabs.on_session_inserted(created.session_index);
        self.tabs.insert_tab_after_active(created.session_index);
        self.activate_session(created.session_index);
        self.schedule_session_save();
        self.set_status(format!(
            "Opened {} in an isolated task terminal; task context remains in Ember",
            provider.display_name()
        ));
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
mod tests {
    use super::*;

    fn row(status: TaskStatus, updated_at_ms: u64, title: &str) -> TaskRowSnapshot {
        TaskRowSnapshot {
            id: TaskId::new(),
            title: title.to_string(),
            provider: AgentProvider::Codex,
            status,
            branch: "ember/task".to_string(),
            updated_at_ms,
            has_agent_terminal: true,
            has_validation_terminal: false,
            has_active_agent_stream: false,
            validation_status: TaskValidationStatus::NotRun,
            validation_attempt: 0,
            validation_detail: None,
            needs_attention: status.needs_attention(),
            status_detail: None,
        }
    }

    #[test]
    fn dashboard_orders_attention_then_running_then_finished() {
        let mut rows = vec![
            row(TaskStatus::Completed, 50, "done"),
            row(TaskStatus::Working, 10, "working"),
            row(TaskStatus::Failed, 1, "failed-old"),
            row(TaskStatus::WaitingForHuman, 20, "waiting-new"),
        ];
        sort_rows(&mut rows);
        let titles: Vec<_> = rows.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(titles, vec!["waiting-new", "failed-old", "working", "done"]);
    }

    #[test]
    fn display_boundary_neutralizes_bidi_and_bounds_text() {
        let rendered = visible_bounded("safe\u{202e}spoof", 32);
        assert_eq!(rendered, "safe\\u{202E}spoof");
        assert!(visible_bounded(&"x".repeat(500), 20).len() <= 20);
    }
}
