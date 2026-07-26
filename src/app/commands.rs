//! Continuous-terminal command timeline UI.
//!
//! The terminal owns semantic execution records. This module snapshots only
//! the small fields needed to paint the sidebar, records an action while egui
//! closures are active, and performs terminal/clipboard/PTY work afterwards.

use super::state::TerminalApp;
use crate::execution_journal::{self, HistoryLoad, HistoryRequestError, PersistedExecution};
use crate::terminal::{CommandState, MAX_COMPLETED_COMMAND_OUTPUT_BYTES};
use eframe::egui;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

const COMMAND_DETAIL_COMMAND_BYTES: usize = 8 * 1024;
const COMMAND_DETAIL_OUTPUT_BYTES: usize = 16 * 1024;
const DETAIL_TRUNCATION_MARKER: &str = "\n… preview truncated …\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandTarget {
    pub session_id: String,
    pub execution_id: String,
}

#[derive(Debug, Default)]
pub struct CommandSidebarState {
    pub query: String,
    pub selected: Option<CommandTarget>,
    filter: CommandFilter,
    pending_action: Option<CommandAction>,
    history_session_id: Option<String>,
    history: Vec<PersistedExecution>,
    history_load: Option<HistoryLoad>,
    history_loaded: bool,
    history_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CommandFilter {
    #[default]
    All,
    Failed,
    Running,
}

#[derive(Clone, Debug)]
struct CommandRowSnapshot {
    target: CommandTarget,
    sequence: u64,
    command_summary: String,
    command_preview: String,
    command_exact: bool,
    command_multiline: bool,
    cwd: Option<String>,
    state: CommandState,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    started_at: Option<SystemTime>,
    output_copy_available: bool,
}

#[derive(Clone, Debug)]
struct DetailTextSnapshot {
    text: String,
    truncated: bool,
    total_bytes: usize,
}

#[derive(Clone, Debug)]
struct CommandDetailSnapshot {
    target: CommandTarget,
    command: Option<DetailTextSnapshot>,
    command_exact: bool,
    command_omitted: bool,
    output: Option<DetailTextSnapshot>,
    output_copy_available: bool,
    state: CommandState,
    command_from_history: bool,
    output_from_history: bool,
}

#[derive(Clone, Copy, Debug)]
struct ReplayGuardSnapshot {
    prompt_ready: bool,
    alternate_screen: bool,
    bracketed_paste: bool,
    pending_input: bool,
}

#[derive(Clone, Copy, Debug)]
enum CommandActionKind {
    Jump,
    CopyCommand,
    CopyOutput,
    CopyCombined,
    Fill,
    RunAgain,
}

#[derive(Clone, Debug)]
struct CommandAction {
    target: CommandTarget,
    kind: CommandActionKind,
}

#[derive(Debug)]
enum ReplayOutcome {
    Filled,
    Ran,
    NotPromptReady,
    AlternateScreen,
    BracketedPasteDisabled,
    PendingInput,
    EmptyCommand,
    MultilineRun,
    WriteFailed(crate::shell::ShellWriteError),
}

impl TerminalApp {
    /// Render commands for the currently focused tab in chronological order.
    pub(crate) fn render_sidebar_commands(&mut self, ui: &mut egui::Ui) {
        let active_index = self.session_manager.active_index();
        let selected_before = self.command_sidebar.selected.clone();
        let (session_id, session_title, mut rows, replay_guard, live_selected_detail) = {
            let Some(session) = self.session_manager.sessions().get(active_index) else {
                ui.label("No active session");
                return;
            };
            let session_id = session.metadata.session_id.clone();
            let session_title = Self::session_cwd_title(session);
            let pending_input = !session.pending_input.is_empty();
            let terminal = session.terminal.lock();
            let replay_guard = ReplayGuardSnapshot {
                prompt_ready: terminal.shell_is_prompt_ready(),
                alternate_screen: terminal.is_alt_buffer(),
                bracketed_paste: terminal.is_bracketed_paste_enabled(),
                pending_input,
            };
            let rows = terminal
                .command_records()
                .iter()
                .filter_map(|record| {
                    let command = record
                        .command
                        .as_deref()
                        .map(str::trim)
                        .filter(|command| !command.is_empty());
                    let display = command.or_else(|| {
                        record
                            .command_truncated
                            .then_some("(command omitted: exceeds integration limit)")
                    })?;
                    let output_copy_available = record
                        .captured_output
                        .as_ref()
                        .is_some_and(|output| !output.text.is_empty())
                        || record
                            .output_start
                            .zip(record.output_end)
                            .is_some_and(|(start, end)| end > start);
                    Some(CommandRowSnapshot {
                        target: CommandTarget {
                            session_id: session_id.clone(),
                            execution_id: record.id.clone(),
                        },
                        sequence: record.sequence,
                        command_summary: single_line_command_preview(display, 160),
                        command_preview: single_line_command_preview(display, 512),
                        command_exact: record.command_exact
                            && !record.command_truncated
                            && command.is_some(),
                        command_multiline: record
                            .command
                            .as_deref()
                            .is_some_and(replay_command_is_multiline),
                        cwd: record
                            .cwd
                            .as_deref()
                            .map(|cwd| single_line_command_preview(cwd, 256)),
                        state: record.state,
                        exit_code: record.exit_code,
                        duration_ms: record.duration_ms,
                        started_at: record.started_at,
                        output_copy_available,
                    })
                })
                .collect::<Vec<_>>();
            let selected_detail = selected_before
                .as_ref()
                .filter(|target| target.session_id == session_id)
                .and_then(|target| {
                    let record = terminal.command_record(&target.execution_id)?;
                    let command = record.command.as_deref().map(|command| {
                        detail_text_snapshot(
                            command,
                            false,
                            command.len(),
                            COMMAND_DETAIL_COMMAND_BYTES,
                        )
                    });
                    let output = record.captured_output.as_ref().map(|output| {
                        detail_text_snapshot(
                            &output.text,
                            output.truncated,
                            output.total_bytes,
                            COMMAND_DETAIL_OUTPUT_BYTES,
                        )
                    });
                    let output_copy_available = record
                        .captured_output
                        .as_ref()
                        .is_some_and(|output| !output.text.is_empty())
                        || record
                            .output_start
                            .zip(record.output_end)
                            .is_some_and(|(start, end)| end > start);
                    Some(CommandDetailSnapshot {
                        target: target.clone(),
                        command,
                        command_exact: record.command_exact && !record.command_truncated,
                        command_omitted: record.command_truncated && record.command.is_none(),
                        output,
                        output_copy_available,
                        state: record.state,
                        command_from_history: false,
                        output_from_history: false,
                    })
                });
            (
                session_id,
                session_title,
                rows,
                replay_guard,
                selected_detail,
            )
        };

        self.sync_command_sidebar_history(&session_id, ui.ctx());
        enrich_current_tab_rows_from_history(&mut rows, &self.command_sidebar.history);
        let selected_detail = live_selected_detail.map(|mut detail| {
            if let Some(record) = self
                .command_sidebar
                .history
                .iter()
                .find(|record| record.id == detail.target.execution_id)
            {
                enrich_live_detail_from_history(&mut detail, record);
            }
            detail
        });
        rows.sort_by_key(|row| row.sequence);

        ui.add(
            egui::Label::new(
                egui::RichText::new(session_title)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            )
            .truncate(),
        );
        ui.add_space(3.0);

        ui.horizontal(|ui| {
            let clear_width = if self.command_sidebar.query.is_empty() {
                0.0
            } else {
                24.0
            };
            let search_width = (ui.available_width() - clear_width).max(40.0);
            ui.add_sized(
                [search_width, 24.0],
                egui::TextEdit::singleline(&mut self.command_sidebar.query)
                    .hint_text("Search commands…"),
            );
            if clear_width > 0.0
                && ui
                    .add_sized([clear_width, 24.0], egui::Button::new("×"))
                    .on_hover_text("Clear search")
                    .clicked()
            {
                self.command_sidebar.query.clear();
            }
        });

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Show")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            for (filter, label) in [
                (CommandFilter::All, "All"),
                (CommandFilter::Failed, "Failed"),
                (CommandFilter::Running, "Running"),
            ] {
                if ui
                    .selectable_label(
                        self.command_sidebar.filter == filter,
                        egui::RichText::new(label).small(),
                    )
                    .clicked()
                {
                    self.command_sidebar.filter = filter;
                }
            }
        });

        let query = self.command_sidebar.query.trim().to_lowercase();
        let visible_rows = rows
            .iter()
            .filter(|row| command_row_matches(row, &query, self.command_sidebar.filter))
            .collect::<Vec<_>>();
        let count_label = if query.is_empty() {
            format!("{} commands", visible_rows.len())
        } else {
            format!("{} of {} commands", visible_rows.len(), rows.len())
        };
        ui.label(
            egui::RichText::new(count_label)
                .small()
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(2.0);

        let mut action = None;
        let mut clear_selection = false;
        if visible_rows.is_empty() {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(if rows.is_empty() {
                    "Commands will appear here after they run."
                } else {
                    "No matching commands."
                })
                .small()
                .color(ui.visuals().weak_text_color()),
            );
        } else {
            egui::ScrollArea::vertical()
                .id_salt(("command_timeline", &session_id))
                .auto_shrink([false, true])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for row in visible_rows {
                        let selected = self.command_sidebar.selected.as_ref() == Some(&row.target);
                        let fill = if selected {
                            ui.visuals().selection.bg_fill.gamma_multiply(0.55)
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        let frame = egui::Frame::NONE
                            .fill(fill)
                            .corner_radius(egui::CornerRadius::same(5))
                            .inner_margin(egui::Margin::symmetric(5, 4))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let (symbol, color, label) = command_status(row);
                                    ui.colored_label(color, symbol).on_hover_text(label);
                                    ui.vertical(|ui| {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&row.command_summary)
                                                    .monospace(),
                                            )
                                            .truncate()
                                            .selectable(false),
                                        );
                                        let metadata = command_metadata(row);
                                        if !metadata.is_empty() {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(metadata)
                                                        .small()
                                                        .color(ui.visuals().weak_text_color()),
                                                )
                                                .truncate()
                                                .selectable(false),
                                            );
                                        }
                                    });
                                });
                            });
                        let row_id = ui.make_persistent_id((
                            "command_timeline_row",
                            &row.target.session_id,
                            &row.target.execution_id,
                        ));
                        let response = ui
                            .interact(frame.response.rect, row_id, egui::Sense::click())
                            .on_hover_text(format!(
                                "{}\n\nClick to jump · Right-click for actions",
                                row.command_preview
                            ));
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if response.clicked() {
                            self.command_sidebar.selected = Some(row.target.clone());
                            action = Some(CommandAction {
                                target: row.target.clone(),
                                kind: CommandActionKind::Jump,
                            });
                        }
                        response.context_menu(|ui| {
                            command_menu_item(
                                ui,
                                &mut action,
                                row,
                                "Copy command",
                                CommandActionKind::CopyCommand,
                                (!row.command_exact)
                                    .then_some("The shell did not provide exact command metadata"),
                            );
                            command_menu_item(
                                ui,
                                &mut action,
                                row,
                                "Copy output",
                                CommandActionKind::CopyOutput,
                                (!row.output_copy_available)
                                    .then_some("Rendered command output is unavailable or empty"),
                            );
                            command_menu_item(
                                ui,
                                &mut action,
                                row,
                                "Copy command + output",
                                CommandActionKind::CopyCombined,
                                combined_copy_disabled_reason(row),
                            );
                            ui.separator();
                            command_menu_item(
                                ui,
                                &mut action,
                                row,
                                "Fill at prompt",
                                CommandActionKind::Fill,
                                replay_disabled_reason(row, replay_guard, false),
                            );
                            command_menu_item(
                                ui,
                                &mut action,
                                row,
                                "Run again",
                                CommandActionKind::RunAgain,
                                replay_disabled_reason(row, replay_guard, true),
                            );
                        });
                        if selected {
                            if let Some(detail) = selected_detail
                                .as_ref()
                                .filter(|detail| detail.target == row.target)
                            {
                                render_command_detail(
                                    ui,
                                    row,
                                    detail,
                                    replay_guard,
                                    &mut action,
                                    &mut clear_selection,
                                );
                            }
                        }
                        ui.add_space(2.0);
                    }
                });
        }

        let selected_missing = selected_before
            .as_ref()
            .is_some_and(|target| target.session_id == session_id)
            && selected_detail.is_none();
        if clear_selection || selected_missing {
            self.command_sidebar.selected = None;
        }

        // The containing Panel::show closure is still alive here. Stage the
        // action so main.rs can execute it after that outer closure returns.
        if action.is_some() {
            self.command_sidebar.pending_action = action;
        }
    }

    fn sync_command_sidebar_history(&mut self, session_id: &str, ctx: &egui::Context) {
        if self.command_sidebar.history_session_id.as_deref() != Some(session_id) {
            self.command_sidebar.history_session_id = Some(session_id.to_owned());
            self.command_sidebar.history.clear();
            self.command_sidebar.history_load = None;
            self.command_sidebar.history_loaded = false;
            self.command_sidebar.history_error = None;
        }

        let polled = self
            .command_sidebar
            .history_load
            .as_ref()
            .map(HistoryLoad::try_snapshot);
        match polled {
            Some(Ok(Some(snapshot))) => {
                self.command_sidebar.history_load = None;
                if snapshot.session_id == session_id {
                    self.command_sidebar.history = snapshot.records;
                    self.command_sidebar.history_error = snapshot.error;
                    self.command_sidebar.history_loaded = true;
                }
            }
            Some(Ok(None)) => {
                ctx.request_repaint_after(Duration::from_millis(75));
                return;
            }
            Some(Err(jterm_core::execution_journal::HistoryLoadDisconnected)) => {
                self.command_sidebar.history_load = None;
                self.command_sidebar.history_loaded = true;
                self.command_sidebar.history_error =
                    Some("background reader stopped unexpectedly".to_owned());
            }
            None => {}
        }

        if self.command_sidebar.history_loaded || self.command_sidebar.history_load.is_some() {
            return;
        }
        match execution_journal::request_history(session_id.to_owned()) {
            Ok(load) => {
                self.command_sidebar.history_load = Some(load);
                ctx.request_repaint_after(Duration::from_millis(75));
            }
            Err(HistoryRequestError::Full) => {
                ctx.request_repaint_after(Duration::from_millis(150));
            }
            Err(HistoryRequestError::Closed) => {
                self.command_sidebar.history_loaded = true;
                self.command_sidebar.history_error =
                    Some("background reader is unavailable".to_owned());
            }
        }
    }

    pub(crate) fn execute_pending_command_sidebar_action(&mut self) {
        if let Some(action) = self.command_sidebar.pending_action.take() {
            self.execute_command_sidebar_action(action);
        }
    }

    fn execute_command_sidebar_action(&mut self, action: CommandAction) {
        match action.kind {
            CommandActionKind::Jump => self.jump_to_sidebar_command(&action.target),
            CommandActionKind::CopyCommand => {
                self.copy_sidebar_command_text(&action.target, CopyKind::Command)
            }
            CommandActionKind::CopyOutput => {
                self.copy_sidebar_command_text(&action.target, CopyKind::Output)
            }
            CommandActionKind::CopyCombined => {
                self.copy_sidebar_command_text(&action.target, CopyKind::Combined)
            }
            CommandActionKind::Fill => self.replay_sidebar_command(&action.target, false),
            CommandActionKind::RunAgain => self.replay_sidebar_command(&action.target, true),
        }
    }

    fn target_session_index(&self, target: &CommandTarget) -> Option<usize> {
        self.session_manager
            .sessions()
            .iter()
            .position(|session| session.metadata.session_id == target.session_id)
    }

    fn persisted_sidebar_execution(&self, target: &CommandTarget) -> Option<&PersistedExecution> {
        if self.command_sidebar.history_session_id.as_deref() != Some(target.session_id.as_str()) {
            return None;
        }
        self.command_sidebar
            .history
            .iter()
            .find(|record| record.id == target.execution_id)
    }

    fn jump_to_sidebar_command(&mut self, target: &CommandTarget) {
        let Some(index) = self.target_session_index(target) else {
            self.set_status("Command session is no longer available");
            return;
        };
        // The Commands view is built from the active tab. Re-activating that
        // same session sets `force_resize_session`, and the resize later in
        // this frame resets the scrollback position we are about to choose.
        // Only switch when a stable target genuinely belongs to another tab.
        if self.session_manager.active_index() != index && !self.activate_session(index) {
            self.set_status("Command session is no longer available");
            return;
        }
        let jumped = {
            let Some(session) = self.session_manager.sessions().get(index) else {
                self.set_status("Command session is no longer available");
                return;
            };
            session
                .terminal
                .lock()
                .scroll_to_command(&target.execution_id)
        };
        if jumped {
            self.smooth_scroll_velocity = 0.0;
            self.smooth_scroll_pixel_offset = 0.0;
            self.renderer.scroll_pixel_offset = 0.0;
            for renderer in &mut self.pane_renderers {
                renderer.scroll_pixel_offset = 0.0;
            }
        } else {
            self.set_status("Command position is no longer in scrollback");
        }
    }

    fn copy_sidebar_command_text(&mut self, target: &CommandTarget, kind: CopyKind) {
        let Some(index) = self.target_session_index(target) else {
            self.set_status("Command session is no longer available");
            return;
        };
        let live_captured = self
            .session_manager
            .sessions()
            .get(index)
            .and_then(|session| {
                let terminal = session.terminal.lock();
                let record = terminal.command_record(&target.execution_id)?;
                let command = record.command.clone().unwrap_or_default();
                let command_exact = record.command_exact && !record.command_truncated;
                let output = match kind {
                    CopyKind::Command => None,
                    CopyKind::Output | CopyKind::Combined => terminal
                        .command_output_text(
                            &target.execution_id,
                            MAX_COMPLETED_COMMAND_OUTPUT_BYTES,
                        )
                        .map(|text| (text.text, text.truncated)),
                };
                Some((command, command_exact, output))
            });
        let persisted_captured = self.persisted_sidebar_execution(target).map(|record| {
            let output = match kind {
                CopyKind::Command => None,
                CopyKind::Output | CopyKind::Combined => record
                    .output
                    .as_ref()
                    .map(|output| (output.text.clone(), output.truncated)),
            };
            (
                record.command.clone(),
                !record.command_truncated && !record.command.is_empty(),
                output,
            )
        });
        let captured = match (live_captured, persisted_captured) {
            (Some((mut command, mut command_exact, mut output)), Some(persisted)) => {
                if !command_exact && persisted.1 {
                    command = persisted.0;
                    command_exact = true;
                }
                if output.is_none() {
                    output = persisted.2;
                }
                Some((command, command_exact, output))
            }
            (Some(captured), None) | (None, Some(captured)) => Some(captured),
            (None, None) => None,
        };
        let Some(captured) = captured else {
            self.set_status("Command record is no longer available");
            return;
        };

        let (text, truncated, label) = match kind {
            CopyKind::Command if !captured.1 || captured.0.is_empty() => {
                self.set_status("Exact command text is unavailable");
                return;
            }
            CopyKind::Command => (captured.0, false, "command"),
            CopyKind::Output => match captured.2 {
                Some((output, truncated)) if !output.is_empty() => {
                    (output, truncated, "command output")
                }
                _ => {
                    self.set_status("Command output is unavailable or empty");
                    return;
                }
            },
            CopyKind::Combined => {
                if !captured.1 || captured.0.is_empty() {
                    self.set_status("Exact command text is unavailable");
                    return;
                }
                let Some((output, truncated)) = captured.2 else {
                    self.set_status("Command output is unavailable");
                    return;
                };
                (
                    combine_command_and_output(&captured.0, &output),
                    truncated,
                    "command and output",
                )
            }
        };
        let char_count = text.chars().count();
        let copy_result = self
            .clipboard
            .as_ref()
            .map(|clipboard| clipboard.copy(&text));
        match copy_result {
            Some(Ok(())) => self.set_status(format!(
                "Copied {label} ({char_count} characters{})",
                if truncated { ", truncated" } else { "" }
            )),
            Some(Err(error)) => {
                self.set_status_for(format!("Copy failed: {error}"), Duration::from_secs(4))
            }
            None => self.set_status("Clipboard is unavailable"),
        }
    }

    fn replay_sidebar_command(&mut self, target: &CommandTarget, run: bool) {
        let Some(index) = self.target_session_index(target) else {
            self.set_status("Command session is no longer available");
            return;
        };
        if self.session_manager.active_index() != index && !self.activate_session(index) {
            self.set_status("Command session is no longer available");
            return;
        }

        let persisted_command = self
            .persisted_sidebar_execution(target)
            .filter(|record| !record.command_truncated && !record.command.is_empty())
            .map(|record| record.command.clone());
        let outcome = {
            let Some(session) = self.session_manager.get_session_mut(index) else {
                return self.set_status("Command session is no longer available");
            };
            let pending_input = !session.pending_input.is_empty();
            let replay = {
                let terminal = session.terminal.lock();
                let command = terminal
                    .command_record(&target.execution_id)
                    .and_then(|record| {
                        (record.command_exact && !record.command_truncated)
                            .then(|| record.command.clone())
                            .flatten()
                    })
                    .or(persisted_command);
                (
                    command,
                    terminal.shell_is_prompt_ready(),
                    terminal.is_alt_buffer(),
                    terminal.is_bracketed_paste_enabled(),
                )
            };
            let Some(command) = replay.0 else {
                return self.set_status("Exact command text is unavailable");
            };
            let command = trim_replay_command(&command);
            if replay.2 {
                ReplayOutcome::AlternateScreen
            } else if !replay.1 {
                ReplayOutcome::NotPromptReady
            } else if !replay.3 {
                ReplayOutcome::BracketedPasteDisabled
            } else if pending_input {
                ReplayOutcome::PendingInput
            } else if command.is_empty() {
                ReplayOutcome::EmptyCommand
            } else if run && replay_command_is_multiline(&command) {
                ReplayOutcome::MultilineRun
            } else {
                let mut payload = crate::wrap_bracketed_paste(command.as_bytes().to_vec());
                if run {
                    payload.push(b'\r');
                }
                match session.shell.write(&payload) {
                    Ok(()) => {
                        session.terminal.lock().scroll_to_bottom();
                        if run {
                            ReplayOutcome::Ran
                        } else {
                            ReplayOutcome::Filled
                        }
                    }
                    Err(error) => ReplayOutcome::WriteFailed(error),
                }
            }
        };

        match outcome {
            ReplayOutcome::Filled => self.set_status("Command filled at prompt"),
            ReplayOutcome::Ran => self.set_status("Command queued to run"),
            ReplayOutcome::NotPromptReady => {
                self.set_status("Wait for the shell prompt before replaying a command")
            }
            ReplayOutcome::AlternateScreen => {
                self.set_status("Cannot replay a command while an alternate-screen app is open")
            }
            ReplayOutcome::BracketedPasteDisabled => {
                self.set_status("Safe replay requires bracketed-paste mode")
            }
            ReplayOutcome::PendingInput => {
                self.set_status("Wait for pending terminal input to be delivered")
            }
            ReplayOutcome::EmptyCommand => self.set_status("Command text is empty"),
            ReplayOutcome::MultilineRun => {
                self.set_status("Run again is disabled for multiline commands; use Fill instead")
            }
            ReplayOutcome::WriteFailed(error) => self.set_status_for(
                format!("Command replay failed: {error}"),
                Duration::from_secs(4),
            ),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum CopyKind {
    Command,
    Output,
    Combined,
}

/// Persisted metadata may fill gaps in a command that already belongs to the
/// active tab, but it must never create a sidebar row on its own. A slice
/// makes that row-count invariant explicit.
fn enrich_current_tab_rows_from_history(
    rows: &mut [CommandRowSnapshot],
    history: &[PersistedExecution],
) {
    let history_by_id = history
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect::<HashMap<_, _>>();
    for row in rows {
        if let Some(record) = history_by_id.get(row.target.execution_id.as_str()) {
            enrich_live_row_from_history(row, record);
        }
    }
}

fn enrich_live_row_from_history(row: &mut CommandRowSnapshot, record: &PersistedExecution) {
    let exact_command = (!record.command_truncated)
        .then_some(record.command.trim())
        .filter(|command| !command.is_empty());
    if !row.command_exact {
        if let Some(command) = exact_command {
            row.command_summary = single_line_command_preview(command, 160);
            row.command_preview = single_line_command_preview(command, 512);
            row.command_exact = true;
            row.command_multiline = replay_command_is_multiline(&record.command);
        }
    }
    row.output_copy_available |= record
        .output
        .as_ref()
        .is_some_and(|output| !output.text.is_empty());
}

fn enrich_live_detail_from_history(
    detail: &mut CommandDetailSnapshot,
    record: &PersistedExecution,
) {
    if !detail.command_exact && !record.command_truncated && !record.command.is_empty() {
        detail.command = Some(detail_text_snapshot(
            &record.command,
            false,
            record.command.len(),
            COMMAND_DETAIL_COMMAND_BYTES,
        ));
        detail.command_exact = true;
        detail.command_omitted = false;
        detail.command_from_history = true;
    }
    if detail.output.is_none() {
        if let Some(output) = record.output.as_ref() {
            detail.output = Some(detail_text_snapshot(
                &output.text,
                output.truncated,
                usize::try_from(output.total_bytes).unwrap_or(usize::MAX),
                COMMAND_DETAIL_OUTPUT_BYTES,
            ));
            detail.output_from_history = true;
        }
    }
    detail.output_copy_available |= record
        .output
        .as_ref()
        .is_some_and(|output| !output.text.is_empty());
}

fn command_menu_item(
    ui: &mut egui::Ui,
    action: &mut Option<CommandAction>,
    row: &CommandRowSnapshot,
    label: &str,
    kind: CommandActionKind,
    disabled_reason: Option<&str>,
) {
    let response = ui.add_enabled(disabled_reason.is_none(), egui::Button::new(label));
    let response = if let Some(reason) = disabled_reason {
        response.on_disabled_hover_text(reason)
    } else {
        response
    };
    if response.clicked() {
        *action = Some(CommandAction {
            target: row.target.clone(),
            kind,
        });
        ui.close();
    }
}

fn combined_copy_disabled_reason(row: &CommandRowSnapshot) -> Option<&'static str> {
    if !row.command_exact {
        Some("The shell did not provide exact command metadata")
    } else if !row.output_copy_available {
        Some("Rendered command output is unavailable or empty")
    } else {
        None
    }
}

fn command_detail_action_button(
    ui: &mut egui::Ui,
    action: &mut Option<CommandAction>,
    row: &CommandRowSnapshot,
    label: &str,
    kind: CommandActionKind,
    disabled_reason: Option<&str>,
) {
    let response = ui.add_enabled(
        disabled_reason.is_none(),
        egui::Button::new(egui::RichText::new(label).small()).small(),
    );
    let response = if let Some(reason) = disabled_reason {
        response.on_disabled_hover_text(reason)
    } else {
        response
    };
    if response.clicked() {
        *action = Some(CommandAction {
            target: row.target.clone(),
            kind,
        });
    }
}

fn render_command_detail(
    ui: &mut egui::Ui,
    row: &CommandRowSnapshot,
    detail: &CommandDetailSnapshot,
    replay_guard: ReplayGuardSnapshot,
    action: &mut Option<CommandAction>,
    clear_selection: &mut bool,
) {
    egui::Frame::group(ui.style())
        .corner_radius(egui::CornerRadius::same(5))
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Command details").small().strong());
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui
                            .add(egui::Button::new(egui::RichText::new("×").small()).small())
                            .on_hover_text("Close details")
                            .clicked()
                        {
                            *clear_selection = true;
                        }
                    },
                );
            });

            match detail.command.as_ref() {
                Some(command) => {
                    egui::ScrollArea::vertical()
                        .id_salt((
                            "semantic_command_detail_command",
                            &detail.target.session_id,
                            &detail.target.execution_id,
                        ))
                        .max_height(96.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&command.text).monospace().small(),
                                )
                                .selectable(true)
                                .wrap(),
                            );
                        });
                    let mut provenance = if detail.command_exact && detail.command_from_history {
                        "exact rsh journal metadata".to_owned()
                    } else if detail.command_exact {
                        "exact shell metadata".to_owned()
                    } else if detail.command_from_history {
                        "truncated rsh journal metadata; replay disabled".to_owned()
                    } else {
                        "display-derived; replay disabled".to_owned()
                    };
                    if command.truncated {
                        provenance.push_str(&format!(
                            " · preview of {}",
                            format_byte_count(command.total_bytes)
                        ));
                    }
                    ui.label(
                        egui::RichText::new(provenance)
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }
                None => {
                    ui.label(
                        egui::RichText::new(if detail.command_omitted {
                            "Exact command omitted by the producer because it exceeded the limit."
                        } else {
                            "Command text is unavailable."
                        })
                        .small()
                        .color(ui.visuals().weak_text_color()),
                    );
                }
            }

            ui.add_space(3.0);
            ui.horizontal_wrapped(|ui| {
                command_detail_action_button(
                    ui,
                    action,
                    row,
                    "Jump",
                    CommandActionKind::Jump,
                    None,
                );
                command_detail_action_button(
                    ui,
                    action,
                    row,
                    "Copy cmd",
                    CommandActionKind::CopyCommand,
                    (!detail.command_exact)
                        .then_some("The shell did not provide exact command metadata"),
                );
                command_detail_action_button(
                    ui,
                    action,
                    row,
                    "Copy output",
                    CommandActionKind::CopyOutput,
                    (!detail.output_copy_available)
                        .then_some("Rendered command output is unavailable or empty"),
                );
                command_detail_action_button(
                    ui,
                    action,
                    row,
                    "Fill",
                    CommandActionKind::Fill,
                    replay_disabled_reason(row, replay_guard, false),
                );
                command_detail_action_button(
                    ui,
                    action,
                    row,
                    "Run",
                    CommandActionKind::RunAgain,
                    replay_disabled_reason(row, replay_guard, true),
                );
            });

            ui.separator();
            match (detail.state, detail.output.as_ref()) {
                (CommandState::Complete, Some(output)) if output.text.is_empty() => {
                    ui.label(
                        egui::RichText::new("Command produced no rendered output.")
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }
                (CommandState::Complete, Some(output)) => {
                    let output_source = if detail.output_from_history {
                        "Persisted output"
                    } else {
                        "Output"
                    };
                    let output_metadata = if output.truncated {
                        format!(
                            "{output_source} · truncated from {}",
                            format_byte_count(output.total_bytes)
                        )
                    } else {
                        format!("{output_source} · {}", format_byte_count(output.total_bytes))
                    };
                    ui.label(
                        egui::RichText::new(output_metadata)
                            .small()
                            .strong(),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt((
                            "semantic_command_detail_output",
                            &detail.target.session_id,
                            &detail.target.execution_id,
                        ))
                        .max_height(180.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&output.text).monospace().small(),
                                )
                                .selectable(true)
                                .wrap(),
                            );
                        });
                }
                (CommandState::Complete, None) => {
                    ui.label(
                        egui::RichText::new(
                            "Rendered output preview is unavailable (its terminal range may have been evicted).",
                        )
                        .small()
                        .color(ui.visuals().weak_text_color()),
                    );
                }
                (CommandState::Running, _) => {
                    ui.label(
                        egui::RichText::new("Command is running; output is captured on completion.")
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }
                (CommandState::Prompt | CommandState::Editing, _) => {
                    ui.label(
                        egui::RichText::new("Waiting for the command to run.")
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }
            }
        });
}

fn replay_disabled_reason(
    row: &CommandRowSnapshot,
    guard: ReplayGuardSnapshot,
    run: bool,
) -> Option<&'static str> {
    if !row.command_exact {
        Some("The shell did not provide exact command metadata")
    } else if guard.alternate_screen {
        Some("Unavailable while an alternate-screen app is open")
    } else if !guard.prompt_ready {
        Some("Wait for the shell prompt")
    } else if !guard.bracketed_paste {
        Some("Safe replay requires bracketed-paste mode")
    } else if guard.pending_input {
        Some("Wait for pending terminal input to be delivered")
    } else if run && row.command_multiline {
        Some("Use Fill for multiline commands")
    } else {
        None
    }
}

fn command_row_matches(row: &CommandRowSnapshot, query: &str, filter: CommandFilter) -> bool {
    let matches_filter = match filter {
        CommandFilter::All => true,
        CommandFilter::Failed => {
            row.state == CommandState::Complete && row.exit_code.is_some_and(|code| code != 0)
        }
        CommandFilter::Running => row.state == CommandState::Running,
    };
    matches_filter
        && (query.is_empty()
            || row.command_preview.to_lowercase().contains(query)
            || row
                .cwd
                .as_deref()
                .is_some_and(|cwd| cwd.to_lowercase().contains(query)))
}

fn command_status(row: &CommandRowSnapshot) -> (&'static str, egui::Color32, &'static str) {
    match row.state {
        CommandState::Prompt => ("○", egui::Color32::from_rgb(90, 160, 240), "Prompt"),
        CommandState::Editing => ("●", egui::Color32::from_rgb(90, 160, 240), "Editing"),
        CommandState::Running => ("●", egui::Color32::from_rgb(230, 175, 60), "Running"),
        CommandState::Complete if row.exit_code == Some(0) => {
            ("✓", egui::Color32::from_rgb(70, 190, 115), "Succeeded")
        }
        CommandState::Complete if row.exit_code.is_some() => {
            ("✕", egui::Color32::from_rgb(225, 85, 85), "Failed")
        }
        CommandState::Complete => ("○", egui::Color32::GRAY, "Completed"),
    }
}

fn command_metadata(row: &CommandRowSnapshot) -> String {
    let mut parts = Vec::with_capacity(4);
    if let Some(cwd) = row.cwd.as_deref() {
        parts.push(abbreviate_home(cwd));
    }
    if let Some(duration_ms) = row.duration_ms {
        parts.push(format_duration(duration_ms));
    }
    if let Some(age) = format_age(row.started_at) {
        parts.push(age);
    }
    if row.state == CommandState::Complete {
        if let Some(exit_code) = row.exit_code {
            parts.push(format!("exit {exit_code}"));
        }
    }
    parts.join(" · ")
}

fn abbreviate_home(cwd: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let cwd_path = std::path::Path::new(cwd);
        if cwd_path == home {
            return "~".to_string();
        }
        if let Ok(rest) = cwd_path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    cwd.to_string()
}

fn format_duration(duration_ms: u64) -> String {
    if duration_ms < 1_000 {
        format!("{duration_ms}ms")
    } else if duration_ms < 60_000 {
        format!("{:.1}s", duration_ms as f64 / 1_000.0)
    } else {
        let seconds = duration_ms / 1_000;
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    }
}

fn format_age(started_at: Option<SystemTime>) -> Option<String> {
    let age = SystemTime::now().duration_since(started_at?).ok()?;
    Some(if age < Duration::from_secs(10) {
        "now".to_string()
    } else if age < Duration::from_secs(60) {
        format!("{}s ago", age.as_secs())
    } else if age < Duration::from_secs(60 * 60) {
        format!("{}m ago", age.as_secs() / 60)
    } else if age < Duration::from_secs(24 * 60 * 60) {
        format!("{}h ago", age.as_secs() / (60 * 60))
    } else {
        format!("{}d ago", age.as_secs() / (24 * 60 * 60))
    })
}

fn detail_text_snapshot(
    value: &str,
    source_truncated: bool,
    total_bytes: usize,
    max_bytes: usize,
) -> DetailTextSnapshot {
    if value.len() <= max_bytes {
        return DetailTextSnapshot {
            text: value.to_owned(),
            truncated: source_truncated,
            total_bytes: total_bytes.max(value.len()),
        };
    }

    let marker = if max_bytes >= DETAIL_TRUNCATION_MARKER.len() + 2 {
        DETAIL_TRUNCATION_MARKER
    } else {
        ""
    };
    let payload_budget = max_bytes.saturating_sub(marker.len());
    let head_budget = payload_budget / 2;
    let tail_budget = payload_budget - head_budget;
    let mut head_end = head_budget.min(value.len());
    while !value.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = value.len().saturating_sub(tail_budget);
    while !value.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let mut text = String::with_capacity(max_bytes);
    text.push_str(&value[..head_end]);
    text.push_str(marker);
    text.push_str(&value[tail_start..]);
    debug_assert!(text.len() <= max_bytes);
    DetailTextSnapshot {
        text,
        truncated: true,
        total_bytes: total_bytes.max(value.len()),
    }
}

fn format_byte_count(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn trim_replay_command(command: &str) -> String {
    command.trim_end_matches(&['\r', '\n'][..]).to_string()
}

fn replay_command_is_multiline(command: &str) -> bool {
    command
        .trim_end_matches(&['\r', '\n'][..])
        .chars()
        .any(|ch| matches!(ch, '\r' | '\n'))
}

fn single_line_command_preview(command: &str, max_chars: usize) -> String {
    let mut chars = command.chars().peekable();
    let mut preview = String::new();
    let mut consumed = 0;
    while consumed < max_chars {
        let Some(ch) = chars.next() else {
            break;
        };
        consumed += 1;
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                preview.push_str(" ↵ ");
            }
            '\n' => preview.push_str(" ↵ "),
            control if control.is_control() => preview.push('�'),
            visible => preview.push(visible),
        }
    }
    if chars.next().is_some() {
        preview.push('…');
    }
    preview
}

fn combine_command_and_output(command: &str, output: &str) -> String {
    if output.is_empty() {
        command.to_string()
    } else {
        format!(
            "{}\n{}",
            command.trim_end_matches(&['\r', '\n'][..]),
            output
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persisted_test_record() -> PersistedExecution {
        PersistedExecution {
            id: "persisted-execution".to_owned(),
            seq: 9,
            command: "printf hi".to_owned(),
            command_truncated: false,
            cwd: "/tmp".to_owned(),
            started_at_ms: 1_000,
            exit_code: Some(0),
            duration_ms: Some(12),
            cwd_after: Some("/tmp".to_owned()),
            ended_at_ms: Some(1_012),
            output: Some(jterm_core::execution_journal::PersistedExecutionOutput {
                text: "hi".to_owned(),
                truncated: false,
                total_bytes: 2,
                captured_at_ms: 1_012,
            }),
        }
    }

    fn replay_test_row(exact: bool, multiline: bool) -> CommandRowSnapshot {
        CommandRowSnapshot {
            target: CommandTarget {
                session_id: "session".to_owned(),
                execution_id: "execution".to_owned(),
            },
            sequence: 1,
            command_summary: "echo test".to_owned(),
            command_preview: "echo test".to_owned(),
            command_exact: exact,
            command_multiline: multiline,
            cwd: None,
            state: CommandState::Complete,
            exit_code: Some(0),
            duration_ms: None,
            started_at: None,
            output_copy_available: false,
        }
    }

    fn ready_replay_guard() -> ReplayGuardSnapshot {
        ReplayGuardSnapshot {
            prompt_ready: true,
            alternate_screen: false,
            bracketed_paste: true,
            pending_input: false,
        }
    }

    #[test]
    fn replay_trims_only_trailing_line_endings() {
        assert_eq!(
            trim_replay_command("printf 'a\\nb'\r\n\n"),
            "printf 'a\\nb'"
        );
        assert_eq!(trim_replay_command(" echo hi  "), " echo hi  ");
    }

    #[test]
    fn multiline_replay_detection_ignores_only_trailing_line_endings() {
        assert!(!replay_command_is_multiline("echo hi\r\n"));
        assert!(replay_command_is_multiline("printf one\nprintf two\n"));
        assert!(replay_command_is_multiline("printf one\rprintf two"));
    }

    #[test]
    fn command_detail_preview_is_utf8_safe_bounded_and_keeps_both_ends() {
        let value = format!("start-{}-end", "雪".repeat(40));
        let preview = detail_text_snapshot(&value, false, value.len(), 48);

        assert!(preview.truncated);
        assert_eq!(preview.total_bytes, value.len());
        assert!(preview.text.len() <= 48);
        assert!(preview.text.starts_with("start"));
        assert!(preview.text.ends_with("end"));
        assert!(preview.text.contains("preview truncated"));
        assert!(std::str::from_utf8(preview.text.as_bytes()).is_ok());
    }

    #[test]
    fn command_detail_preview_preserves_source_truncation_and_total_size() {
        let preview = detail_text_snapshot("short", true, 4096, 128);
        assert_eq!(preview.text, "short");
        assert!(preview.truncated);
        assert_eq!(preview.total_bytes, 4096);
        assert_eq!(format_byte_count(12), "12 B");
        assert_eq!(format_byte_count(1536), "1.5 KiB");
    }

    #[test]
    fn semantic_filters_compose_with_command_and_cwd_search() {
        let mut row = replay_test_row(true, false);
        row.command_preview = "cargo test".to_owned();
        row.cwd = Some("/work/jterm2".to_owned());

        assert!(command_row_matches(&row, "cargo", CommandFilter::All));
        assert!(command_row_matches(&row, "jterm2", CommandFilter::All));
        assert!(!command_row_matches(&row, "missing", CommandFilter::All));
        assert!(!command_row_matches(&row, "", CommandFilter::Failed));

        row.exit_code = Some(101);
        assert!(command_row_matches(&row, "cargo", CommandFilter::Failed));

        row.state = CommandState::Running;
        row.exit_code = None;
        assert!(command_row_matches(&row, "", CommandFilter::Running));
        assert!(!command_row_matches(&row, "", CommandFilter::Failed));
    }

    #[test]
    fn replay_menu_guard_tracks_rsh_editor_safety_requirements() {
        let exact = replay_test_row(true, false);
        assert_eq!(
            replay_disabled_reason(&exact, ready_replay_guard(), false),
            None
        );
        assert_eq!(
            replay_disabled_reason(&exact, ready_replay_guard(), true),
            None
        );

        let mut guard = ready_replay_guard();
        guard.alternate_screen = true;
        assert!(replay_disabled_reason(&exact, guard, false).is_some());

        let mut guard = ready_replay_guard();
        guard.prompt_ready = false;
        assert!(replay_disabled_reason(&exact, guard, false).is_some());

        let mut guard = ready_replay_guard();
        guard.bracketed_paste = false;
        assert!(replay_disabled_reason(&exact, guard, false).is_some());

        let mut guard = ready_replay_guard();
        guard.pending_input = true;
        assert!(replay_disabled_reason(&exact, guard, false).is_some());

        let multiline = replay_test_row(true, true);
        assert_eq!(
            replay_disabled_reason(&multiline, ready_replay_guard(), false),
            None
        );
        assert!(replay_disabled_reason(&multiline, ready_replay_guard(), true).is_some());

        let inexact = replay_test_row(false, false);
        assert!(replay_disabled_reason(&inexact, ready_replay_guard(), false).is_some());
    }

    #[test]
    fn journal_only_records_never_create_current_tab_rows() {
        let mut matching_record = persisted_test_record();
        matching_record.id = "execution".to_owned();
        let unmatched_record = persisted_test_record();
        let mut rows = vec![replay_test_row(false, false)];
        rows[0].command_summary = "(command omitted)".to_owned();
        rows[0].command_preview = rows[0].command_summary.clone();

        enrich_current_tab_rows_from_history(&mut rows, &[unmatched_record, matching_record]);

        assert_eq!(rows.len(), 1);
        assert!(rows[0].command_exact);
        assert_eq!(rows[0].command_summary, "printf hi");
        assert!(rows[0].output_copy_available);
    }

    #[test]
    fn combined_copy_has_one_boundary_newline() {
        assert_eq!(
            combine_command_and_output("echo hi\n", "hi\n"),
            "echo hi\nhi\n"
        );
        assert_eq!(combine_command_and_output("true", ""), "true");
    }

    #[test]
    fn command_preview_is_single_line_and_bounded() {
        assert_eq!(
            single_line_command_preview("one\r\ntwo\nthree", 100),
            "one ↵ two ↵ three"
        );
        assert_eq!(single_line_command_preview("abcdef", 3), "abc…");
    }
}
