//! Experimental Tasks dashboard.
//!
//! Rendering takes owned row snapshots and only stages [`TaskId`] actions.
//! The action executor resolves the task and stable session ID again after the
//! egui closure, so a concurrent PTY exit or tab removal cannot redirect an
//! action to an unrelated index.

use crate::agent::{AgentProvider, TaskId, TaskStatus};
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
    has_terminal: bool,
    status_detail: Option<String>,
}

impl TaskRowSnapshot {
    fn group_rank(&self) -> u8 {
        if self.status.needs_attention() {
            0
        } else if self.status.is_running() {
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

pub fn attention_count(tasks: &[crate::agent::AgentTask]) -> usize {
    tasks
        .iter()
        .filter(|task| task.status != TaskStatus::Archived && task.needs_attention())
        .count()
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
        let mut rows: Vec<_> =
            self.task_manager
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
                    has_terminal: task.agent_session_id.as_deref().is_some_and(|session_id| {
                        self.session_manager.index_of(session_id).is_some()
                    }),
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
                    let attention = row.status.needs_attention();
                    let marker = if attention {
                        "!"
                    } else if row.status.is_running() {
                        "●"
                    } else if row.status == TaskStatus::Completed {
                        "✓"
                    } else {
                        "·"
                    };
                    let color = task_status_color(ui, row.status);
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
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {}",
                                row.provider.display_name(),
                                row.status.label()
                            ))
                            .small()
                            .color(color),
                        );
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
                                .add_enabled(row.has_terminal, egui::Button::new("Open terminal"))
                                .on_disabled_hover_text("Agent terminal is no longer available")
                                .clicked()
                            {
                                pending = Some(TaskSidebarAction::FocusTerminal(row.id));
                            }
                            if ui.button("Review diff").clicked() {
                                pending = Some(TaskSidebarAction::ReviewDiff(row.id));
                            }
                            if ui
                                .add_enabled(
                                    !row.status.is_running(),
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
                    .and_then(|task| task.agent_session_id.clone());
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
            (task.status == TaskStatus::Created && task.agent_session_id.is_none()).then(|| {
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
            .bind_agent_session(task_id, created.session_id.clone())
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
            has_terminal: true,
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
