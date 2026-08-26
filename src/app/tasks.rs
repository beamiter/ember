//! Experimental Tasks dashboard.
//!
//! Rendering takes owned row snapshots and only stages [`TaskId`] actions.
//! The action executor resolves the task and stable session ID again after the
//! egui closure, so a concurrent PTY exit or tab removal cannot redirect an
//! action to an unrelated index.

use crate::agent::{
    AgentProvider, AgentSessionOutcome, ApprovalDecision, ApprovalId, CodexAppServerApprovalKind,
    CodexAppServerPhase, CodexAppServerTurnHistory, CodexAppServerViewSnapshot, NativePromptPolicy,
    TaskId, TaskRuntimeKind, TaskStatus, TaskValidationStatus, CODEX_APP_SERVER_LIVE_TURN_MAX,
    NATIVE_AGENT_FOLLOW_UP_MAX_BYTES,
};
use crate::app::state::TerminalApp;
use crate::review_text::{sanitize_prompt_payload, visible_bounded, VisualSpoofDisposition};
use eframe::egui;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskSidebarAction {
    StartCodex(TaskId),
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
    ReviewDiff(TaskId),
    Archive(TaskId),
}

#[derive(Default)]
pub struct TaskSidebarState {
    pub selected: Option<TaskId>,
    pub pending_action: Option<TaskSidebarAction>,
    pending_creation: Option<PendingTaskCreation>,
    follow_up_drafts: HashMap<TaskId, String>,
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
    runtime_kind: TaskRuntimeKind,
    branch: String,
    updated_at_ms: u64,
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

fn render_native_codex_view(
    ui: &mut egui::Ui,
    task_id: TaskId,
    view: &CodexAppServerViewSnapshot,
    approvals_enabled: bool,
    pending: &mut Option<TaskSidebarAction>,
) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Native Codex").small().strong());
            ui.label(
                egui::RichText::new(format!("{:?}", view.phase))
                    .small()
                    .weak(),
            );
            if view.dropped_updates > 0 {
                ui.label(
                    egui::RichText::new(format!("· {} updates compacted", view.dropped_updates))
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

        let history = visible_native_turn_history(view);
        if !history.is_empty() || view.dropped_turns > 0 {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("Completed turns ({})", history.len()))
                        .small()
                        .strong(),
                );
                if view.dropped_turns > 0 {
                    ui.label(
                        egui::RichText::new(format!(
                            "· {} earlier turn(s) compacted",
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

        for approval in &view.pending_approvals {
            ui.separator();
            let kind = match approval.kind {
                CodexAppServerApprovalKind::Command => "Command approval",
                CodexAppServerApprovalKind::FileChange => "File-change approval",
            };
            ui.label(
                egui::RichText::new(kind)
                    .small()
                    .strong()
                    .color(ui.visuals().warn_fg_color),
            );
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
                if ui
                    .add_enabled(
                        native_approval_can_allow(approval, approvals_enabled),
                        egui::Button::new("Allow once"),
                    )
                    .on_hover_text("Approve only the exact patch displayed above")
                    .on_disabled_hover_text(
                        "Native approvals are display-and-deny only in this live session",
                    )
                    .clicked()
                {
                    *pending = Some(TaskSidebarAction::Approve(task_id, approval.id));
                }
                if ui
                    .add_enabled(approvals_enabled, egui::Button::new("Deny"))
                    .clicked()
                {
                    *pending = Some(TaskSidebarAction::Deny(task_id, approval.id));
                }
            });
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
                    "{} update(s) compacted in this turn",
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
                                    "Created an isolated {provider_name} task; choose Start Codex"
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
                runtime_kind: task.runtime_kind,
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
        let native_ai_enabled = self.config.ai_enabled && self.config.ai_share_command_context;
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
                    let color = if row.native_preparing {
                        egui::Color32::from_rgb(90, 150, 230)
                    } else if row.validation_status == TaskValidationStatus::NotRun {
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
                        let activity = if row.native_preparing {
                            format!("{} · Preparing…", row.provider.display_name())
                        } else if row.validation_attempt == 0 {
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
                        if row.native_preparing {
                            ui.spinner();
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        ui.add(
                            egui::Label::new(egui::RichText::new(&row.branch).small().weak())
                                .truncate(),
                        );
                    });

                    if selected {
                        let native_view = self.agent_runtime.snapshot(row.id);
                        let native_idle = row.status == TaskStatus::ReadyForReview
                            && row.has_active_agent_stream
                            && native_view
                                .as_ref()
                                .is_some_and(|view| view.phase == CodexAppServerPhase::Ready);
                        ui.horizontal_wrapped(|ui| {
                            if row.status == TaskStatus::Created
                                && row.runtime_kind == TaskRuntimeKind::Unassigned
                                && !row.native_preparing
                            {
                                if ui
                                    .add_enabled(
                                        native_ai_enabled,
                                        egui::Button::new("Start Codex"),
                                    )
                                    .on_disabled_hover_text(
                                        "Enable AI features and cloud command-context sharing in Settings → AI first",
                                    )
                                    .on_hover_text(
                                        "Start a native Codex app-server session. Review points can continue on the same loaded thread; finish the session before validation. Agent tool writes are restricted to this worktree, while the current Codex sandbox may read other host files.",
                                    )
                                    .clicked()
                                {
                                    pending = Some(TaskSidebarAction::StartCodex(row.id));
                                }
                                if ui
                                    .button("Terminal fallback")
                                    .on_hover_text(
                                        "Open the provider CLI in a PTY without Ember-native events or approval cards; the provider TUI owns its prompts",
                                    )
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
                                        "Cancel preparation"
                                    } else {
                                        "Stop Codex"
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
                                        if native_idle {
                                            "Finish Codex to end the native session and unlock validation"
                                        } else {
                                            "Wait for the native Agent turn to reach review, then finish the session"
                                        }
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
                        if let Some(view) = native_view.as_ref() {
                            render_native_codex_view(
                                ui,
                                row.id,
                                view,
                                self.agent_runtime.has_running(row.id)
                                    && row.has_active_agent_stream,
                                &mut pending,
                            );
                        }
                        if native_idle {
                            ui.group(|ui| {
                                ui.label(
                                    egui::RichText::new("Review feedback")
                                        .small()
                                        .strong(),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Send another turn on this loaded Codex thread, or finish the session to unlock validation.",
                                    )
                                    .small()
                                    .weak(),
                                );
                                let draft = self
                                    .task_sidebar
                                    .follow_up_drafts
                                    .entry(row.id)
                                    .or_default();
                                ui.add_enabled(
                                    native_ai_enabled,
                                    egui::TextEdit::multiline(draft)
                                        .desired_rows(3)
                                        .char_limit(MAX_NATIVE_FOLLOW_UP_CHARS)
                                        .hint_text("Describe what Codex should change next…"),
                                )
                                .on_disabled_hover_text(
                                    "Enable AI features and command-context sharing before sending another cloud turn",
                                );
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
                                    if ui
                                        .add_enabled(can_send, egui::Button::new("Send follow-up"))
                                        .on_disabled_hover_text(format!(
                                            "Feedback must be non-empty, at most {NATIVE_AGENT_FOLLOW_UP_MAX_BYTES} UTF-8 bytes, and sent before the {CODEX_APP_SERVER_LIVE_TURN_MAX}-turn session limit",
                                        ))
                                        .clicked()
                                    {
                                        pending = Some(TaskSidebarAction::FollowUp(
                                            row.id,
                                            draft.clone(),
                                        ));
                                    }
                                    if ui
                                        .add_enabled(can_finish, egui::Button::new("Finish Codex"))
                                        .on_hover_text(
                                            "End this idle native session; validation unlocks only after containment is empty and the provider is reaped",
                                        )
                                        .on_disabled_hover_text(
                                            "Send or clear the draft before finishing Codex",
                                        )
                                        .clicked()
                                    {
                                        pending = Some(TaskSidebarAction::FinishCodex(row.id));
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
            TaskSidebarAction::StartCodex(task_id) => self.start_task_native_codex(task_id),
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
                    .prompt_codex(&self.task_manager, task_id, &text, policy)
                {
                    Ok(()) => {
                        self.task_sidebar.follow_up_drafts.remove(&task_id);
                        self.set_status("Follow-up queued on the existing Codex thread…");
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
                    self.agent_runtime.clear_retained(task_id);
                    self.task_sidebar.follow_up_drafts.remove(&task_id);
                    if self.task_sidebar.selected == Some(task_id) {
                        self.task_sidebar.selected = None;
                    }
                    self.set_status("Task hidden; worktree left in place");
                }
                Err(error) => self.set_status_for(error.to_string(), Duration::from_secs(5)),
            },
        }
    }

    fn start_task_native_codex(&mut self, task_id: TaskId) {
        let policy = NativePromptPolicy {
            share_command_context: self.config.ai_enabled && self.config.ai_share_command_context,
            redact_secrets: self.config.ai_redact_secrets,
        };
        match self
            .agent_runtime
            .start_codex(&mut self.task_manager, task_id, policy)
        {
            Ok(()) => self.set_status("Preparing native Codex prerequisites in the background…"),
            Err(error) => self.set_status_for(
                format!("Could not start native Codex: {error}"),
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
        } else if let Some(completion) = report.completions.last() {
            let message = if report.completions.len() > 1 {
                format!(
                    "{} native Codex sessions stopped; open Tasks for individual results",
                    report.completions.len()
                )
            } else {
                match completion.outcome {
                    AgentSessionOutcome::Clean => {
                        "Native Codex stopped cleanly; review its diff, then run validation"
                            .to_string()
                    }
                    AgentSessionOutcome::Cancelled => {
                        "Native Codex was cancelled and fully stopped".to_string()
                    }
                    AgentSessionOutcome::Failed => format!(
                        "Native Codex failed: {}",
                        completion
                            .detail
                            .as_deref()
                            .unwrap_or("provider session did not complete")
                    ),
                }
            };
            self.set_status_for(message, Duration::from_secs(8));
        } else if report.preparations_started > 0 {
            self.set_status(if report.preparations_started == 1 {
                "Native Codex prerequisites verified; starting app-server…".to_string()
            } else {
                format!(
                    "{} native Codex sessions finished preparation and are starting…",
                    report.preparations_started
                )
            });
        }
        if self.agent_runtime.needs_fast_poll() || report.budget_exhausted {
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
        let launch = self.task_manager.get(task_id).and_then(|task| {
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
            runtime_kind: TaskRuntimeKind::Unassigned,
            branch: "ember/task".to_string(),
            updated_at_ms,
            has_agent_terminal: true,
            has_validation_terminal: false,
            has_active_agent_stream: false,
            native_preparing: false,
            terminal_retry_available: false,
            native_terminal_fallback_available: false,
            validation_status: TaskValidationStatus::NotRun,
            validation_attempt: 0,
            validation_detail: None,
            needs_attention: status.needs_attention(),
            status_detail: None,
        }
    }

    #[test]
    fn dashboard_orders_attention_then_running_then_finished() {
        let mut preparing = row(TaskStatus::Created, 30, "preparing");
        preparing.native_preparing = true;
        assert!(preparing.is_running());
        let mut rows = vec![
            row(TaskStatus::Completed, 50, "done"),
            preparing,
            row(TaskStatus::Working, 10, "working"),
            row(TaskStatus::Failed, 1, "failed-old"),
            row(TaskStatus::WaitingForHuman, 20, "waiting-new"),
        ];
        sort_rows(&mut rows);
        let titles: Vec<_> = rows.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["waiting-new", "failed-old", "preparing", "working", "done"]
        );
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
