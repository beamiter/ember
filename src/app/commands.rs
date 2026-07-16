//! Continuous-terminal command timeline UI.
//!
//! The terminal owns semantic execution records. This module snapshots only
//! the small fields needed to paint the sidebar, records an action while egui
//! closures are active, and performs terminal/clipboard/PTY work afterwards.

use super::state::TerminalApp;
use crate::terminal::{CommandState, MAX_COMPLETED_COMMAND_OUTPUT_BYTES};
use eframe::egui;
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandTarget {
    pub session_id: String,
    pub execution_id: String,
}

#[derive(Clone, Debug, Default)]
pub struct CommandSidebarState {
    pub query: String,
    pub selected: Option<CommandTarget>,
    pending_action: Option<CommandAction>,
}

#[derive(Clone, Debug)]
struct CommandRowSnapshot {
    target: CommandTarget,
    sequence: u64,
    command_summary: String,
    command_preview: String,
    command_exact: bool,
    cwd: Option<String>,
    state: CommandState,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    started_at: Option<SystemTime>,
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
        let (session_id, session_title, mut rows) = {
            let Some(session) = self.session_manager.sessions().get(active_index) else {
                ui.label("No active session");
                return;
            };
            let session_id = session.metadata.session_id.clone();
            let session_title = Self::session_cwd_title(session);
            let terminal = session.terminal.lock();
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
                        cwd: record
                            .cwd
                            .as_deref()
                            .map(|cwd| single_line_command_preview(cwd, 256)),
                        state: record.state,
                        exit_code: record.exit_code,
                        duration_ms: record.duration_ms,
                        started_at: record.started_at,
                    })
                })
                .collect::<Vec<_>>();
            (session_id, session_title, rows)
        };
        rows.sort_by_key(|row| row.sequence);

        ui.label(
            egui::RichText::new(session_title)
                .small()
                .color(ui.visuals().weak_text_color()),
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

        let query = self.command_sidebar.query.trim().to_lowercase();
        let visible_rows = rows
            .iter()
            .filter(|row| command_row_matches(row, &query))
            .collect::<Vec<_>>();
        ui.label(
            egui::RichText::new(if query.is_empty() {
                format!("{} commands", visible_rows.len())
            } else {
                format!("{} of {} commands", visible_rows.len(), rows.len())
            })
            .small()
            .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(2.0);

        let mut action = None;
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
                                row.command_exact,
                            );
                            command_menu_item(
                                ui,
                                &mut action,
                                row,
                                "Copy output",
                                CommandActionKind::CopyOutput,
                                true,
                            );
                            command_menu_item(
                                ui,
                                &mut action,
                                row,
                                "Copy command + output",
                                CommandActionKind::CopyCombined,
                                row.command_exact,
                            );
                            ui.separator();
                            command_menu_item(
                                ui,
                                &mut action,
                                row,
                                "Fill at prompt",
                                CommandActionKind::Fill,
                                row.command_exact,
                            );
                            command_menu_item(
                                ui,
                                &mut action,
                                row,
                                "Run again",
                                CommandActionKind::RunAgain,
                                row.command_exact,
                            );
                        });
                        ui.add_space(2.0);
                    }
                });
        }

        // The containing Panel::show closure is still alive here. Stage the
        // action so main.rs can execute it after that outer closure returns.
        if action.is_some() {
            self.command_sidebar.pending_action = action;
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

    fn jump_to_sidebar_command(&mut self, target: &CommandTarget) {
        let Some(index) = self.target_session_index(target) else {
            self.set_status("Command session is no longer available");
            return;
        };
        if !self.activate_session(index) {
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
        let captured = self
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
        if !self.activate_session(index) {
            self.set_status("Command session is no longer available");
            return;
        }

        let outcome = {
            let Some(session) = self.session_manager.get_session_mut(index) else {
                return self.set_status("Command session is no longer available");
            };
            if !session.pending_input.is_empty() {
                ReplayOutcome::PendingInput
            } else {
                let replay = {
                    let terminal = session.terminal.lock();
                    let command =
                        terminal
                            .command_record(&target.execution_id)
                            .and_then(|record| {
                                (record.command_exact && !record.command_truncated)
                                    .then(|| record.command.clone())
                                    .flatten()
                            });
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
                if !replay.1 {
                    ReplayOutcome::NotPromptReady
                } else if replay.2 {
                    ReplayOutcome::AlternateScreen
                } else if !replay.3 {
                    ReplayOutcome::BracketedPasteDisabled
                } else if command.is_empty() {
                    ReplayOutcome::EmptyCommand
                } else if run && command.chars().any(|ch| matches!(ch, '\r' | '\n')) {
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

fn command_menu_item(
    ui: &mut egui::Ui,
    action: &mut Option<CommandAction>,
    row: &CommandRowSnapshot,
    label: &str,
    kind: CommandActionKind,
    enabled: bool,
) {
    let response = ui
        .add_enabled(enabled, egui::Button::new(label))
        .on_disabled_hover_text("The shell did not provide exact command metadata");
    if response.clicked() {
        *action = Some(CommandAction {
            target: row.target.clone(),
            kind,
        });
        ui.close();
    }
}

fn command_row_matches(row: &CommandRowSnapshot, query: &str) -> bool {
    query.is_empty()
        || row.command_preview.to_lowercase().contains(query)
        || row
            .cwd
            .as_deref()
            .is_some_and(|cwd| cwd.to_lowercase().contains(query))
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
    let mut parts = Vec::with_capacity(3);
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

fn trim_replay_command(command: &str) -> String {
    command.trim_end_matches(&['\r', '\n'][..]).to_string()
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

    #[test]
    fn replay_trims_only_trailing_line_endings() {
        assert_eq!(
            trim_replay_command("printf 'a\\nb'\r\n\n"),
            "printf 'a\\nb'"
        );
        assert_eq!(trim_replay_command(" echo hi  "), " echo hi  ");
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
