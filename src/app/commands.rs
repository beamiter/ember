//! Continuous-terminal command timeline UI.
//!
//! The terminal owns semantic execution records. This module snapshots only
//! the small fields needed to paint the sidebar, records an action while egui
//! closures are active, and performs terminal/clipboard/PTY work afterwards.

use super::state::TerminalApp;
use crate::execution_journal::{self, HistoryLoad, HistoryRequestError, PersistedExecution};
use crate::terminal::{CommandRecord, CommandState, MAX_COMPLETED_COMMAND_OUTPUT_BYTES};
use eframe::egui;
use jterm_core::block_contract::{classify_completed, CompletedBlockOutcome};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

const COMMAND_DETAIL_COMMAND_BYTES: usize = 8 * 1024;
const COMMAND_DETAIL_OUTPUT_BYTES: usize = 16 * 1024;
const DETAIL_TRUNCATION_MARKER: &str = "\n… preview truncated …\n";
const MAX_BLOCK_CLIPBOARD_BYTES: usize = 32 * 1024 * 1024;

fn block_absence_message(has_prompt_marks: bool, ordinary: &str) -> String {
    if has_prompt_marks {
        ordinary.to_string()
    } else {
        format!(
            "{ordinary}: this shell is not reporting commands — run \"Install or update jsh\" \
             from the command palette for shell integration"
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockClipboardBuildError {
    TooLarge,
    Allocation,
}

fn append_bounded_block_part(
    aggregate: &mut String,
    part: &str,
    separator: &str,
    limit: usize,
) -> Result<(), BlockClipboardBuildError> {
    let separator = if aggregate.is_empty() { "" } else { separator };
    let additional = separator
        .len()
        .checked_add(part.len())
        .ok_or(BlockClipboardBuildError::TooLarge)?;
    let next_len = aggregate
        .len()
        .checked_add(additional)
        .ok_or(BlockClipboardBuildError::TooLarge)?;
    if next_len > limit {
        return Err(BlockClipboardBuildError::TooLarge);
    }
    aggregate
        .try_reserve(additional)
        .map_err(|_| BlockClipboardBuildError::Allocation)?;
    aggregate.push_str(separator);
    aggregate.push_str(part);
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandTarget {
    pub session_id: String,
    pub execution_id: String,
}

fn retained_record_version(
    records: &std::collections::VecDeque<crate::terminal::CommandRecord>,
) -> crate::block_search::RetainedRecordVersion {
    crate::block_search::RetainedRecordVersion {
        len: records.len(),
        oldest_sequence: records.front().map(|record| record.sequence),
        newest_sequence: records.back().map(|record| record.sequence),
    }
}

fn block_search_record_version(
    records: &std::collections::VecDeque<crate::terminal::CommandRecord>,
) -> crate::block_search::BlockSearchRecordVersion {
    let mut complete = records.iter().filter(|record| record.complete);
    let oldest_sequence = complete.next().map(|record| record.sequence);
    let mut len = usize::from(oldest_sequence.is_some());
    let mut newest_sequence = oldest_sequence;
    for record in complete {
        len += 1;
        newest_sequence = Some(record.sequence);
    }
    crate::block_search::BlockSearchRecordVersion {
        len,
        oldest_sequence,
        newest_sequence,
    }
}

fn first_meaningful_line(text: &str) -> Option<(usize, &str)> {
    text.lines()
        .enumerate()
        .find(|(_, line)| !line.trim().is_empty())
}

fn metadata_browse_display(
    record: &crate::block_mode::CachedBlockSearchRecord,
    scope: crate::block_mode::BlockSearchScope,
) -> Option<(&str, bool, Option<usize>)> {
    let command = record
        .command
        .as_deref()
        .map(|command| (command, false, None));
    let output = record
        .output
        .as_deref()
        .and_then(first_meaningful_line)
        .map(|(index, line)| (line, true, Some(index + 1)));
    match scope {
        crate::block_mode::BlockSearchScope::All => command.or(output),
        crate::block_mode::BlockSearchScope::Command => command,
        crate::block_mode::BlockSearchScope::Output => output,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockSearchActivation {
    RejectStale,
    Close,
    Advance,
}

fn block_search_activation(
    target_is_available: bool,
    continuous_review: bool,
) -> BlockSearchActivation {
    if !target_is_available {
        BlockSearchActivation::RejectStale
    } else if continuous_review {
        BlockSearchActivation::Advance
    } else {
        BlockSearchActivation::Close
    }
}

/// Keep the last usable same-pane index/results when a query intent cannot be
/// compiled. A completed-record version may change in the same frame as a
/// regex toggle or query edit; validating against an empty cache before the
/// destructive rebuild prevents that race from blanking the picker merely to
/// rediscover the same error. A pane switch deliberately bypasses this gate so
/// results from the previous terminal are still released immediately.
fn defer_same_session_block_search_rebuild_if_invalid(
    state: &mut crate::block_search::BlockSearchState,
    active_session_id: &str,
) -> bool {
    if state.session_id.as_deref() != Some(active_session_id) {
        return false;
    }
    // The current intent was already compiled and rejected. Record-version
    // churn does not make an invalid expression valid, so avoid recompiling it
    // on every rendered frame while the source refresh remains deferred.
    if state.query_error.is_some() && state.computed_query.as_deref() == Some(state.query.as_str())
    {
        return true;
    }
    let options = crate::block_mode::BlockSearchOptions {
        case_sensitive: state.case_sensitive,
        regex: state.regex,
        whole_word: state.whole_word,
    };
    match crate::block_mode::search_blocks_with_options_filtered_in_scope(
        &[],
        &state.query,
        options,
        state.scope,
        |_| true,
    ) {
        Ok(_) => false,
        Err(error) => {
            state.query_error = Some(error.to_string());
            state.computed_query = Some(state.query.clone());
            true
        }
    }
}

/// Retire the terminal and sidebar views of one logical block selection.
/// Taking the fields separately lets input routing call this while it holds a
/// disjoint mutable borrow of the active session.
pub(crate) fn clear_block_selection_state(
    block_selection: &mut Option<crate::block_mode::BlockSelection>,
    sidebar_selection: &mut Option<CommandTarget>,
) {
    *block_selection = None;
    *sidebar_selection = None;
}

pub(crate) fn block_selection_state_targets_session(
    block_selection: Option<&crate::block_mode::BlockSelection>,
    sidebar_selection: Option<&CommandTarget>,
    session_id: &str,
) -> bool {
    block_selection.is_some_and(|selection| selection.session_id == session_id)
        || sidebar_selection.is_some_and(|target| target.session_id == session_id)
}

pub(crate) fn clear_block_selection_state_for_session(
    block_selection: &mut Option<crate::block_mode::BlockSelection>,
    sidebar_selection: &mut Option<CommandTarget>,
    session_id: &str,
) {
    if block_selection_state_targets_session(
        block_selection.as_ref(),
        sidebar_selection.as_ref(),
        session_id,
    ) {
        clear_block_selection_state(block_selection, sidebar_selection);
    }
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
    command_context_fits: bool,
    command_multiline: bool,
    cwd: Option<String>,
    cwd_context_fits: bool,
    state: CommandState,
    complete: bool,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    started_at: Option<SystemTime>,
    output_copy_available: bool,
    output_context_available: bool,
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

/// How far a block reveal may move the viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockReveal {
    /// Explicit "take me there" gesture (a Commands-sidebar row click): always
    /// re-pin the block at the top of the viewport.
    Force,
    /// Selection movement: keep the reader's viewport when the block header is
    /// already on screen, and only scroll for an off-screen target.
    IfOffscreen,
}

#[derive(Clone, Copy, Debug)]
enum CommandActionKind {
    Jump,
    CopyCommand,
    CopyOutput,
    CopyCombined,
    Fill,
    RunAgain,
    FixWithAgent,
    ExplainWithAgent,
    CreateAgentTask,
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
    PromptNotEmpty,
    EmptyCommand,
    UnsafeCommand(String),
    MultilineRun,
    WorkingDirectoryChanged,
    WriteFailed(crate::shell::ShellWriteError),
}

fn replay_outcome_accepted(outcome: &ReplayOutcome) -> bool {
    matches!(outcome, ReplayOutcome::Filled | ReplayOutcome::Ran)
}

/// An execution-authorizing action needs an independently observed local
/// shell cwd. OSC 7/133 remains useful display/context data, but because any
/// PTY program can emit it, it cannot validate itself. Requiring `/proc`'s
/// shell cwd intentionally disables automatic Run/Fix on remote wrappers and
/// multiplexers whose local process cwd does not describe the reported
/// workspace; those need a future explicit remote backend.
pub(crate) fn verified_local_command_cwd(
    recorded: &str,
    reported: Option<&str>,
    process: Option<&str>,
) -> bool {
    let recorded = std::path::Path::new(recorded);
    recorded.is_absolute()
        && process.is_some_and(|process| std::path::Path::new(process) == recorded)
        && reported.is_none_or(|reported| std::path::Path::new(reported) == recorded)
}

#[derive(Debug)]
enum SelectedReplayError {
    NoSelection,
    MissingRecord,
    ExactCommandUnavailable,
    NoCommands,
    NotPromptReady,
    AlternateScreen,
    BracketedPasteDisabled,
    PendingInput,
    PromptNotEmpty,
    UnsafeCommand(crate::review_text::ReviewTextError),
    TooLarge { limit: usize },
    WriteFailed(crate::shell::ShellWriteError),
}

#[derive(Clone, Copy)]
struct SelectedReplayRecord<'a> {
    id: &'a str,
    command: Option<&'a str>,
    exact: bool,
    truncated: bool,
    complete: bool,
}

#[derive(Clone, Copy, Debug)]
struct SelectedReplayGuard {
    alternate_screen: bool,
    prompt_ready: bool,
    bracketed_paste: bool,
    pending_input: bool,
    prompt_empty: bool,
}

/// Gate ownership before replay mechanics. A foreground/alt-screen program
/// always owns Enter. At an idle prompt, determine whether the selection has a
/// real command before requiring bracketed paste or an empty pending queue, so
/// a background-only range still lets Enter pass through.
fn prepare_selected_replay<T>(
    guard: SelectedReplayGuard,
    prepare: impl FnOnce() -> Result<T, SelectedReplayError>,
) -> Result<T, SelectedReplayError> {
    if guard.alternate_screen {
        return Err(SelectedReplayError::AlternateScreen);
    }
    if !guard.prompt_ready {
        return Err(SelectedReplayError::NotPromptReady);
    }
    let prepared = prepare()?;
    if !guard.prompt_empty {
        return Err(SelectedReplayError::PromptNotEmpty);
    }
    if !guard.bracketed_paste {
        return Err(SelectedReplayError::BracketedPasteDisabled);
    }
    if guard.pending_input {
        return Err(SelectedReplayError::PendingInput);
    }
    Ok(prepared)
}

impl TerminalApp {
    fn active_pane_has_prompt_marks(&self) -> bool {
        let index = self.session_manager.active_index();
        self.session_manager
            .sessions()
            .get(index)
            .is_some_and(|session| session.terminal.lock().has_prompt_marks())
    }

    /// Explain why a block action found no target without blaming shell
    /// integration on panes that are already reporting OSC 133 correctly.
    fn explain_block_absence(&self, ordinary: &str) -> String {
        block_absence_message(self.active_pane_has_prompt_marks(), ordinary)
    }
    /// Whether a direct, atomic UI write would overtake an older mouse edge or
    /// protocol reply for `session_id`. Callers still check the session's
    /// `pending_input`: this gate covers the independent producer FIFOs.
    pub(crate) fn direct_input_is_blocked_for_session(&self, session_id: &str) -> bool {
        let mouse_barrier_session_id = self
            .terminal_mouse_capture
            .as_ref()
            .filter(|capture| capture.reported_to_app && !capture.pending_controls.is_empty())
            .map(|capture| capture.session_id.as_str());
        let Some(index) = self.session_manager.index_of(session_id) else {
            return true;
        };
        let Some(protocol_responses) = self.session_manager.protocol_response_sender(index) else {
            return true;
        };
        crate::session_manager::user_input_flush_block(
            session_id,
            mouse_barrier_session_id,
            &self.osc_paste_input_barriers,
            &protocol_responses,
        )
        .is_some()
    }

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
                    // Match the shared completed-block contract's Unicode
                    // whitespace rule before constructing a command-only row.
                    let command = record
                        .command
                        .as_deref()
                        .filter(|command| !command.trim().is_empty());
                    let replayable_command = command.and_then(|command| {
                        crate::review_text::sanitize_history_replay(
                            command,
                            crate::review_text::MAX_HISTORY_COMMAND_BYTES,
                        )
                        .ok()
                    });
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
                    // A completed Agent task needs the owned C..D snapshot,
                    // including a genuinely empty one. Bare anchors can still
                    // point at rows already evicted from scrollback, so they
                    // must not advertise context availability on their own.
                    let output_context_available = record.captured_output.is_some();
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
                            && replayable_command.is_some(),
                        command_context_fits: command.is_some_and(|command| {
                            command.len() <= crate::agent::context::AGENT_BLOCK_COMMAND_PROMPT_BYTES
                        }),
                        command_multiline: replayable_command
                            .as_deref()
                            .is_some_and(replay_command_is_multiline),
                        cwd: record
                            .cwd
                            .as_deref()
                            .map(|cwd| single_line_command_preview(cwd, 256)),
                        cwd_context_fits: record.cwd.as_ref().is_some_and(|cwd| {
                            !cwd.trim().is_empty()
                                && cwd.len() <= crate::agent::context::AGENT_BLOCK_CWD_PROMPT_BYTES
                        }),
                        state: record.state,
                        complete: record.complete,
                        exit_code: record.exit_code,
                        duration_ms: record.duration_ms,
                        started_at: record.started_at,
                        output_copy_available,
                        output_context_available,
                    })
                })
                .collect::<Vec<_>>();
            let selected_detail = selected_before
                .as_ref()
                .filter(|target| target.session_id == session_id)
                .and_then(|target| {
                    let record = terminal.command_record(&target.execution_id)?;
                    let replayable_command = record.command.as_deref().and_then(|command| {
                        crate::review_text::sanitize_history_replay(
                            command,
                            crate::review_text::MAX_HISTORY_COMMAND_BYTES,
                        )
                        .ok()
                    });
                    let command = record.command.as_deref().map(|command| {
                        let visible = crate::review_text::visible_bounded(
                            command,
                            COMMAND_DETAIL_COMMAND_BYTES,
                        );
                        detail_text_snapshot(
                            &visible,
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
                        command_exact: record.command_exact
                            && !record.command_truncated
                            && replayable_command.is_some(),
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
        let mut selected_detail = live_selected_detail;
        if let Some(session) = self.session_manager.sessions().get(active_index) {
            let terminal = session.terminal.lock();
            enrich_current_tab_rows_from_history(
                &mut rows,
                terminal.command_records(),
                &self.command_sidebar.history,
            );
            if let Some(detail) = selected_detail.as_mut() {
                if let Some(record) = self.command_sidebar.history.iter().find(|persisted| {
                    terminal
                        .command_record(&detail.target.execution_id)
                        .is_some_and(|live| persisted_execution_matches_live(live, persisted))
                }) {
                    enrich_live_detail_from_history(detail, record);
                }
            }
        }
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
                            // Full-duplex sync with the terminal's block
                            // selection (id-based, same session): a sidebar
                            // click selects the block, just as selecting a
                            // block highlights the sidebar row.
                            self.block_selection =
                                (self.config.block_mode && row.complete).then(|| {
                                    crate::block_mode::BlockSelection::single(
                                        row.target.session_id.clone(),
                                        row.target.execution_id.clone(),
                                    )
                                });
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
                                if completed_command_row_is_failed(row) {
                                    "Retry"
                                } else {
                                    "Run again"
                                },
                                CommandActionKind::RunAgain,
                                replay_disabled_reason(row, replay_guard, true),
                            );
                            if completed_command_row_is_failed(row) {
                                ui.separator();
                                command_menu_item(
                                    ui,
                                    &mut action,
                                    row,
                                    "Fix with Agent",
                                    CommandActionKind::FixWithAgent,
                                    agent_task_disabled_reason(row),
                                );
                                command_menu_item(
                                    ui,
                                    &mut action,
                                    row,
                                    "Explain with Agent",
                                    CommandActionKind::ExplainWithAgent,
                                    agent_task_disabled_reason(row),
                                );
                                command_menu_item(
                                    ui,
                                    &mut action,
                                    row,
                                    "Create Agent Task",
                                    CommandActionKind::CreateAgentTask,
                                    agent_task_disabled_reason(row),
                                );
                            }
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
            self.clear_block_selection();
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
            CommandActionKind::Fill => self.replay_sidebar_command(&action.target, false, false),
            CommandActionKind::RunAgain => self.replay_sidebar_command(&action.target, true, false),
            CommandActionKind::FixWithAgent => {
                self.start_agent_task_for_command(&action.target, AgentTaskIntent::Fix)
            }
            CommandActionKind::ExplainWithAgent => {
                self.start_agent_task_for_command(&action.target, AgentTaskIntent::Explain)
            }
            CommandActionKind::CreateAgentTask => {
                self.start_agent_task_for_command(&action.target, AgentTaskIntent::Compose)
            }
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
        let session = self
            .session_manager
            .sessions()
            .iter()
            .find(|session| session.metadata.session_id == target.session_id)?;
        let terminal = session.terminal.lock();
        let live = terminal.command_record(&target.execution_id)?;
        let index = self
            .command_sidebar
            .history
            .iter()
            .position(|persisted| persisted_execution_matches_live(live, persisted))?;
        drop(terminal);
        self.command_sidebar.history.get(index)
    }

    /// Whether a stable buffer anchor is already painted somewhere in the
    /// currently rendered projected viewport. `origins` only ever describe the
    /// rendered window, so `display_point_for` answering `Some` is exactly the
    /// "already on screen" question — no document-row arithmetic, and it fails
    /// closed to `false` whenever the anchor cannot be resolved.
    fn projected_anchor_is_displayed(
        terminal: &crate::terminal::TerminalState,
        viewport: &crate::terminal::ProjectedViewport,
        anchor: crate::terminal::BufferAnchor,
    ) -> bool {
        terminal
            .buffer_anchor_to_absolute(anchor)
            .and_then(|(row, column)| {
                terminal
                    .raw_row_id_at_absolute(row)
                    .map(|row_id| crate::terminal::RawCellAnchor { row_id, column })
            })
            .and_then(|raw| viewport.display_point_for(raw))
            .is_some()
    }

    /// Explicit "take me there": activate the owning pane and re-pin the block
    /// at the top of the viewport. This is the Commands-sidebar row gesture.
    fn jump_to_sidebar_command(&mut self, target: &CommandTarget) {
        self.reveal_sidebar_command(target, BlockReveal::Force);
    }

    /// Activate the owning pane and bring `target` into view under `reveal`.
    fn reveal_sidebar_command(&mut self, target: &CommandTarget, reveal: BlockReveal) {
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
            let block_mode = self.config.block_mode;
            let Some(session) = self.session_manager.sessions_mut().get_mut(index) else {
                self.set_status("Command session is no longer available");
                return;
            };
            let terminal_arc = std::sync::Arc::clone(&session.terminal);
            let policy = &session.projection_policy;
            let view_state = &mut session.projection_view_state;
            let mut terminal = terminal_arc.lock();
            let Some(anchor) = terminal
                .command_record(&target.execution_id)
                .map(|record| record.prompt_start)
            else {
                return self.set_status("Command position is no longer in scrollback");
            };
            let viewport = terminal.projected_viewport_with_state(
                crate::terminal::HistoryProjection::identity(),
                block_mode,
                policy,
                view_state,
            );
            // 块导航应该移动选择,而不是重新钉住视口:命令头已经在屏幕上时
            // 保留用户正在读的位置,与 `scroll_to_buffer_anchor` 的最小移动
            // 语义(以及 search reveal)一致。侧边栏点击行是明确的"带我过去"
            // 手势,仍然强制重新钉住。
            let minimal = reveal == BlockReveal::IfOffscreen;
            if viewport.is_transformed() {
                if minimal && Self::projected_anchor_is_displayed(&terminal, &viewport, anchor) {
                    true
                } else {
                    matches!(
                        terminal.reveal_buffer_anchor_in_projection(policy, view_state, anchor),
                        crate::terminal::ProjectedBufferAnchorLocation::Visible { .. }
                    )
                }
            } else if terminal.is_alt_buffer_active() {
                // `scroll_to_command` 的 alt-buffer 拒绝在这里显式保留。
                false
            } else if minimal && terminal.viewport_buffer_mapping_is_exact() {
                // `scroll_to_buffer_anchor` 的"已可见就不动"短路走的是原始行
                // 算术;reflow 之后可见行不再等于 scrollback 索引,那时它可能
                // 误判为"已经可见"而一动不动。映射不精确时退回强制跳转。
                terminal.scroll_to_buffer_anchor(anchor)
            } else {
                terminal.scroll_to_command(&target.execution_id)
            }
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

    /// Command/output for one block target, exactly as the copy commands see
    /// it: command authority always comes from the live record; a journal
    /// record whose full live identity was verified may fill output after the
    /// captured buffer has been evicted.
    /// The command snapshot keeps raw-presence/truncation provenance separate
    /// from its sanitized copy text. A present but unsafe/oversized command
    /// must never collapse into a background block merely because no safe
    /// command string can be returned.
    fn captured_block_text(
        &self,
        target: &CommandTarget,
        want_output: bool,
    ) -> Option<CapturedBlockText> {
        let live_captured = self
            .session_manager
            .sessions()
            .iter()
            .find(|session| session.metadata.session_id == target.session_id)
            .and_then(|session| {
                let terminal = session.terminal.lock();
                let record = terminal.command_record(&target.execution_id)?;
                let (command, command_present, command_exact) = captured_command_provenance(
                    record.command.as_deref(),
                    record.command_exact,
                    record.command_truncated,
                );
                let output = want_output
                    .then(|| {
                        terminal
                            .command_output_text(
                                &target.execution_id,
                                MAX_COMPLETED_COMMAND_OUTPUT_BYTES,
                            )
                            .map(|text| (text.text, text.truncated))
                    })
                    .flatten();
                Some(CapturedBlockText {
                    command,
                    command_present,
                    command_exact,
                    command_truncated: record.command_truncated,
                    output,
                })
            });
        let persisted_captured = self.persisted_sidebar_execution(target).map(|record| {
            let (command, command_present, command_exact) =
                captured_command_provenance(Some(&record.command), true, record.command_truncated);
            let output = want_output
                .then(|| {
                    record
                        .output
                        .as_ref()
                        .map(|output| (output.text.clone(), output.truncated))
                })
                .flatten();
            CapturedBlockText {
                command,
                command_present,
                command_exact,
                command_truncated: record.command_truncated,
                output,
            }
        });
        match (live_captured, persisted_captured) {
            (Some(mut live), Some(persisted)) => {
                if live.output.is_none() {
                    live.output = persisted.output;
                }
                Some(live)
            }
            (Some(captured), None) | (None, Some(captured)) => Some(captured),
            (None, None) => None,
        }
    }

    /// Take an owned provenance snapshot for an Agent task while the semantic
    /// record still exists. Live terminal state is authoritative; after full
    /// command/cwd/exit/duration verification, the bounded jsh journal may
    /// fill only an evicted output snapshot and non-authorizing timestamps.
    fn semantic_context_for_command(
        &self,
        target: &CommandTarget,
    ) -> Result<crate::agent::SemanticCommandContext, String> {
        let live = self
            .session_manager
            .sessions()
            .iter()
            .find(|session| session.metadata.session_id == target.session_id)
            .ok_or_else(|| "Command session is no longer available".to_string())
            .and_then(|session| {
                let terminal = session.terminal.lock();
                let record = terminal
                    .command_record(&target.execution_id)
                    .ok_or_else(|| "Command record is no longer available".to_string())?;
                let output = terminal
                    .command_output_text(&target.execution_id, MAX_COMPLETED_COMMAND_OUTPUT_BYTES);
                Ok(crate::agent::SemanticCommandContext {
                    source_session_id: target.session_id.clone(),
                    source_execution_id: target.execution_id.clone(),
                    source_sequence: record.sequence,
                    source_shell: session.shell.shell_program().map(str::to_string),
                    command: record.command.clone(),
                    command_exact: record.command_exact,
                    command_truncated: record.command_truncated,
                    cwd: record.cwd.clone(),
                    cwd_after: record.cwd_after.clone(),
                    exit_code: record.exit_code,
                    duration_ms: record.duration_ms,
                    output_text: output
                        .as_ref()
                        .map(|output| output.text.clone())
                        .unwrap_or_default(),
                    output_available: output.is_some(),
                    output_truncated: output.as_ref().is_some_and(|output| output.truncated),
                    output_total_bytes: output
                        .as_ref()
                        .map(|output| output.total_bytes)
                        .unwrap_or_default(),
                    started_at: record.started_at,
                    finished_at: record.finished_at,
                })
            })?;

        let Some(persisted) = self.persisted_sidebar_execution(target) else {
            return Ok(live);
        };
        let mut merged = live;
        let _ = enrich_semantic_context_from_history(&mut merged, persisted);
        Ok(merged)
    }

    fn start_agent_task_for_command(&mut self, target: &CommandTarget, intent: AgentTaskIntent) {
        // With the Tasks dashboard enabled, fixing a failed command takes the
        // provider-native path: first create the isolated worktree, then let
        // the user explicitly start Codex with the configured sharing policy.
        // Explain remains a read-only request in the legacy inline panel.
        let create_is_local_worktree =
            self.config.experimental_task_sidebar && intent == AgentTaskIntent::Fix;
        if !create_is_local_worktree && !self.config.ai_enabled {
            self.set_status_for(
                "Enable AI in Settings before creating an Agent task",
                Duration::from_secs(5),
            );
            return;
        }
        let lifecycle_health = self
            .session_manager
            .sessions()
            .iter()
            .find(|session| session.metadata.session_id == target.session_id)
            .and_then(|session| {
                let terminal = session.terminal.lock();
                terminal.command_record(&target.execution_id).map(|record| {
                    crate::block_mode::assess_lifecycle(
                        record.start_mark_seen,
                        record.completion_provenance,
                    )
                })
            });
        if !matches!(
            lifecycle_health,
            Some(
                crate::block_mode::BlockLifecycleHealth::Healthy
                    | crate::block_mode::BlockLifecycleHealth::Recovered
            )
        ) {
            self.set_status_for(
                "Agent execution actions require a trusted command lifecycle",
                Duration::from_secs(6),
            );
            return;
        }
        let semantic = match self.semantic_context_for_command(target) {
            Ok(context) => context,
            Err(error) => {
                self.set_status_for(error, Duration::from_secs(5));
                return;
            }
        };
        let is_failed = semantic.exit_code.is_some_and(|exit_code| {
            classify_completed(semantic.command.as_deref(), Some(exit_code)).is_failed()
        });
        if !is_failed {
            self.set_status("Agent tasks are available for failed commands");
            return;
        }
        if !semantic.command_exact {
            self.set_status("Exact command metadata is required for an Agent task");
            return;
        }
        let Some(recorded_cwd) = semantic.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty())
        else {
            self.set_status("Recorded command cwd is required for an Agent task");
            return;
        };
        let current_cwds = self
            .session_manager
            .sessions()
            .iter()
            .find(|session| session.metadata.session_id == target.session_id)
            .map(|session| {
                (
                    session.terminal.lock().current_working_dir.clone(),
                    jterm_core::process::process_cwd(session.get_shell_pid()),
                )
            });
        let Some((reported_cwd, process_cwd)) = current_cwds else {
            self.set_status_for(
                "The source terminal working directory is unavailable; wait for shell integration before starting an Agent task",
                Duration::from_secs(6),
            );
            return;
        };
        if !verified_local_command_cwd(
            recorded_cwd,
            reported_cwd.as_deref(),
            process_cwd.as_deref(),
        ) {
            self.set_status_for(
                "The source terminal cwd is not independently verified; return a local shell to the command's recorded cwd before starting an Agent task",
                Duration::from_secs(6),
            );
            return;
        }
        if create_is_local_worktree {
            match self.begin_command_worktree_task(semantic, crate::agent::AgentProvider::Codex) {
                Ok(()) => {}
                Err(error) => self.set_status_for(
                    format!("Could not create Agent task: {error}"),
                    Duration::from_secs(6),
                ),
            }
            return;
        }
        let prompt = match intent {
            // Command/output are untrusted PTY evidence and are already
            // framed by BlockContext. Never interpolate either into the task
            // instruction, where model-looking text could impersonate Ember.
            AgentTaskIntent::Fix => Some(
                "Fix the attached failed command. Diagnose the root cause, make only the necessary changes, and before completing rerun the exact validation command from the attached semantic context."
                    .to_string(),
            ),
            AgentTaskIntent::Explain => Some(
                "Explain the attached failed command: identify the root cause, cite the relevant evidence in its semantic output, and propose the smallest safe next step. Do not change files unless I ask."
                    .to_string(),
            ),
            AgentTaskIntent::Compose => None,
        };
        match self
            .agent_panel
            .start_for_block(&self.config, semantic, prompt)
        {
            Ok(()) => self.set_status(match intent {
                AgentTaskIntent::Fix => "Agent is working on the failed command",
                AgentTaskIntent::Explain => "Agent is explaining the failed command",
                AgentTaskIntent::Compose => {
                    "Created a fresh Agent task with the failed command attached"
                }
            }),
            Err(error) => self.set_status_for(
                format!("Could not start Agent task: {error}"),
                Duration::from_secs(5),
            ),
        }
    }

    fn copy_sidebar_command_text(&mut self, target: &CommandTarget, kind: CopyKind) {
        if self.target_session_index(target).is_none() {
            self.set_status("Command session is no longer available");
            return;
        }
        let want_output = !matches!(kind, CopyKind::Command);
        let Some(captured) = self.captured_block_text(target, want_output) else {
            self.set_status("Command record is no longer available");
            return;
        };

        let (text, truncated, label) = match kind {
            CopyKind::Command if !captured.command_exact || captured.command.is_none() => {
                self.set_status("Exact command text is unavailable");
                return;
            }
            CopyKind::Command => (
                captured.command.expect("guarded exact command"),
                false,
                "command",
            ),
            CopyKind::Output => match captured.output {
                Some((output, truncated)) if !output.is_empty() => {
                    (output, truncated, "command output")
                }
                _ => {
                    self.set_status("Command output is unavailable or empty");
                    return;
                }
            },
            CopyKind::Combined => {
                if !captured.command_exact || captured.command.is_none() {
                    self.set_status("Exact command text is unavailable");
                    return;
                }
                let Some((output, truncated)) = captured.output else {
                    self.set_status("Command output is unavailable");
                    return;
                };
                (
                    combine_command_and_output(
                        captured.command.as_deref().expect("guarded exact command"),
                        &output,
                    ),
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

    /// The current block selection as a sidebar-style target, or `None` when
    /// it dangles (session closed or record evicted) — never a panic.
    pub(crate) fn live_block_target(&self) -> Option<CommandTarget> {
        let selection = self.block_selection.as_ref()?;
        let session_id = &selection.session_id;
        let record_id = &selection.active_id;
        let session = self
            .session_manager
            .sessions()
            .iter()
            .find(|session| &session.metadata.session_id == session_id)?;
        session
            .terminal
            .lock()
            .command_record(record_id)
            .is_some_and(|record| record.complete)
            .then(|| CommandTarget {
                session_id: session_id.clone(),
                execution_id: record_id.clone(),
            })
    }

    /// Fallback target for `block:*` commands without a live selection: the
    /// most recent complete record of the active session that satisfies
    /// `wanted` (a command to copy/recall, or output to copy).
    fn latest_block_target(
        &mut self,
        wanted: impl Fn(&crate::terminal::CommandRecord) -> bool,
    ) -> Option<CommandTarget> {
        let session = self.session_manager.get_active_session_mut();
        let session_id = session.metadata.session_id.clone();
        let terminal = session.terminal.lock();
        terminal
            .command_records()
            .iter()
            .rev()
            .find(|record| record.complete && wanted(record))
            .map(|record| CommandTarget {
                session_id,
                execution_id: record.id.clone(),
            })
    }

    fn record_has_command(record: &crate::terminal::CommandRecord) -> bool {
        record.command_truncated
            || record
                .command
                .as_deref()
                .is_some_and(|command| !command.trim().is_empty())
    }

    fn record_has_output(record: &crate::terminal::CommandRecord) -> bool {
        record
            .captured_output
            .as_ref()
            .is_some_and(|output| !output.text.is_empty())
            || record
                .output_start
                .zip(record.output_end)
                .is_some_and(|(start, end)| end > start)
    }

    /// `block:jump_first_failed`: select and reveal the OLDEST FAILED block
    /// in the active session — the same `classify_outcome`-based failed set
    /// as the scrollbar markers and `block:jump_prev/next_failed`, so all
    /// failed-block features agree. Alt-screen is a silent no-op like the
    /// rest of keyboard block navigation.
    pub(crate) fn block_jump_first_failed(&mut self) {
        let Some(navigation) =
            self.block_navigation(|outcomes, _| crate::block_mode::oldest_failed_index(outcomes))
        else {
            return;
        };
        let Some(target) = navigation.target else {
            let message = self.explain_block_absence("No failed command in this session");
            self.set_status(message);
            return;
        };
        self.apply_block_selection(target);
    }

    /// Apply a mouse gesture to one immutable, completed history card. The
    /// renderer has already classified the hit zone, but record identity and
    /// completion are checked again here so a PTY update between paint and
    /// dispatch cannot retarget the click.
    pub(crate) fn apply_block_pointer_selection(
        &mut self,
        session_id: &str,
        record_id: &str,
        gesture: crate::block_mode::BlockSelectionGesture,
    ) {
        if !self.config.block_mode {
            self.clear_block_selection();
            return;
        }
        let Some(index) = self.session_manager.index_of(session_id) else {
            self.clear_block_selection_for_session(session_id);
            return;
        };
        let ordered_ids = {
            let Some(session) = self.session_manager.sessions().get(index) else {
                return;
            };
            let terminal = session.terminal.lock();
            if terminal.is_alt_buffer_active()
                || !terminal
                    .command_record(record_id)
                    .is_some_and(|record| record.complete)
            {
                return;
            }
            terminal
                .command_records()
                .iter()
                .filter(|record| record.complete)
                .map(|record| record.id.clone())
                .collect::<Vec<_>>()
        };
        if !ordered_ids.iter().any(|id| id == record_id) {
            return;
        }

        let current = self
            .block_selection
            .clone()
            .filter(|selection| selection.session_id == session_id);
        let selection = match gesture {
            crate::block_mode::BlockSelectionGesture::Plain => {
                Some(crate::block_mode::BlockSelection::single(
                    session_id.to_owned(),
                    record_id.to_owned(),
                ))
            }
            crate::block_mode::BlockSelectionGesture::Extend => {
                let mut selection = current.unwrap_or_else(|| {
                    crate::block_mode::BlockSelection::single(
                        session_id.to_owned(),
                        record_id.to_owned(),
                    )
                });
                selection.extend_to(&ordered_ids, record_id);
                Some(selection)
            }
            crate::block_mode::BlockSelectionGesture::Toggle => {
                if let Some(mut selection) = current {
                    selection
                        .toggle(&ordered_ids, record_id)
                        .then_some(selection)
                } else {
                    Some(crate::block_mode::BlockSelection::single(
                        session_id.to_owned(),
                        record_id.to_owned(),
                    ))
                }
            }
            crate::block_mode::BlockSelectionGesture::Activate => {
                let mut selection = current.unwrap_or_else(|| {
                    crate::block_mode::BlockSelection::single(
                        session_id.to_owned(),
                        record_id.to_owned(),
                    )
                });
                selection.activate(record_id);
                Some(selection)
            }
        };
        let Some(selection) = selection else {
            self.clear_block_selection_for_session(session_id);
            return;
        };
        let target = CommandTarget {
            session_id: session_id.to_owned(),
            execution_id: selection.active_id.clone(),
        };
        self.block_selection = Some(selection);
        self.sync_block_selection_to_sidebar(&target);
    }

    /// Dispatch a context-menu request against its stable clicked record. All
    /// clicked-only actions use `request.record_id`; batch actions use the
    /// current range only when it still contains that id.
    pub(crate) fn execute_block_menu_action(
        &mut self,
        session_id: &str,
        request: crate::block_mode::BlockMenuRequest,
    ) {
        let target_is_live_finished = self
            .session_manager
            .index_of(session_id)
            .and_then(|index| self.session_manager.sessions().get(index))
            .is_some_and(|session| {
                let terminal = session.terminal.lock();
                !terminal.is_alt_buffer_active()
                    && terminal
                        .command_record(&request.record_id)
                        .is_some_and(|record| record.complete)
            });
        if !target_is_live_finished {
            self.set_status("Command block is no longer available");
            return;
        }
        let target = CommandTarget {
            session_id: session_id.to_owned(),
            execution_id: request.record_id,
        };
        match request.action {
            crate::block_mode::BlockMenuAction::CopyCommands => {
                self.copy_block_context(&target, CopyKind::Command)
            }
            crate::block_mode::BlockMenuAction::AskAgent => {
                self.block_ask_agent_about_target(&target)
            }
            crate::block_mode::BlockMenuAction::CopyOutputs => {
                self.copy_block_context(&target, CopyKind::Output)
            }
            crate::block_mode::BlockMenuAction::CopyBlocks => {
                self.copy_block_context(&target, CopyKind::Combined)
            }
            crate::block_mode::BlockMenuAction::CopyMarkdown => {
                self.copy_block_context_markdown(&target)
            }
            crate::block_mode::BlockMenuAction::Reinput => self.block_reinput_selected_commands(),
            // 与 Commands 侧边栏的 "Run again" 共用同一条重放路径:命令
            // 权威性、只读任务终端、提示符/括号粘贴守卫和状态反馈都已经在
            // 那里实现,这里不重复判断。
            crate::block_mode::BlockMenuAction::Rerun => {
                self.replay_sidebar_command(&target, true, false)
            }
            crate::block_mode::BlockMenuAction::ScrollTop => {
                self.block_scroll_target_edge(&target, false)
            }
            crate::block_mode::BlockMenuAction::ScrollBottom => {
                self.block_scroll_target_edge(&target, true)
            }
            crate::block_mode::BlockMenuAction::Search => self.block_search_toggle(),
            crate::block_mode::BlockMenuAction::ToggleBookmark => {
                let _ = self.block_toggle_bookmark_target(&target);
            }
            crate::block_mode::BlockMenuAction::CopyJson => self.copy_block_json(&target),
            crate::block_mode::BlockMenuAction::CollapseOutput => {
                self.block_set_output_collapsed(&target, true)
            }
            crate::block_mode::BlockMenuAction::ExpandOutput => {
                self.block_set_output_collapsed(&target, false)
            }
        }
    }

    fn block_set_output_collapsed(&mut self, target: &CommandTarget, collapsed: bool) {
        let Some(index) = self.session_manager.index_of(&target.session_id) else {
            self.set_status("Command block is no longer available");
            return;
        };
        let Some(session) = self.session_manager.sessions_mut().get_mut(index) else {
            self.set_status("Command block is no longer available");
            return;
        };
        let terminal = std::sync::Arc::clone(&session.terminal);
        let sequence = {
            let terminal = terminal.lock();
            let Some(record) = terminal
                .command_record(&target.execution_id)
                .filter(|record| record.complete)
            else {
                self.set_status("Command block is no longer available");
                return;
            };
            if collapsed && terminal.finished_output_range(record.sequence).is_none() {
                self.set_status("This block has no exact retained output to collapse");
                return;
            }
            record.sequence
        };
        let changed = if collapsed {
            session.projection_policy.collapse(sequence)
        } else {
            session.projection_policy.expand(sequence)
        };
        if changed {
            terminal.lock().clear_text_selection();
            self.smooth_scroll_velocity = 0.0;
            self.smooth_scroll_pixel_offset = 0.0;
            self.set_status(if collapsed {
                "Collapsed block output"
            } else {
                "Expanded block output"
            });
            if self.search_state.is_open {
                self.reveal_current_search_match();
            }
        }
    }

    fn block_scroll_target_edge(&mut self, target: &CommandTarget, bottom: bool) {
        let block_mode = self.config.block_mode;
        let jumped = self
            .target_session_index(target)
            .and_then(|index| self.session_manager.sessions_mut().get_mut(index))
            .is_some_and(|session| {
                let terminal_arc = std::sync::Arc::clone(&session.terminal);
                let policy = &session.projection_policy;
                let view_state = &mut session.projection_view_state;
                let mut terminal = terminal_arc.lock();
                let viewport = terminal.projected_viewport_with_state(
                    crate::terminal::HistoryProjection::identity(),
                    block_mode,
                    policy,
                    view_state,
                );
                if !viewport.is_transformed() {
                    return terminal.scroll_to_command_edge(&target.execution_id, bottom);
                }
                let Some(anchor) = terminal.command_edge_anchor(&target.execution_id, bottom)
                else {
                    return false;
                };
                match terminal.reveal_buffer_anchor_in_projection(policy, view_state, anchor) {
                    crate::terminal::ProjectedBufferAnchorLocation::Visible { .. } => true,
                    crate::terminal::ProjectedBufferAnchorLocation::Hidden { zone_id }
                        if bottom && policy.is_collapsed(zone_id) =>
                    {
                        terminal.reveal_collapsed_summary(policy, view_state, zone_id)
                    }
                    _ => false,
                }
            });
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

    pub(crate) fn block_scroll_selected_edge(&mut self, bottom: bool) -> bool {
        let Some(target) = self.live_block_target() else {
            return false;
        };
        self.block_scroll_target_edge(&target, bottom);
        true
    }

    fn toggle_resolved_block_bookmark(
        &mut self,
        session_id: &str,
        sequence: u64,
        version: crate::block_search::RetainedRecordVersion,
    ) -> bool {
        let added = self.block_bookmarks.toggle(session_id, sequence, version);
        self.set_status(if added {
            "Bookmarked command block"
        } else {
            "Removed command block bookmark"
        });
        added
    }

    fn block_toggle_bookmark_target(&mut self, target: &CommandTarget) -> Option<bool> {
        let identity = self
            .target_session_index(target)
            .and_then(|index| self.session_manager.sessions().get(index))
            .and_then(|session| {
                let terminal = session.terminal.lock();
                let records = terminal.command_records();
                let sequence = terminal
                    .command_record(&target.execution_id)
                    .filter(|record| record.complete)
                    .map(|record| record.sequence)?;
                Some((sequence, retained_record_version(records)))
            });
        let Some((sequence, version)) = identity else {
            self.set_status("That block is no longer retained");
            return None;
        };
        Some(self.toggle_resolved_block_bookmark(&target.session_id, sequence, version))
    }

    pub(crate) fn block_toggle_bookmark(&mut self) {
        let Some(target) = self.live_block_target().filter(|target| {
            self.target_session_index(target)
                .and_then(|index| self.session_manager.sessions().get(index))
                .is_some_and(|session| {
                    session
                        .terminal
                        .lock()
                        .command_record(&target.execution_id)
                        .is_some_and(|record| record.complete)
                })
        }) else {
            self.set_status("Select a completed command block to bookmark");
            return;
        };
        let _ = self.block_toggle_bookmark_target(&target);
    }

    /// Toggle the record behind one picker hit without activating or closing
    /// the row. Every path validates the live record id and resolves it to the
    /// terminal-owned sequence before touching bookmark truth.
    pub(crate) fn block_search_toggle_bookmark(
        &mut self,
        target: Option<crate::block_search::BlockSearchBookmarkTarget>,
    ) -> bool {
        if !self.block_search.is_open {
            return false;
        }
        if self.block_search.query_error.is_some() {
            self.set_status("Fix the search query before bookmarking a result");
            return false;
        }
        let Some(target) = target else {
            self.block_search.computed_query = None;
            self.refresh_block_search_hits();
            self.set_status(if self.block_search.hits.is_empty() {
                "No search result is selected"
            } else {
                "Block search is refreshing; choose the result again"
            });
            return false;
        };
        let active_session_id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        let identity = if active_session_id == target.session_id
            && self.block_search.contains_bookmark_target(&target)
        {
            self.session_manager
                .index_of(&target.session_id)
                .and_then(|index| self.session_manager.sessions().get(index))
                .and_then(|session| {
                    let terminal = session.terminal.lock();
                    let records = terminal.command_records();
                    if block_search_record_version(records) != target.record_version {
                        return None;
                    }
                    let sequence = terminal
                        .command_record(&target.record_id)
                        .filter(|record| record.complete)
                        .map(|record| record.sequence)?;
                    Some((sequence, retained_record_version(records)))
                })
        } else {
            None
        };
        let Some((sequence, version)) = identity else {
            self.block_search.computed_query = None;
            self.refresh_block_search_hits();
            self.set_status("That search result is no longer retained");
            return false;
        };
        self.toggle_resolved_block_bookmark(&target.session_id, sequence, version);
        {
            // Bookmark revision forces an anchor-preserving re-filter of the
            // existing bounded cache. Under Bookmarked, removing the selected
            // record therefore lands on the closest surviving old rank.
            self.block_search.needs_focus = true;
            self.refresh_block_search_hits();
        }
        true
    }

    /// Prune only after the retained `command_records` deque identity changes.
    /// The O(1) gate keeps static frames cheap; a changed deque then scans at
    /// most the bounded semantic history. Captured-output or scrollback
    /// eviction leaves this version unchanged and cannot erase bookmarks.
    pub(crate) fn prune_block_bookmarks_to_retained_records(&mut self) -> bool {
        let mut changed = false;
        for session_id in self.block_bookmarks.session_ids() {
            let Some(session) = self
                .session_manager
                .sessions()
                .iter()
                .find(|session| session.metadata.session_id == session_id)
            else {
                changed |= self.block_bookmarks.remove_session(&session_id);
                continue;
            };
            let terminal = session.terminal.lock();
            let records = terminal.command_records();
            let version = retained_record_version(records);
            if !self.block_bookmarks.needs_prune(&session_id, version) {
                continue;
            }
            let live_complete_sequences = records
                .iter()
                .filter(|record| record.complete)
                .map(|record| record.sequence)
                .collect::<std::collections::HashSet<_>>();
            drop(terminal);
            changed |=
                self.block_bookmarks
                    .retain_live(&session_id, version, &live_complete_sequences);
        }
        changed
    }

    pub(crate) fn block_jump_bookmark(&mut self, step: crate::block_mode::SelectStep) {
        let session = self.session_manager.get_active_session_mut();
        let session_id = session.metadata.session_id.clone();
        let terminal = session.terminal.lock();
        if terminal.is_alt_buffer_active() {
            return;
        }
        let records = terminal.command_records();
        let valid_records = records
            .iter()
            .filter(|record| record.complete)
            .map(|record| (record.sequence, record.id.clone()))
            .collect::<Vec<_>>();
        let current = self
            .block_selection
            .as_ref()
            .filter(|selection| selection.session_id == session_id)
            .and_then(|selection| {
                valid_records
                    .iter()
                    .position(|(_, id)| id == &selection.active_id)
            });
        let mut bookmarked_indices = self
            .block_bookmarks
            .get(&session_id)
            .map(|bookmarks| {
                valid_records
                    .iter()
                    .enumerate()
                    .filter_map(|(index, (sequence, _))| {
                        bookmarks.contains(sequence).then_some(index)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let retained_version = retained_record_version(records);
        let retained_sequences = valid_records
            .iter()
            .map(|(sequence, _)| *sequence)
            .collect::<std::collections::HashSet<_>>();
        drop(terminal);
        self.block_bookmarks
            .retain_live(&session_id, retained_version, &retained_sequences);
        if bookmarked_indices.is_empty() {
            let message =
                self.explain_block_absence("No bookmarked command blocks in this session");
            self.set_status(message);
            return;
        }
        bookmarked_indices.sort_unstable();
        let target_index = match step {
            crate::block_mode::SelectStep::Older => current
                .and_then(|current| {
                    bookmarked_indices
                        .iter()
                        .rev()
                        .copied()
                        .find(|index| *index < current)
                })
                .unwrap_or_else(|| *bookmarked_indices.last().expect("non-empty bookmarks")),
            crate::block_mode::SelectStep::Newer => current
                .and_then(|current| {
                    bookmarked_indices
                        .iter()
                        .copied()
                        .find(|index| *index > current)
                })
                .unwrap_or(bookmarked_indices[0]),
        };
        let Some((_, record_id)) = valid_records.get(target_index).cloned() else {
            return;
        };
        self.apply_block_selection(CommandTarget {
            session_id,
            execution_id: record_id,
        });
    }

    fn block_ask_agent_about_target(&mut self, target: &CommandTarget) {
        if !self.config.ai_enabled {
            self.set_status_for(
                "Enable AI in Settings before asking about a block",
                Duration::from_secs(5),
            );
            return;
        }
        let semantic = match self.semantic_context_for_command(target) {
            Ok(semantic) => semantic,
            Err(error) => {
                self.set_status_for(error, Duration::from_secs(5));
                return;
            }
        };
        if let Some(reason) = crate::agent::context::block_agent_context_disabled_reason(
            semantic.command.as_deref(),
            semantic.command_exact,
            semantic.command_truncated,
            semantic.cwd.as_deref(),
            Some(semantic.output_available),
        ) {
            self.set_status_for(reason, Duration::from_secs(5));
            return;
        };
        match self
            .agent_panel
            .start_for_block(&self.config, semantic, None)
        {
            Ok(()) => self.set_status("Created an Agent task with the block attached"),
            Err(error) => self.set_status_for(
                format!("Could not start Agent task: {error}"),
                Duration::from_secs(5),
            ),
        }
    }

    /// Resolve the clicked card's effective selection in terminal order. A
    /// dangling member rejects the whole batch instead of silently copying a
    /// different subset after history eviction.
    fn block_context_targets(
        &self,
        clicked: &CommandTarget,
    ) -> Result<Vec<CommandTarget>, &'static str> {
        let Some(index) = self.target_session_index(clicked) else {
            return Err("Command session is no longer available");
        };
        let Some(session) = self.session_manager.sessions().get(index) else {
            return Err("Command session is no longer available");
        };
        let terminal = session.terminal.lock();
        if terminal.is_alt_buffer_active()
            || !terminal
                .command_record(&clicked.execution_id)
                .is_some_and(|record| record.complete)
        {
            return Err("Command block is no longer available");
        }
        let selected = self.block_selection.as_ref().filter(|selection| {
            selection.session_id == clicked.session_id
                && selection
                    .selected_ids
                    .iter()
                    .any(|id| id == &clicked.execution_id)
        });
        let wanted = selected
            .map(|selection| selection.selected_ids.as_slice())
            .unwrap_or(std::slice::from_ref(&clicked.execution_id));
        let targets = terminal
            .command_records()
            .iter()
            .filter(|record| record.complete && wanted.iter().any(|id| id == &record.id))
            .map(|record| CommandTarget {
                session_id: clicked.session_id.clone(),
                execution_id: record.id.clone(),
            })
            .collect::<Vec<_>>();
        if targets.len() != wanted.len() {
            return Err("A selected command block is no longer available");
        }
        Ok(targets)
    }

    fn copy_block_context(&mut self, clicked: &CommandTarget, kind: CopyKind) {
        match self.block_context_targets(clicked) {
            Ok(targets) => self.copy_block_targets(&targets, kind),
            Err(error) => self.set_status(error),
        }
    }

    fn copy_block_context_markdown(&mut self, clicked: &CommandTarget) {
        match self.block_context_targets(clicked) {
            Ok(targets) => self.copy_block_targets_markdown(&targets),
            Err(error) => self.set_status(error),
        }
    }

    /// Selection-aware target resolution for keyboard commands. A selection
    /// in the active session is authoritative and copied old-to-new; no
    /// selection falls back to the newest completed matching record.
    fn block_targets_or_newest(
        &mut self,
        wanted: impl Fn(&crate::terminal::CommandRecord) -> bool,
        missing: &str,
    ) -> Option<Vec<CommandTarget>> {
        let active_session_id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        let selected_ids = self
            .block_selection
            .as_ref()
            .filter(|selection| selection.session_id == active_session_id)
            .map(|selection| selection.selected_ids.clone());
        if let Some(selected_ids) = selected_ids {
            let targets = {
                let session = self.session_manager.get_active_session_mut();
                let terminal = session.terminal.lock();
                terminal
                    .command_records()
                    .iter()
                    .filter(|record| {
                        record.complete && selected_ids.iter().any(|id| id == &record.id)
                    })
                    .map(|record| CommandTarget {
                        session_id: active_session_id.clone(),
                        execution_id: record.id.clone(),
                    })
                    .collect::<Vec<_>>()
            };
            if targets.len() != selected_ids.len() {
                self.set_status("A selected command block is no longer available");
                return None;
            }
            return Some(targets);
        }
        let target = self.latest_block_target(wanted);
        if target.is_none() {
            let message = self.explain_block_absence(missing);
            self.set_status(message);
        }
        target.map(|target| vec![target])
    }

    fn report_block_clipboard_build_error(&mut self, error: BlockClipboardBuildError) {
        self.set_status_for(
            match error {
                BlockClipboardBuildError::TooLarge => {
                    "Selected blocks exceed the 32 MiB clipboard limit"
                }
                BlockClipboardBuildError::Allocation => {
                    "Could not allocate the bounded block clipboard payload"
                }
            },
            Duration::from_secs(5),
        );
    }

    fn copy_block_targets(&mut self, targets: &[CommandTarget], kind: CopyKind) {
        let separator = if matches!(kind, CopyKind::Command) {
            "\n"
        } else {
            "\n\n"
        };
        let mut text = String::new();
        let mut count = 0usize;
        let mut any_truncated = false;
        for target in targets {
            let Some(captured) =
                self.captured_block_text(target, !matches!(kind, CopyKind::Command))
            else {
                self.set_status("A selected command block is no longer available");
                return;
            };
            let background = captured.is_background();
            let part = match kind {
                CopyKind::Command => {
                    if background {
                        None
                    } else {
                        if !captured.command_exact || captured.command.is_none() {
                            self.set_status(
                                "Exact command text is unavailable for part of the selection",
                            );
                            return;
                        }
                        captured.command
                    }
                }
                CopyKind::Output => captured.output.filter(|(text, _)| !text.is_empty()).map(
                    |(output, truncated)| {
                        any_truncated |= truncated;
                        output
                    },
                ),
                CopyKind::Combined => {
                    if !background && (!captured.command_exact || captured.command.is_none()) {
                        self.set_status(
                            "Exact command text is unavailable for part of the selection",
                        );
                        return;
                    }
                    match (background, captured.command, captured.output) {
                        (false, Some(command), Some((output, truncated))) => {
                            any_truncated |= truncated;
                            Some(combine_command_and_output(&command, &output))
                        }
                        (false, Some(command), None) => Some(command),
                        (true, _, Some((output, truncated))) if !output.is_empty() => {
                            any_truncated |= truncated;
                            Some(output)
                        }
                        _ => None,
                    }
                }
            };
            let Some(part) = part else {
                continue;
            };
            if let Err(error) =
                append_bounded_block_part(&mut text, &part, separator, MAX_BLOCK_CLIPBOARD_BYTES)
            {
                self.report_block_clipboard_build_error(error);
                return;
            }
            count += 1;
        }
        if count == 0 {
            self.set_status(match kind {
                CopyKind::Command => "The selected blocks contain no commands",
                CopyKind::Output => "The selected blocks contain no captured output",
                CopyKind::Combined => "The selected blocks contain no copyable text",
            });
            return;
        }
        let result = self
            .clipboard
            .as_ref()
            .map(|clipboard| clipboard.copy(&text));
        match result {
            Some(Ok(())) => self.set_status(format!(
                "Copied {count} block{}{}",
                if count == 1 { "" } else { "s" },
                if any_truncated {
                    " (truncated output)"
                } else {
                    ""
                }
            )),
            Some(Err(error)) => {
                self.set_status_for(format!("Copy failed: {error}"), Duration::from_secs(4))
            }
            None => self.set_status("Clipboard is unavailable"),
        }
    }

    fn block_markdown_for_target(&self, target: &CommandTarget) -> Result<String, &'static str> {
        let Some(captured) = self.captured_block_text(target, true) else {
            return Err("Command block is no longer available");
        };
        let background = captured.is_background();
        let (exit_code, duration_ms, finished_secs, cwd, start_mark_seen, provenance) = self
            .session_manager
            .sessions()
            .iter()
            .find(|session| session.metadata.session_id == target.session_id)
            .and_then(|session| {
                let terminal = session.terminal.lock();
                terminal.command_record(&target.execution_id).map(|record| {
                    (
                        record.exit_code,
                        record.duration_ms,
                        record.finished_at.and_then(crate::block_mode::epoch_secs),
                        record.cwd.clone(),
                        record.start_mark_seen,
                        record.completion_provenance,
                    )
                })
            })
            .or_else(|| {
                self.persisted_sidebar_execution(target).map(|record| {
                    (
                        record.exit_code,
                        record.duration_ms,
                        record.ended_at_ms.map(|ms| ms / 1000),
                        (!record.cwd.is_empty()).then(|| record.cwd.clone()),
                        false,
                        crate::block_mode::CompletionProvenance::JournalRecovered,
                    )
                })
            })
            .unwrap_or((
                None,
                None,
                None,
                None,
                false,
                crate::block_mode::CompletionProvenance::Unknown,
            ));
        let finished = finished_secs.map(|secs| {
            crate::block_mode::format_local_datetime(
                secs,
                crate::block_mode::local_utc_offset_secs(secs),
            )
        });
        let (output, output_truncated) = captured.output.unwrap_or_default();
        let command = (!captured.command_truncated)
            .then_some(captured.command.as_deref())
            .flatten();
        Ok(crate::block_mode::block_markdown_with_lifecycle(
            &crate::block_mode::MarkdownBlock {
                command,
                command_exact: captured.command_exact,
                command_omitted: !background && command.is_none(),
                command_truncated: captured.command_truncated,
                output: &output,
                output_truncated,
                exit_code,
                duration_ms,
                finished: finished.as_deref(),
                cwd: cwd.as_deref(),
            },
            start_mark_seen,
            provenance,
        ))
    }

    fn block_json_for_target(&self, target: &CommandTarget) -> Result<String, &'static str> {
        let Some(captured) = self.captured_block_text(target, true) else {
            return Err("Command block is no longer available");
        };
        let metadata = self
            .session_manager
            .sessions()
            .iter()
            .find(|session| session.metadata.session_id == target.session_id)
            .and_then(|session| {
                let terminal = session.terminal.lock();
                terminal.command_record(&target.execution_id).map(|record| {
                    (
                        record.sequence,
                        record.cwd.clone(),
                        record.exit_code,
                        record.duration_ms,
                        record
                            .started_at
                            .and_then(crate::block_mode::epoch_secs)
                            .map(|seconds| seconds.saturating_mul(1_000)),
                        record
                            .finished_at
                            .and_then(crate::block_mode::epoch_secs)
                            .map(|seconds| seconds.saturating_mul(1_000)),
                        terminal.grid.row_len(),
                        record.start_mark_seen,
                        record.completion_provenance,
                    )
                })
            })
            .ok_or("Command block is no longer available")?;
        let background = captured.is_background();
        let (output, output_truncated) = captured.output.unwrap_or_default();
        let command = (!captured.command_truncated)
            .then_some(captured.command)
            .flatten();
        let command_omitted = !background && command.is_none();
        let value = serde_json::json!({
            "id": target.execution_id,
            "sequence": metadata.0,
            "kind": if background { "background" } else { "command" },
            "prompt": serde_json::Value::Null,
            "cmd": command,
            "command_present": captured.command_present,
            "command_omitted": command_omitted,
            "command_exact": captured.command_exact,
            "command_truncated": captured.command_truncated,
            "output": output,
            "output_truncated": output_truncated,
            "exit_code": if background { None } else { metadata.2 },
            "duration_ms": if background { None } else { metadata.3 },
            "start_time_ms": metadata.4,
            "end_time_ms": metadata.5,
            "cwd": metadata.1,
            "cols": metadata.6,
            "completion_provenance": crate::block_mode::completion_provenance_schema_name(metadata.8),
            "lifecycle_health": crate::block_mode::lifecycle_health_schema_name(
                crate::block_mode::assess_lifecycle(metadata.7, metadata.8),
            ),
        });
        serde_json::to_string_pretty(&value).map_err(|_| "Could not serialize command block")
    }

    fn copy_block_json(&mut self, target: &CommandTarget) {
        let json = match self.block_json_for_target(target) {
            Ok(json) => json,
            Err(error) => {
                self.set_status(error);
                return;
            }
        };
        let result = self
            .clipboard
            .as_ref()
            .map(|clipboard| clipboard.copy(&json));
        match result {
            Some(Ok(())) => self.set_status("Copied block as JSON"),
            Some(Err(error)) => {
                self.set_status_for(format!("Copy failed: {error}"), Duration::from_secs(4))
            }
            None => self.set_status("Clipboard is unavailable"),
        }
    }

    fn copy_block_targets_markdown(&mut self, targets: &[CommandTarget]) {
        let mut aggregate = String::new();
        let mut count = 0usize;
        for target in targets {
            match self.block_markdown_for_target(target) {
                Ok(markdown) => {
                    if let Err(error) = append_bounded_block_part(
                        &mut aggregate,
                        &markdown,
                        "\n\n---\n\n",
                        MAX_BLOCK_CLIPBOARD_BYTES,
                    ) {
                        self.report_block_clipboard_build_error(error);
                        return;
                    }
                    count += 1;
                }
                Err(error) => {
                    self.set_status(error);
                    return;
                }
            }
        }
        if count == 0 {
            self.set_status("No command block to copy");
            return;
        }
        let result = self
            .clipboard
            .as_ref()
            .map(|clipboard| clipboard.copy(&aggregate));
        match result {
            Some(Ok(())) => self.set_status(format!(
                "Copied {count} block{} as Markdown",
                if count == 1 { "" } else { "s" }
            )),
            Some(Err(error)) => {
                self.set_status_for(format!("Copy failed: {error}"), Duration::from_secs(4))
            }
            None => self.set_status("Clipboard is unavailable"),
        }
    }

    /// `block:copy_command`: copy the selected block's command; with no
    /// selection, the most recent complete record with one.
    pub(crate) fn block_copy_command(&mut self) {
        let Some(targets) =
            self.block_targets_or_newest(Self::record_has_command, "No command block to copy from")
        else {
            return;
        };
        self.copy_block_targets(&targets, CopyKind::Command);
    }

    /// `block:copy_output`: copy the selected block's output (captured text,
    /// or extracted from its output anchors); with no selection, the most
    /// recent complete record with output.
    pub(crate) fn block_copy_output(&mut self) {
        let Some(targets) = self.block_targets_or_newest(
            Self::record_has_output,
            "No command block with output to copy",
        ) else {
            return;
        };
        self.copy_block_targets(&targets, CopyKind::Output);
    }

    /// `block:recall_command`: insert (never execute) the selected/latest
    /// command at the prompt, through the sidebar Fill action's prompt-ready
    /// guard machinery.
    pub(crate) fn block_recall_command(&mut self) {
        let Some(target) =
            self.block_target_or_newest(Self::record_has_command, "No command block to recall")
        else {
            return;
        };
        self.replay_sidebar_command(&target, false, true);
    }

    /// `block:select_prev`: move the block selection to the next-older
    /// selectable block (or start at the newest when nothing is selected).
    pub(crate) fn block_select_prev(&mut self) {
        self.block_select_step(crate::block_mode::SelectStep::Older);
    }

    /// `block:select_next`: move the block selection to the next-newer
    /// selectable block (or start at the newest when nothing is selected).
    pub(crate) fn block_select_next(&mut self) {
        self.block_select_step(crate::block_mode::SelectStep::Newer);
    }

    /// `block:select_all`: select every completed block in the active pane.
    /// The oldest id is the fixed range anchor and the newest id is the active
    /// edge, so the first Shift+Up contracts the range exactly like anvil.
    pub(crate) fn block_select_all(&mut self) {
        let Some((session_id, ids)) = self.selectable_block_ids() else {
            return;
        };
        let count = ids.len();
        let Some(selection) = crate::block_mode::BlockSelection::all(session_id.clone(), ids)
        else {
            let message = self.explain_block_absence("No completed command blocks to select");
            self.set_status(message);
            return;
        };
        let target = CommandTarget {
            session_id,
            execution_id: selection.active_id.clone(),
        };
        self.block_selection = Some(selection);
        self.sync_block_selection_to_sidebar(&target);
        self.reveal_sidebar_command(&target, BlockReveal::IfOffscreen);
        self.set_status(format!("Selected {count} command blocks"));
    }

    /// `block:reinput_selected_commands`: put every selected real command back
    /// into the live editor, in terminal order, as one bracketed-paste frame.
    /// Background blocks contribute no empty line. The operation is atomic:
    /// one stale/inexact/unsafe record rejects the whole selection.
    pub(crate) fn block_reinput_selected_commands(&mut self) {
        match self.try_reinput_selected_commands() {
            Ok(count) => self.set_status(format!(
                "Filled {count} selected command{} at prompt",
                if count == 1 { "" } else { "s" }
            )),
            Err(error) => self.report_selected_replay_error(error),
        }
    }

    /// Enter is context-sensitive: only consume it after bytes were safely
    /// written at an idle prompt. A running program (including `read`) and a
    /// selection containing only background output must receive Enter normally.
    pub(crate) fn block_reinput_selected_commands_from_enter(&mut self) -> bool {
        match self.try_reinput_selected_commands() {
            Ok(count) => {
                self.set_status(format!(
                    "Filled {count} selected command{} at prompt",
                    if count == 1 { "" } else { "s" }
                ));
                true
            }
            // These states genuinely belong to the child: a foreground/read
            // program needs Enter, and background-only selection has nothing
            // to insert. Every other idle-prompt failure is consumed and
            // surfaced; forwarding it could submit text the user did not mean
            // to run after the reviewed selection failed validation.
            Err(
                SelectedReplayError::NoSelection
                | SelectedReplayError::NoCommands
                | SelectedReplayError::NotPromptReady
                | SelectedReplayError::AlternateScreen,
            ) => false,
            Err(error) => {
                self.report_selected_replay_error(error);
                true
            }
        }
    }

    fn try_reinput_selected_commands(&mut self) -> Result<usize, SelectedReplayError> {
        if !self.config.block_mode {
            self.clear_block_selection();
            return Err(SelectedReplayError::NoSelection);
        }
        let active_index = self.session_manager.active_index();
        let active_session_id = self
            .session_manager
            .sessions()
            .get(active_index)
            .map(|session| session.metadata.session_id.clone())
            .ok_or(SelectedReplayError::NoSelection)?;
        let selection = self
            .block_selection
            .clone()
            .filter(|selection| selection.session_id == active_session_id)
            .ok_or(SelectedReplayError::NoSelection)?;
        let direct_input_blocked = self.direct_input_is_blocked_for_session(&active_session_id);

        let result = {
            let session = self
                .session_manager
                .get_session_mut(active_index)
                .ok_or(SelectedReplayError::NoSelection)?;
            let (command, count) = {
                let terminal = session.terminal.lock();
                let guard = SelectedReplayGuard {
                    alternate_screen: terminal.is_alt_buffer_active(),
                    prompt_ready: terminal.shell_is_prompt_ready(),
                    bracketed_paste: terminal.is_bracketed_paste_enabled(),
                    pending_input: direct_input_blocked || !session.pending_input.is_empty(),
                    prompt_empty: terminal.prompt_input_is_empty(),
                };
                prepare_selected_replay(guard, || {
                    selected_commands_in_terminal_order(
                        terminal
                            .command_records()
                            .iter()
                            .map(|record| SelectedReplayRecord {
                                id: &record.id,
                                command: record.command.as_deref(),
                                exact: record.command_exact,
                                truncated: record.command_truncated,
                                complete: record.complete,
                            }),
                        &selection.selected_ids,
                        crate::review_text::MAX_PROMPT_INSERT_BYTES,
                    )
                })?
            };
            // Every constituent command was already sanitized above; do not
            // run the single-record 64-KiB validator over the combined buffer
            // and accidentally undercut MAX_PROMPT_INSERT_BYTES.
            let payload = replay_prepared_payload(&command, false);
            session
                .shell
                .write(&payload)
                .map_err(SelectedReplayError::WriteFailed)?;
            let mut terminal = session.terminal.lock();
            terminal.note_user_input(&payload);
            terminal.scroll_to_bottom();
            drop(terminal);
            session.projection_view_state.scroll_to_bottom();
            Ok(count)
        };

        if result.is_ok() {
            self.clear_block_selection();
        }
        result
    }

    fn report_selected_replay_error(&mut self, error: SelectedReplayError) {
        let message = match error {
            SelectedReplayError::NoSelection => "No command blocks are selected".to_string(),
            SelectedReplayError::MissingRecord => {
                "A selected command block is no longer available".to_string()
            }
            SelectedReplayError::ExactCommandUnavailable => {
                "Exact command text is unavailable for part of the selection".to_string()
            }
            SelectedReplayError::NoCommands => {
                "The selected blocks contain no commands to reinput".to_string()
            }
            SelectedReplayError::NotPromptReady => {
                "Wait for the shell prompt before reinputting commands".to_string()
            }
            SelectedReplayError::AlternateScreen => {
                "Cannot reinput commands while an alternate-screen app is open".to_string()
            }
            SelectedReplayError::BracketedPasteDisabled => {
                "Safe multi-command replay requires bracketed-paste mode".to_string()
            }
            SelectedReplayError::PendingInput => {
                "Wait for pending terminal input to be delivered".to_string()
            }
            SelectedReplayError::PromptNotEmpty => {
                "Clear the current prompt before inserting selected commands".to_string()
            }
            SelectedReplayError::UnsafeCommand(error) => {
                format!("Command replay rejected: {error}")
            }
            SelectedReplayError::TooLarge { limit } => {
                format!("Selected commands exceed the {limit}-byte replay limit")
            }
            SelectedReplayError::WriteFailed(error) => {
                format!("Command replay failed: {error}")
            }
        };
        self.set_status_for(message, Duration::from_secs(5));
    }

    /// Context-sensitive Ctrl+Up/Down behavior shared with anvil/forge.
    /// Ctrl+Up starts at the newest block when no range exists; Ctrl+Down with
    /// no range keeps its legacy small-scroll behavior. Once selected, either
    /// direction moves/collapses the active edge, and moving newer past the
    /// newest block exits selection so a subsequent Ctrl+Down can scroll — but
    /// a multi-block range collapses onto the newest block first, so one stray
    /// step never discards the whole range (see `block_mode::block_step`).
    pub(crate) fn block_context_scroll(&mut self, step: crate::block_mode::SelectStep) -> bool {
        if !self.config.block_mode {
            self.clear_block_selection();
            return false;
        }
        if self.block_move_selection(step, false) {
            return true;
        }
        if step == crate::block_mode::SelectStep::Newer {
            return false;
        }
        let Some(navigation) = self.block_navigation(|outcomes, current| {
            crate::block_mode::next_selected_index(outcomes, current, step)
        }) else {
            return false;
        };
        let Some(target) = navigation.target else {
            return false;
        };
        self.apply_block_selection(target);
        true
    }

    /// Move an existing active edge by one block. Plain movement collapses a
    /// range to the target; Shift movement keeps the anchor and rebuilds the
    /// inclusive range. Returns false when no visible/live selection owns the
    /// key, allowing the arrow/Enter to continue to the PTY.
    pub(crate) fn block_move_selection(
        &mut self,
        step: crate::block_mode::SelectStep,
        extend: bool,
    ) -> bool {
        let Some((session_id, ordered_ids)) = self.selectable_block_ids() else {
            return false;
        };
        let Some(mut selection) = self
            .block_selection
            .clone()
            .filter(|selection| selection.session_id == session_id)
        else {
            return false;
        };
        let Some(current) = ordered_ids.iter().position(|id| id == &selection.active_id) else {
            // An evicted id must not strand arrow keys in application state.
            self.clear_block_selection();
            return false;
        };

        let target_index = match crate::block_mode::block_step(
            current,
            ordered_ids.len(),
            selection.selected_ids.len(),
            step,
            extend,
        ) {
            crate::block_mode::BlockStep::To(index) => index,
            crate::block_mode::BlockStep::Exit => {
                self.clear_block_selection();
                // 这个按键被消费掉了:不给反馈的话,用户只会看到选择凭空消失。
                self.set_status("Block selection cleared");
                return true;
            }
        };
        let target_id = ordered_ids[target_index].clone();
        let mut extended_count = None;
        if extend {
            selection.extend_to(&ordered_ids, &target_id);
            // 卡片轮廓已经画出了范围,但一次 Shift 步进只改变一条边;把总数
            // 说出来,用户才知道 Enter/复制到底会作用在几个块上。单块时不
            // 提示——那会让每一次 Ctrl+↑ 都变成噪音。
            if selection.selected_ids.len() > 1 {
                extended_count = Some(selection.selected_ids.len());
            }
        } else {
            selection =
                crate::block_mode::BlockSelection::single(session_id.clone(), target_id.clone());
        }
        let target = CommandTarget {
            session_id,
            execution_id: target_id,
        };
        self.block_selection = Some(selection);
        self.sync_block_selection_to_sidebar(&target);
        if let Some(count) = extended_count {
            self.set_status(format!("Selected {count} command blocks"));
        }
        // 放在计数提示之后:跳转失败的告警必须盖住"选中了 N 个块"。
        self.reveal_sidebar_command(&target, BlockReveal::IfOffscreen);
        true
    }

    /// Snapshot immutable, completed block ids in terminal order. Live prompt
    /// and running rows always remain PTY-owned and can never enter a block
    /// selection through keyboard navigation or range extension.
    fn selectable_block_ids(&mut self) -> Option<(String, Vec<String>)> {
        if !self.config.block_mode {
            self.clear_block_selection();
            return None;
        }
        let session = self.session_manager.get_active_session_mut();
        let session_id = session.metadata.session_id.clone();
        let terminal = session.terminal.lock();
        if terminal.is_alt_buffer_active() {
            return None;
        }
        let records = terminal.command_records();
        let newest = records.len().checked_sub(1);
        let ids = records
            .iter()
            .enumerate()
            .filter(|(index, record)| {
                record.complete
                    && crate::block_mode::classify_outcome(
                        record.command.as_deref(),
                        record.command_truncated,
                        record.exit_code,
                        record.state,
                        record.complete,
                        Some(*index) == newest,
                    ) != crate::block_mode::BlockOutcome::Prompt
            })
            .map(|(_, record)| record.id.clone())
            .collect();
        Some((session_id, ids))
    }

    /// Keyboard navigation over the same completed set as card clicks.
    /// Clamped at either end the
    /// selection is kept silently; a dangling selected id counts as no
    /// selection and both directions restart at the newest selectable block.
    fn block_select_step(&mut self, step: crate::block_mode::SelectStep) {
        let Some(navigation) = self.block_navigation(|outcomes, current| {
            crate::block_mode::next_selected_index(outcomes, current, step)
        }) else {
            return;
        };
        let Some(target) = navigation.target else {
            // 到达两端时静默保持当前选中;只有完全没有可选块才提示。
            if !navigation.had_selection {
                let message = self.explain_block_absence("No command block to select");
                self.set_status(message);
            }
            return;
        };
        self.apply_block_selection(target);
    }

    /// `block:jump_prev_failed`: move the block selection to the nearest
    /// FAILED block older than the selection (newest failed with none).
    pub(crate) fn block_jump_prev_failed(&mut self) {
        self.block_jump_failed_step(crate::block_mode::SelectStep::Older);
    }

    /// `block:jump_next_failed`: move the block selection to the nearest
    /// FAILED block newer than the selection (newest failed with none).
    pub(crate) fn block_jump_next_failed(&mut self) {
        self.block_jump_failed_step(crate::block_mode::SelectStep::Newer);
    }

    /// Failed-only keyboard navigation (same failed classification as the
    /// scrollbar markers). The selection may sit on any block, failed or not;
    /// the step is strictly older/newer. No failed block in the requested
    /// direction is a silent no-op; zero failed blocks toasts with
    /// `block:jump_first_failed`'s wording.
    fn block_jump_failed_step(&mut self, step: crate::block_mode::SelectStep) {
        let Some(navigation) = self.block_navigation(|outcomes, current| {
            crate::block_mode::next_failed_index(outcomes, current, step)
        }) else {
            return;
        };
        let Some(target) = navigation.target else {
            if !navigation.any_failed {
                let message = self.explain_block_absence("No failed command in this session");
                self.set_status(message);
            }
            return;
        };
        self.apply_block_selection(target);
    }

    /// Shared snapshot for keyboard block navigation: classify the active
    /// session's records, resolve the current selection (cross-session or
    /// dangling ids count as no selection), and let `pick` choose the record
    /// index to select. `None` when the alt buffer is active.
    fn block_navigation(
        &mut self,
        pick: impl FnOnce(&[crate::block_mode::BlockOutcome], Option<usize>) -> Option<usize>,
    ) -> Option<BlockNavigation> {
        if !self.config.block_mode {
            self.clear_block_selection();
            return None;
        }
        let session = self.session_manager.get_active_session_mut();
        let session_id = session.metadata.session_id.clone();
        let terminal = session.terminal.lock();
        if terminal.is_alt_buffer_active() {
            // vim/btop 全屏应用下块界面不可见,导航只会隐形跳动:静默忽略。
            return None;
        }
        let records = terminal.command_records();
        let newest = records.len().checked_sub(1);
        let outcomes: Vec<crate::block_mode::BlockOutcome> = records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                if !record.complete {
                    return crate::block_mode::BlockOutcome::Prompt;
                }
                crate::block_mode::classify_outcome(
                    record.command.as_deref(),
                    record.command_truncated,
                    record.exit_code,
                    record.state,
                    record.complete,
                    Some(index) == newest,
                )
            })
            .collect();
        let current = self
            .block_selection
            .as_ref()
            .filter(|selection| selection.session_id == session_id)
            .and_then(|selection| {
                records
                    .iter()
                    .position(|record| record.complete && record.id == selection.active_id)
            });
        let target = pick(&outcomes, current)
            .and_then(|index| records.get(index))
            .map(|record| CommandTarget {
                session_id: session_id.clone(),
                execution_id: record.id.clone(),
            });
        Some(BlockNavigation {
            target,
            had_selection: current.is_some(),
            any_failed: outcomes
                .iter()
                .any(|outcome| matches!(outcome, crate::block_mode::BlockOutcome::Failed(_))),
        })
    }

    /// Select `target` and reveal it: set `block_selection`, highlight the
    /// Commands-sidebar row, and scroll to the block (the same jump path as
    /// `block:jump_first_failed`).
    fn apply_block_selection(&mut self, target: CommandTarget) -> bool {
        if !self.config.block_mode {
            self.clear_block_selection();
            return false;
        }
        let completed = self
            .target_session_index(&target)
            .and_then(|index| self.session_manager.sessions().get(index))
            .is_some_and(|session| {
                session
                    .terminal
                    .lock()
                    .command_record(&target.execution_id)
                    .is_some_and(|record| record.complete)
            });
        if !completed {
            self.set_status("Command block is no longer available");
            return false;
        }
        self.block_selection = Some(crate::block_mode::BlockSelection::single(
            target.session_id.clone(),
            target.execution_id.clone(),
        ));
        self.sync_block_selection_to_sidebar(&target);
        self.reveal_sidebar_command(&target, BlockReveal::IfOffscreen);
        true
    }

    /// Highlight the newly selected block's row in the Commands sidebar.
    /// This IS the sidebar's own selection model — the same field a row
    /// click sets — so the row also expands its detail panel, exactly as if
    /// it had been clicked. Set unconditionally (id-based, no rendering):
    /// opening the sidebar on the Commands view later shows the highlight.
    pub(crate) fn sync_block_selection_to_sidebar(&mut self, target: &CommandTarget) {
        self.command_sidebar.selected = Some(target.clone());
    }

    /// Clear both views of the same block selection. Gutter/keyboard block
    /// selection and the Commands-sidebar highlight are deliberately kept in
    /// sync, so every dismissal path must retire both halves together.
    pub(crate) fn clear_block_selection(&mut self) {
        clear_block_selection_state(
            &mut self.block_selection,
            &mut self.command_sidebar.selected,
        );
    }

    pub(crate) fn clear_block_selection_for_session(&mut self, session_id: &str) {
        clear_block_selection_state_for_session(
            &mut self.block_selection,
            &mut self.command_sidebar.selected,
            session_id,
        );
    }

    /// `block:search`: toggle the cross-block search picker. Like keyboard
    /// block navigation, it never OPENS over a fullscreen (alt-buffer) app —
    /// blocks are invisible there and Enter's jump could only toast — but
    /// closing an already-open picker is always allowed.
    pub(crate) fn block_search_toggle(&mut self) {
        if self.block_search.is_open {
            self.block_search.close();
            return;
        }
        if !self.config.block_mode {
            self.clear_block_selection();
            return;
        }
        let alt_buffer_active = {
            let session = self.session_manager.get_active_session_mut();
            session.terminal.lock().is_alt_buffer_active()
        };
        if alt_buffer_active {
            // vim/btop 全屏应用下块界面不可见:与块导航一致,静默忽略。
            return;
        }
        self.block_search.open();
    }

    /// Refresh through the same path for the F5 shortcut and the visible
    /// button. The query regains focus after a pointer click, while invalid
    /// expressions keep their last valid cache and results untouched.
    pub(crate) fn block_search_manual_refresh(&mut self) {
        if !self.block_search.is_open {
            return;
        }
        self.block_search.needs_focus = true;
        if self.block_search.request_manual_refresh() {
            self.refresh_block_search_hits();
        }
    }

    /// Recompute the picker's hits for the active session and current query.
    ///
    /// Record text is not re-extracted per keystroke: it lives in a bounded
    /// cache rebuilt only when the active session or finalized-record version
    /// changes. Invalid regexes preserve the last usable cache/hits and merely
    /// gate activation until the expression compiles again.
    pub(crate) fn refresh_block_search_hits(&mut self) {
        let query = self.block_search.query.clone();
        // 只有“记录版本变了”(后台命令刚结束)才保留高亮行。查询、大小写、
        // 正则和过滤芯片改变时 computed_query 已被置 None,那是新意图,回到
        // 第一行。锚点必须在 rebuild 之前取:release_index_for_rebuild 会清空
        // hits 并把 selected_index 归零。
        let query_changed = self.block_search.computed_query.as_deref() != Some(query.as_str());
        let mut retained_anchor = if query_changed {
            None
        } else {
            self.block_search.selected_hit_anchor()
        };
        // 记录 id 只在单个 session 内唯一(`local:{sequence}` 每个终端都从 1
        // 开始),所以锚点必须记住它来自哪个 pane。
        let anchor_session_id = self.block_search.session_id.clone();
        let (session_id, record_version) = {
            let session = self.session_manager.get_active_session_mut();
            let terminal = session.terminal.lock();
            (
                session.metadata.session_id.clone(),
                block_search_record_version(terminal.command_records()),
            )
        };
        let bookmark_revision = self.block_bookmarks.revision(&session_id);
        // 切换 tab/pane 后旧的高亮行不再有任何意义:同名 id 会指向另一个
        // pane 里完全无关的块。回到第一行。
        if anchor_session_id.as_deref() != Some(session_id.as_str()) {
            retained_anchor = None;
        }
        if !self
            .block_search
            .needs_refresh(&session_id, record_version, bookmark_revision)
        {
            return;
        }
        let cache_needs_rebuild = self.block_search.session_id.as_deref()
            != Some(session_id.as_str())
            || self.block_search.record_version != Some(record_version);
        if cache_needs_rebuild
            && defer_same_session_block_search_rebuild_if_invalid(
                &mut self.block_search,
                &session_id,
            )
        {
            return;
        }
        if cache_needs_rebuild {
            self.rebuild_block_search_cache(&session_id, record_version);
        }

        let filter = self.block_search.filter;
        let scope = self.block_search.scope;
        let eligible: std::collections::HashSet<String> = {
            let session = self.session_manager.get_active_session_mut();
            let terminal = session.terminal.lock();
            terminal
                .command_records()
                .iter()
                .filter(|record| record.complete)
                .filter(|record| match filter {
                    crate::block_search::BlockSearchFilter::All => true,
                    crate::block_search::BlockSearchFilter::Failed => matches!(
                        crate::block_mode::classify_outcome(
                            record.command.as_deref(),
                            record.command_truncated,
                            record.exit_code,
                            record.state,
                            record.complete,
                            false,
                        ),
                        crate::block_mode::BlockOutcome::Failed(_)
                    ),
                    crate::block_search::BlockSearchFilter::Slow => {
                        record.duration_ms.is_some_and(|duration| {
                            duration >= self.config.notify_long_block_threshold_ms
                        })
                    }
                    crate::block_search::BlockSearchFilter::Bookmarked => {
                        self.block_bookmarks.contains(&session_id, record.sequence)
                    }
                    crate::block_search::BlockSearchFilter::Background => matches!(
                        crate::block_mode::classify_outcome(
                            record.command.as_deref(),
                            record.command_truncated,
                            record.exit_code,
                            record.state,
                            record.complete,
                            false,
                        ),
                        crate::block_mode::BlockOutcome::Background
                    ),
                })
                .map(|record| record.id.clone())
                .collect()
        };
        let results = match crate::block_mode::validated_block_search_query(&query) {
            Err(error) => Err(error),
            Ok(validated)
                if validated.is_empty()
                    && filter != crate::block_search::BlockSearchFilter::All =>
            {
                let mut hits = Vec::new();
                let mut capped = false;
                for record in self
                    .block_search
                    .cache
                    .iter()
                    .rev()
                    .filter(|record| eligible.contains(&record.record_id))
                {
                    if hits.len() >= crate::block_mode::MAX_BLOCK_SEARCH_HITS {
                        capped = true;
                        break;
                    }
                    let display = metadata_browse_display(record, scope);
                    let Some((line_text, is_output_line, line_no)) = display else {
                        continue;
                    };
                    hits.push(crate::block_mode::BlockSearchHit {
                        record_id: record.record_id.clone(),
                        is_output_line,
                        line_no,
                        match_span: None,
                        line_text: crate::block_mode::single_line_clip(
                            line_text,
                            crate::block_mode::BLOCK_SEARCH_LINE_TEXT_CHARS,
                        ),
                        command_preview: crate::block_mode::single_line_clip(
                            record.command.as_deref().unwrap_or_default(),
                            crate::block_mode::BLOCK_SEARCH_COMMAND_PREVIEW_CHARS,
                        ),
                    });
                }
                Ok(crate::block_mode::BlockSearchResults { hits, capped })
            }
            Ok(_) => crate::block_mode::search_blocks_with_options_filtered_in_scope(
                &self.block_search.cache,
                &query,
                crate::block_mode::BlockSearchOptions {
                    case_sensitive: self.block_search.case_sensitive,
                    regex: self.block_search.regex,
                    whole_word: self.block_search.whole_word,
                },
                scope,
                |record_id| eligible.contains(record_id),
            ),
        };
        match results {
            Ok(results) => {
                self.block_search.adopt_hits(
                    results.hits,
                    results.capped,
                    &session_id,
                    retained_anchor.as_ref(),
                );
            }
            Err(error) => {
                self.block_search.query_error = Some(error.to_string());
            }
        }
        self.block_search.session_id = Some(session_id);
        self.block_search.record_version = Some(record_version);
        self.block_search.bookmark_revision = Some(bookmark_revision);
        self.block_search.computed_query = Some(query);
    }

    /// Build the picker's extraction cache from the active session's records,
    /// newest first (so the hit cap keeps recent history). Output text comes
    /// from the same source as `block:copy_output`: the captured snapshot —
    /// read directly off the record, no by-id lookup — with live anchor
    /// extraction as the fallback for records that have no capture yet, each
    /// bounded by the captured-output cap. This is the one extraction pass
    /// per picker-open; per-keystroke searches never touch the terminal.
    fn rebuild_block_search_cache(
        &mut self,
        session_id: &str,
        record_version: crate::block_search::BlockSearchRecordVersion,
    ) {
        // Release the previous 16 MiB-class index and hit allocations before
        // extracting the new 8 MiB source snapshot. Assignment (rather than
        // `clear`) drops Vec capacity too, keeping peak at old-or-source+new.
        self.block_search.release_index_for_rebuild();
        let session = self.session_manager.get_active_session_mut();
        let terminal = session.terminal.lock();
        let snapshot = crate::block_mode::bounded_block_search_sources(
            terminal
                .command_records()
                .iter()
                .rev()
                .filter(|record| record.complete)
                .map(|record| {
                    let output = match record.captured_output.as_ref() {
                        // Captures are produced under MAX_COMPLETED_COMMAND_OUTPUT_BYTES,
                        // so the snapshot is already bounded.
                        Some(captured) => Some(captured.text.clone()),
                        None => terminal
                            .command_output_text(&record.id, MAX_COMPLETED_COMMAND_OUTPUT_BYTES)
                            .map(|text| text.text),
                    };
                    crate::block_mode::BlockSearchSource::new(
                        record.id.clone(),
                        record.command.clone(),
                        output,
                    )
                }),
            crate::block_mode::BLOCK_SEARCH_SOURCE_MAX_BYTES,
        );
        drop(terminal);
        let build = crate::block_mode::build_block_search_cache(
            snapshot,
            crate::block_mode::BLOCK_SEARCH_CACHE_MAX_BYTES,
        );
        self.block_search.cache = build.records;
        self.block_search.older_not_indexed = build.older_not_indexed;
        self.block_search.session_id = Some(session_id.to_string());
        self.block_search.record_version = Some(record_version);
        self.block_search.query_error = None;
    }

    /// A click/plain Enter accepts and closes. Shift+Enter reveals one hit,
    /// advances to the next, and keeps the picker open so several matches can
    /// be reviewed without rebuilding the query.
    pub(crate) fn block_search_confirm(&mut self) {
        self.block_search_accept(false);
    }

    /// Select and reveal the highlighted hit. With no hits this is a no-op and
    /// the picker stays open (palette precedent).
    /// A record that scrolled out of reach in the meantime degrades to the
    /// jump path's own toast.
    pub(crate) fn block_search_accept(&mut self, keep_open: bool) {
        // A PTY completion can rotate the bounded record deque between the
        // last paint and Enter. Refresh first so a hit built for an old
        // finalized-record version is never resolved against the new one.
        self.refresh_block_search_hits();
        let Some((target, hit)) = self
            .block_search
            .selected_hit()
            .zip(self.block_search.session_id.as_ref())
            .map(|(hit, session_id)| {
                (
                    CommandTarget {
                        session_id: session_id.clone(),
                        execution_id: hit.record_id.clone(),
                    },
                    hit.clone(),
                )
            })
        else {
            return;
        };
        let activation =
            block_search_activation(self.apply_block_selection(target.clone()), keep_open);
        if activation == BlockSearchActivation::RejectStale {
            // `apply_block_selection` already reported the stale target. Keep
            // the picker and selection in place; never advance from a result
            // that was not actually revealed.
            self.block_search.computed_query = None;
            self.refresh_block_search_hits();
            return;
        }
        if let Some(line_no) = hit.line_no {
            if let Some(index) = self.session_manager.index_of(&target.session_id) {
                if let Some(session) = self.session_manager.sessions_mut().get_mut(index) {
                    let terminal_arc = std::sync::Arc::clone(&session.terminal);
                    let policy = &mut session.projection_policy;
                    let view_state = &mut session.projection_view_state;
                    let mut terminal = terminal_arc.lock();
                    let anchor = hit
                        .match_span
                        .as_ref()
                        .and_then(|span| {
                            terminal.command_output_match_anchor(
                                &target.execution_id,
                                line_no,
                                span.start,
                                span.end,
                            )
                        })
                        .or_else(|| {
                            terminal.command_output_line_anchor(&target.execution_id, line_no)
                        });
                    if let Some(anchor) = anchor {
                        let transformed = terminal
                            .projected_viewport_with_state(
                                crate::terminal::HistoryProjection::identity(),
                                self.config.block_mode,
                                policy,
                                view_state,
                            )
                            .is_transformed();
                        if transformed {
                            let location = terminal
                                .reveal_buffer_anchor_in_projection(policy, view_state, anchor);
                            if let crate::terminal::ProjectedBufferAnchorLocation::Hidden {
                                zone_id,
                            } = location
                            {
                                // Enter on an explicit search hit means reveal
                                // its content. Expand only the exact collapse
                                // proven to own that raw anchor, rebuild the
                                // plan, then resolve the same stable anchor.
                                if policy.expand(zone_id) {
                                    terminal.clear_text_selection();
                                    let _ = terminal.projected_viewport_with_state(
                                        crate::terminal::HistoryProjection::identity(),
                                        self.config.block_mode,
                                        policy,
                                        view_state,
                                    );
                                    let _ = terminal.reveal_buffer_anchor_in_projection(
                                        policy, view_state, anchor,
                                    );
                                }
                            }
                        } else {
                            let _ = terminal.scroll_to_buffer_anchor(anchor);
                        }
                    }
                }
            }
        }
        match activation {
            BlockSearchActivation::RejectStale => unreachable!(),
            BlockSearchActivation::Close => self.block_search.close(),
            BlockSearchActivation::Advance => {
                if self.block_search.is_open {
                    self.block_search.select_next();
                    self.block_search.needs_focus = true;
                }
            }
        }
    }

    /// Shared targeting rule for every `block:*` copy/recall command: a
    /// selection in the ACTIVE session wins — and when its record is gone it
    /// toasts and does nothing, never silently retargeting — while a
    /// selection belonging to another session counts as no selection at all
    /// (matching keyboard navigation, which is active-session scoped). With
    /// no usable selection, fall back to the newest complete record of the
    /// active session matching `wanted`, toasting `missing` when none does.
    fn block_target_or_newest(
        &mut self,
        wanted: impl Fn(&crate::terminal::CommandRecord) -> bool,
        missing: &str,
    ) -> Option<CommandTarget> {
        let active_session_id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        let selection_in_active_session = self
            .block_selection
            .as_ref()
            .is_some_and(|selection| selection.session_id == active_session_id);
        if selection_in_active_session {
            let target = self.live_block_target();
            if target.is_none() {
                self.set_status("Selected command block is no longer available");
            }
            target
        } else {
            let target = self.latest_block_target(wanted);
            if target.is_none() {
                let message = self.explain_block_absence(missing);
                self.set_status(message);
            }
            target
        }
    }

    /// `block:copy_block`: the whole block as plain text — command line,
    /// newline, output. Background blocks copy output only (anvil/forge
    /// `block_clipboard_text` family rule).
    pub(crate) fn block_copy_block(&mut self) {
        let Some(targets) = self.block_targets_or_newest(
            |record| Self::record_has_command(record) || Self::record_has_output(record),
            "No command block to copy",
        ) else {
            return;
        };
        self.copy_block_targets(&targets, CopyKind::Combined);
    }

    /// `block:copy_markdown`: the block as a Markdown document. The exact
    /// shape (and its sanitization) is pinned in `block_mode` tests; frost
    /// ships the same format.
    pub(crate) fn block_copy_markdown(&mut self) {
        let Some(targets) = self.block_targets_or_newest(
            |record| Self::record_has_command(record) || Self::record_has_output(record),
            "No command block to copy",
        ) else {
            return;
        };
        self.copy_block_targets_markdown(&targets);
    }

    fn replay_sidebar_command(&mut self, target: &CommandTarget, run: bool, require_empty: bool) {
        let Some(index) = self.target_session_index(target) else {
            self.set_status("Command session is no longer available");
            return;
        };
        if self.session_manager.active_index() != index && !self.activate_session(index) {
            self.set_status("Command session is no longer available");
            return;
        }
        if self
            .session_manager
            .sessions()
            .get(index)
            .is_some_and(|session| {
                session.purpose == crate::session::SessionPurpose::RetainedCommand
            })
        {
            self.set_status("Exited task terminals are read-only");
            return;
        }
        let direct_input_blocked = self.direct_input_is_blocked_for_session(&target.session_id);
        let process_cwd = self
            .session_manager
            .sessions()
            .get(index)
            .and_then(|session| jterm_core::process::process_cwd(session.get_shell_pid()));

        let persisted = self
            .persisted_sidebar_execution(target)
            .filter(|record| !record.command_truncated && !record.command.is_empty())
            .map(|record| (record.command.clone(), record.cwd.clone()));
        let outcome = {
            let Some(session) = self.session_manager.get_session_mut(index) else {
                return self.set_status("Command session is no longer available");
            };
            let pending_input = direct_input_blocked || !session.pending_input.is_empty();
            let replay = {
                let terminal = session.terminal.lock();
                let live_record = terminal.command_record(&target.execution_id);
                let command = live_record
                    .and_then(|record| {
                        (record.command_exact && !record.command_truncated)
                            .then(|| record.command.clone())
                            .flatten()
                    })
                    .or_else(|| persisted.as_ref().map(|(command, _cwd)| command.clone()));
                let source_cwd = live_record
                    .and_then(|record| record.cwd.clone())
                    .or_else(|| persisted.as_ref().map(|(_command, cwd)| cwd.clone()));
                (
                    command,
                    source_cwd,
                    terminal.current_working_dir.clone(),
                    process_cwd,
                    terminal.shell_is_prompt_ready(),
                    terminal.is_alt_buffer(),
                    terminal.is_bracketed_paste_enabled(),
                    terminal.prompt_input_is_empty(),
                )
            };
            let Some(command) = replay.0 else {
                return self.set_status("Exact command text is unavailable");
            };
            let command = match prepare_replay_command(&command) {
                Ok(command) => command,
                Err(error) => {
                    return self.set_status_for(
                        format!("Command replay rejected: {error}"),
                        Duration::from_secs(5),
                    );
                }
            };
            let cwd_matches = replay.1.as_deref().is_some_and(|source| {
                verified_local_command_cwd(source, replay.2.as_deref(), replay.3.as_deref())
            });
            if run && !cwd_matches {
                ReplayOutcome::WorkingDirectoryChanged
            } else if replay.5 {
                ReplayOutcome::AlternateScreen
            } else if !replay.4 {
                ReplayOutcome::NotPromptReady
            } else if !replay.6 {
                ReplayOutcome::BracketedPasteDisabled
            } else if pending_input {
                ReplayOutcome::PendingInput
            } else if require_empty && !replay.7 {
                ReplayOutcome::PromptNotEmpty
            } else if command.is_empty() {
                ReplayOutcome::EmptyCommand
            } else if run && replay_command_is_multiline(&command) {
                ReplayOutcome::MultilineRun
            } else {
                match replay_payload(&command, run) {
                    Err(error) => ReplayOutcome::UnsafeCommand(error.to_string()),
                    Ok(payload) => match session.shell.write(&payload) {
                        Ok(()) => {
                            let mut terminal = session.terminal.lock();
                            terminal.note_user_input(&payload);
                            terminal.scroll_to_bottom();
                            drop(terminal);
                            session.projection_view_state.scroll_to_bottom();
                            if run {
                                ReplayOutcome::Ran
                            } else {
                                ReplayOutcome::Filled
                            }
                        }
                        Err(error) => ReplayOutcome::WriteFailed(error),
                    },
                }
            }
        };

        let replay_accepted = replay_outcome_accepted(&outcome);
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
            ReplayOutcome::PromptNotEmpty => {
                self.set_status("Clear the current prompt before recalling a command")
            }
            ReplayOutcome::EmptyCommand => self.set_status("Command text is empty"),
            ReplayOutcome::UnsafeCommand(error) => self.set_status_for(
                format!("Command replay rejected: {error}"),
                Duration::from_secs(5),
            ),
            ReplayOutcome::MultilineRun => {
                self.set_status("Run again is disabled for multiline commands; use Fill instead")
            }
            ReplayOutcome::WorkingDirectoryChanged => self.set_status_for(
                "Retry requires the current shell cwd to match the command's recorded cwd",
                Duration::from_secs(5),
            ),
            ReplayOutcome::WriteFailed(error) => self.set_status_for(
                format!("Command replay failed: {error}"),
                Duration::from_secs(4),
            ),
        }
        if replay_accepted {
            self.clear_block_selection_for_session(&target.session_id);
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum CopyKind {
    Command,
    Output,
    Combined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentTaskIntent {
    Fix,
    Explain,
    Compose,
}

/// One keyboard block-navigation snapshot — see
/// [`TerminalApp::block_navigation`]. `had_selection`/`any_failed` let the
/// callers distinguish a silent clamp from a "nothing to select" toast.
struct BlockNavigation {
    target: Option<CommandTarget>,
    had_selection: bool,
    any_failed: bool,
}

/// Copy/export snapshot that cannot confuse an unavailable command with a
/// genuine background-output record.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CapturedBlockText {
    /// Sanitized replay/display text, absent when the raw command was omitted
    /// or rejected by the bounded visual-spoof guard.
    command: Option<String>,
    /// Whether nonblank raw command metadata existed before sanitization.
    command_present: bool,
    /// Exact, untruncated metadata whose sanitized text remains available.
    command_exact: bool,
    /// Producer-declared omission/shortening, independent of `command`.
    command_truncated: bool,
    output: Option<(String, bool)>,
}

impl CapturedBlockText {
    fn is_background(&self) -> bool {
        !self.command_present && !self.command_truncated
    }
}

fn captured_command_provenance(
    raw_command: Option<&str>,
    command_exact: bool,
    command_truncated: bool,
) -> (Option<String>, bool, bool) {
    let raw_command = raw_command.filter(|command| !command.trim().is_empty());
    let command_present = raw_command.is_some();
    let command = raw_command.and_then(|command| {
        crate::review_text::sanitize_history_replay(
            command,
            crate::review_text::MAX_HISTORY_COMMAND_BYTES,
        )
        .ok()
    });
    let command_is_exact =
        command_exact && !command_truncated && command_present && command.is_some();
    (command, command_present, command_is_exact)
}

fn enrich_semantic_context_from_history(
    context: &mut crate::agent::SemanticCommandContext,
    persisted: &PersistedExecution,
) -> bool {
    if !persisted_execution_matches_snapshot(
        LiveExecutionIdentity {
            id: &context.source_execution_id,
            command: context.command.as_deref(),
            command_exact: context.command_exact,
            command_truncated: context.command_truncated,
            cwd: context.cwd.as_deref(),
            exit_code: context.exit_code,
            duration_ms: context.duration_ms,
        },
        persisted,
    ) {
        return false;
    }
    // The journal is a secondary, bounded output cache. Never let it elevate
    // reconstructed command text to exact or fill execution-authorizing cwd /
    // exit metadata. Those fields remain live OSC evidence and had to match
    // before this point.
    if context.cwd_after.is_none() {
        context.cwd_after = persisted.cwd_after.clone();
    }
    context.started_at = context
        .started_at
        .or_else(|| system_time_from_millis(persisted.started_at_ms));
    context.finished_at = context
        .finished_at
        .or_else(|| persisted.ended_at_ms.and_then(system_time_from_millis));
    if !context.output_available {
        if let Some(output) = persisted.output.as_ref() {
            context.output_text = output.text.clone();
            context.output_available = true;
            context.output_truncated = output.truncated;
            context.output_total_bytes = usize::try_from(output.total_bytes).unwrap_or(usize::MAX);
        }
    }
    true
}

/// jsh's journal `seq` counts accepted non-empty commands, while the
/// terminal's `CommandRecord::sequence` counts prompt records (including a
/// blank Enter). They are intentionally not compared. The process-unique
/// execution id is instead cross-checked against every execution-authorizing
/// live field before journal output may fill an evicted capture.
#[derive(Clone, Copy)]
struct LiveExecutionIdentity<'a> {
    id: &'a str,
    command: Option<&'a str>,
    command_exact: bool,
    command_truncated: bool,
    cwd: Option<&'a str>,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
}

fn persisted_execution_matches_snapshot(
    live: LiveExecutionIdentity<'_>,
    persisted: &PersistedExecution,
) -> bool {
    live.id == persisted.id
        && live.command_exact
        && !live.command_truncated
        && !persisted.command_truncated
        && live.command == Some(persisted.command.as_str())
        && live.cwd == Some(persisted.cwd.as_str())
        && live.exit_code == persisted.exit_code
        && live.duration_ms == persisted.duration_ms
}

fn persisted_execution_matches_live(live: &CommandRecord, persisted: &PersistedExecution) -> bool {
    persisted_execution_matches_snapshot(
        LiveExecutionIdentity {
            id: &live.id,
            command: live.command.as_deref(),
            command_exact: live.command_exact,
            command_truncated: live.command_truncated,
            cwd: live.cwd.as_deref(),
            exit_code: live.exit_code,
            duration_ms: live.duration_ms,
        },
        persisted,
    )
}

/// Persisted metadata may fill gaps in a command that already belongs to the
/// active tab, but it must never create a sidebar row on its own. A slice
/// makes that row-count invariant explicit.
fn enrich_current_tab_rows_from_history(
    rows: &mut [CommandRowSnapshot],
    live_records: &std::collections::VecDeque<CommandRecord>,
    history: &[PersistedExecution],
) {
    let live_by_id = live_records
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect::<HashMap<_, _>>();
    let history_by_id = history
        .iter()
        .filter(|persisted| {
            live_by_id
                .get(persisted.id.as_str())
                .is_some_and(|live| persisted_execution_matches_live(live, persisted))
        })
        .map(|record| (record.id.as_str(), record))
        .collect::<HashMap<_, _>>();
    for row in rows {
        if let Some(record) = history_by_id.get(row.target.execution_id.as_str()) {
            enrich_live_row_from_history(row, record);
        }
    }
}

fn enrich_live_row_from_history(row: &mut CommandRowSnapshot, record: &PersistedExecution) {
    row.output_copy_available |= record
        .output
        .as_ref()
        .is_some_and(|output| !output.text.is_empty());
    row.output_context_available |= record.output.is_some();
}

fn enrich_live_detail_from_history(
    detail: &mut CommandDetailSnapshot,
    record: &PersistedExecution,
) {
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
    let failed = completed_command_row_is_failed(row);
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
                        "exact jsh journal metadata".to_owned()
                    } else if detail.command_exact {
                        "exact shell metadata".to_owned()
                    } else if detail.command_from_history {
                        "truncated jsh journal metadata; replay disabled".to_owned()
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
                    if failed { "Retry" } else { "Run" },
                    CommandActionKind::RunAgain,
                    replay_disabled_reason(row, replay_guard, true),
                );
            });

            if failed {
                ui.add_space(3.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("Agent task")
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                    let disabled = agent_task_disabled_reason(row);
                    command_detail_action_button(
                        ui,
                        action,
                        row,
                        "Fix",
                        CommandActionKind::FixWithAgent,
                        disabled,
                    );
                    command_detail_action_button(
                        ui,
                        action,
                        row,
                        "Explain",
                        CommandActionKind::ExplainWithAgent,
                        disabled,
                    );
                    command_detail_action_button(
                        ui,
                        action,
                        row,
                        "Create task",
                        CommandActionKind::CreateAgentTask,
                        disabled,
                    );
                });
            }

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

/// Classify a completed sidebar row from its bounded, frontend-resolved display
/// command. Rows are built only after `CommandRecord` has applied OSC metadata,
/// screen fallback, and the explicit truncated-command placeholder, so this is
/// intentionally not a raw `CommandMeta::command` field.
fn completed_command_row_outcome(row: &CommandRowSnapshot) -> CompletedBlockOutcome {
    debug_assert_eq!(row.state, CommandState::Complete);
    classify_completed(Some(row.command_preview.as_str()), row.exit_code)
}

fn completed_command_row_is_failed(row: &CommandRowSnapshot) -> bool {
    row.state == CommandState::Complete && completed_command_row_outcome(row).is_failed()
}

/// Agent actions promise the exact semantic command and its C..D output
/// snapshot. Do not silently substitute display-derived text or an empty
/// placeholder: the user should know when a task cannot be reproduced.
fn agent_task_disabled_reason(row: &CommandRowSnapshot) -> Option<&'static str> {
    if !completed_command_row_is_failed(row) {
        Some("Agent tasks are available for failed commands")
    } else if !row.command_exact {
        Some("The shell did not provide exact command metadata")
    } else if !row.command_context_fits {
        Some("The exact command exceeds the Agent context limit")
    } else if row.cwd.as_deref().is_none_or(|cwd| cwd.trim().is_empty()) {
        Some("The shell did not provide the command working directory")
    } else if !row.cwd_context_fits {
        Some("The command working directory exceeds the Agent context limit")
    } else if !row.output_context_available {
        Some("The exact semantic output block is unavailable")
    } else {
        None
    }
}

fn command_row_matches(row: &CommandRowSnapshot, query: &str, filter: CommandFilter) -> bool {
    let matches_filter = match filter {
        CommandFilter::All => true,
        CommandFilter::Failed => {
            row.state == CommandState::Complete && completed_command_row_outcome(row).is_failed()
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
        CommandState::Complete => match completed_command_row_outcome(row) {
            CompletedBlockOutcome::Success => {
                ("✓", egui::Color32::from_rgb(70, 190, 115), "Succeeded")
            }
            CompletedBlockOutcome::Failed(_) => {
                ("✕", egui::Color32::from_rgb(225, 85, 85), "Failed")
            }
            CompletedBlockOutcome::Background | CompletedBlockOutcome::Unknown => {
                ("○", egui::Color32::GRAY, "Completed")
            }
        },
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
    crate::block_mode::format_block_duration(duration_ms)
}

fn system_time_from_millis(milliseconds: u64) -> Option<SystemTime> {
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(milliseconds))
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

/// Collect a selection in record/terminal order. Background records are seen
/// (so eviction is still detected) but omitted from the command buffer.
fn selected_commands_in_terminal_order<'a>(
    records: impl IntoIterator<Item = SelectedReplayRecord<'a>>,
    selected_ids: &[String],
    max_bytes: usize,
) -> Result<(String, usize), SelectedReplayError> {
    if selected_ids.is_empty() {
        return Err(SelectedReplayError::NoSelection);
    }

    let mut text = String::new();
    let mut command_count = 0usize;
    let mut seen_records = 0usize;
    for record in records {
        if !selected_ids.iter().any(|selected| selected == record.id) {
            continue;
        }
        seen_records += 1;
        if !record.complete {
            return Err(SelectedReplayError::MissingRecord);
        }
        // A producer may omit an oversized command entirely. Inspect that
        // provenance before treating a blank value as a background block, so
        // a mixed range is rejected atomically rather than partially recalled.
        if record.truncated {
            return Err(SelectedReplayError::ExactCommandUnavailable);
        }
        let Some(raw_command) = record.command.filter(|command| !command.trim().is_empty()) else {
            // A background block is part of the visual range but contributes
            // neither an empty command nor an extra separator.
            continue;
        };
        if !record.exact {
            return Err(SelectedReplayError::ExactCommandUnavailable);
        }
        let command =
            prepare_replay_command(raw_command).map_err(SelectedReplayError::UnsafeCommand)?;
        let separator = usize::from(!text.is_empty());
        let Some(next_len) = text
            .len()
            .checked_add(separator)
            .and_then(|len| len.checked_add(command.len()))
        else {
            return Err(SelectedReplayError::TooLarge { limit: max_bytes });
        };
        if next_len > max_bytes {
            return Err(SelectedReplayError::TooLarge { limit: max_bytes });
        }
        if separator != 0 {
            text.push('\n');
        }
        text.push_str(&command);
        command_count += 1;
    }

    if seen_records != selected_ids.len() {
        return Err(SelectedReplayError::MissingRecord);
    }
    if command_count == 0 {
        return Err(SelectedReplayError::NoCommands);
    }
    Ok((text, command_count))
}

/// Bytes that put a recalled command back on the child's prompt.
///
/// Only ever reached when the child advertised DECSET 2004 (see
/// [`ReplayOutcome::BracketedPasteDisabled`]), so the payload is always framed;
/// `jterm_core::pty_input` removes any paste marker the recorded command itself
/// carries and keeps the submitting CR *outside* the frame, because Readline
/// deliberately does not execute a newline that arrived inside a bracketed
/// paste. OSC 133 and journal text are untrusted protocol input: the local
/// compatibility boundary rejects visual spoofing and removes C0/C1 controls,
/// retaining only newline/tab where Fill's product semantics require them.
///
/// The leading `Ctrl+U` matters even though the caller checked
/// `shell_is_prompt_ready`: that flag says a prompt has been drawn, not that its
/// line buffer is empty, so without the kill a replay is appended to whatever
/// the user had already typed.
fn replay_payload(
    command: &str,
    run: bool,
) -> Result<Vec<u8>, crate::review_text::ReviewTextError> {
    let command = prepare_replay_command(command)?;
    Ok(replay_prepared_payload(&command, run))
}

/// Frame text already accepted by [`prepare_replay_command`]. Multi-selection
/// sanitizes each record independently before joining it and calls this helper
/// so the aggregate can use the separate prompt-insert budget.
fn replay_prepared_payload(command: &str, run: bool) -> Vec<u8> {
    use jterm_core::pty_input::{
        encode_prompt_insert, PasteModes, PastePolicy, UnbracketedMultiline,
    };
    let policy = PastePolicy {
        submit: run,
        ..PastePolicy::prompt_insert(UnbracketedMultiline::SendVerbatim)
    };
    // A recorded command is not this app's own text: `OSC 133;C;cmd=` is
    // percent-decoded verbatim (`terminal::state::percent_decode_osc_133`), so
    // any program that printed the prompt marker chose these bytes, raw ESC
    // included. `defanged_paste_body` therefore runs to a fixed point before the
    // framing — one de-fanging pass can splice a *new* terminator out of a
    // nested one and hand it straight to the frame.
    encode_prompt_insert(
        &crate::defanged_paste_body(command, policy),
        PasteModes { bracketed: true },
        policy,
        true,
    )
    .bytes
}

fn prepare_replay_command(command: &str) -> Result<String, crate::review_text::ReviewTextError> {
    crate::review_text::sanitize_history_replay(
        command.trim_end_matches(&['\r', '\n'][..]),
        crate::review_text::MAX_HISTORY_COMMAND_BYTES,
    )
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
            '\t' => preview.push_str(" ⇥ "),
            unsafe_character
                if unsafe_character.is_control()
                    || jterm_core::review_input::is_visual_spoofing_character(unsafe_character) =>
            {
                preview.push_str(&format!("\\u{{{:X}}}", unsafe_character as u32));
            }
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
    fn command_sidebar_uses_the_same_duration_contract_as_block_chrome() {
        for duration_ms in [743, 12_345, 60_000, 92_000, 3_600_000, 7_500_000] {
            assert_eq!(
                format_duration(duration_ms),
                crate::block_mode::format_block_duration(duration_ms)
            );
        }
        assert_eq!(format_duration(3_600_000), "1h");
        assert_eq!(format_duration(7_500_000), "2h05m");
    }

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
            command_context_fits: true,
            command_multiline: multiline,
            cwd: None,
            cwd_context_fits: true,
            state: CommandState::Complete,
            complete: true,
            exit_code: Some(0),
            duration_ms: None,
            started_at: None,
            output_copy_available: false,
            output_context_available: false,
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
    fn agent_tasks_require_a_failed_exact_command_and_semantic_output() {
        let mut row = replay_test_row(true, false);
        row.exit_code = Some(101);
        row.cwd = Some("/workspace/ember".to_string());
        row.output_context_available = true;
        assert_eq!(agent_task_disabled_reason(&row), None);

        row.command_exact = false;
        assert_eq!(
            agent_task_disabled_reason(&row),
            Some("The shell did not provide exact command metadata")
        );

        row.command_exact = true;
        row.command_context_fits = false;
        assert_eq!(
            agent_task_disabled_reason(&row),
            Some("The exact command exceeds the Agent context limit")
        );

        row.command_context_fits = true;
        row.cwd = None;
        assert_eq!(
            agent_task_disabled_reason(&row),
            Some("The shell did not provide the command working directory")
        );

        row.cwd = Some("/workspace/ember".to_string());
        row.cwd_context_fits = false;
        assert_eq!(
            agent_task_disabled_reason(&row),
            Some("The command working directory exceeds the Agent context limit")
        );

        row.cwd_context_fits = true;
        row.output_context_available = false;
        assert_eq!(
            agent_task_disabled_reason(&row),
            Some("The exact semantic output block is unavailable")
        );

        row.output_context_available = true;
        row.exit_code = Some(0);
        assert_eq!(
            agent_task_disabled_reason(&row),
            Some("Agent tasks are available for failed commands")
        );
    }

    #[test]
    fn persisted_evidence_enriches_only_the_same_semantic_execution() {
        let mut context = crate::agent::SemanticCommandContext {
            source_session_id: "stable-session".to_owned(),
            source_execution_id: "persisted-execution".to_owned(),
            // Terminal prompt sequence is deliberately unrelated to the
            // journal's accepted-command sequence.
            source_sequence: 42,
            source_shell: Some("/bin/bash".to_owned()),
            command: Some("printf hi".to_owned()),
            command_exact: true,
            command_truncated: false,
            cwd: Some("/tmp".to_owned()),
            cwd_after: None,
            exit_code: Some(0),
            duration_ms: Some(12),
            output_text: String::new(),
            output_available: false,
            output_truncated: false,
            output_total_bytes: 0,
            started_at: None,
            finished_at: None,
        };
        let persisted = persisted_test_record();

        assert!(enrich_semantic_context_from_history(
            &mut context,
            &persisted
        ));
        assert_eq!(context.source_session_id, "stable-session");
        assert_eq!(context.command.as_deref(), Some("printf hi"));
        assert!(context.command_exact);
        assert!(!context.command_truncated);
        assert_eq!(context.cwd.as_deref(), Some("/tmp"));
        assert_eq!(context.cwd_after.as_deref(), Some("/tmp"));
        assert_eq!(context.exit_code, Some(0));
        assert_eq!(context.duration_ms, Some(12));
        assert_eq!(context.output_text, "hi");
        assert!(context.output_available);
        assert_eq!(context.output_total_bytes, 2);
        assert!(context.started_at.is_some());
        assert!(context.finished_at.is_some());

        let before = context.clone();
        context.source_execution_id = "different".to_owned();
        assert!(!enrich_semantic_context_from_history(
            &mut context,
            &persisted
        ));
        assert_eq!(context.command, before.command);
        assert_eq!(context.output_text, before.output_text);

        context.source_execution_id = persisted.id.clone();
        context.command = Some("stale command".to_owned());
        assert!(!enrich_semantic_context_from_history(
            &mut context,
            &persisted
        ));
        assert_eq!(context.command.as_deref(), Some("stale command"));
        assert_eq!(context.output_text, before.output_text);

        for mismatch in ["cwd", "exit", "duration"] {
            let mut candidate = before.clone();
            match mismatch {
                "cwd" => candidate.cwd = Some("/elsewhere".to_owned()),
                "exit" => candidate.exit_code = Some(1),
                "duration" => candidate.duration_ms = Some(99),
                _ => unreachable!(),
            }
            assert!(
                !enrich_semantic_context_from_history(&mut candidate, &persisted),
                "{mismatch} conflict must reject journal enrichment"
            );
        }
    }

    #[test]
    fn replay_normalizes_trailing_line_endings_without_trimming_spaces() {
        assert_eq!(
            prepare_replay_command("printf 'a\\nb'\r\n\n").unwrap(),
            "printf 'a\\nb'"
        );
        assert_eq!(prepare_replay_command(" echo hi  ").unwrap(), " echo hi  ");
    }

    #[test]
    fn selected_replay_uses_terminal_order_and_skips_background_blocks() {
        let records = [
            SelectedReplayRecord {
                id: "old",
                command: Some("printf old"),
                exact: true,
                truncated: false,
                complete: true,
            },
            SelectedReplayRecord {
                id: "background",
                command: None,
                exact: false,
                truncated: false,
                complete: true,
            },
            SelectedReplayRecord {
                id: "new",
                command: Some("printf new\n"),
                exact: true,
                truncated: false,
                complete: true,
            },
        ];
        // Selection storage order is irrelevant; record order is canonical.
        let selected = ["new", "background", "old"].map(str::to_string).to_vec();
        assert_eq!(
            selected_commands_in_terminal_order(records, &selected, 1024).unwrap(),
            ("printf old\nprintf new".to_string(), 2)
        );
    }

    #[test]
    fn selected_replay_is_atomic_for_stale_inexact_and_oversized_ranges() {
        let exact = SelectedReplayRecord {
            id: "exact",
            command: Some("echo exact"),
            exact: true,
            truncated: false,
            complete: true,
        };
        assert!(matches!(
            selected_commands_in_terminal_order(
                [exact],
                &["exact".to_string(), "evicted".to_string()],
                1024,
            ),
            Err(SelectedReplayError::MissingRecord)
        ));

        let inexact = SelectedReplayRecord {
            id: "inexact",
            command: Some("echo reconstructed"),
            exact: false,
            truncated: false,
            complete: true,
        };
        assert!(matches!(
            selected_commands_in_terminal_order(
                [exact, inexact],
                &["exact".to_string(), "inexact".to_string()],
                1024,
            ),
            Err(SelectedReplayError::ExactCommandUnavailable)
        ));

        let omitted_truncated = SelectedReplayRecord {
            id: "omitted",
            command: None,
            exact: false,
            truncated: true,
            complete: true,
        };
        assert!(matches!(
            selected_commands_in_terminal_order(
                [exact, omitted_truncated],
                &["exact".to_string(), "omitted".to_string()],
                1024,
            ),
            Err(SelectedReplayError::ExactCommandUnavailable)
        ));

        assert!(matches!(
            selected_commands_in_terminal_order([exact], &["exact".to_string()], 4),
            Err(SelectedReplayError::TooLarge { limit: 4 })
        ));
    }

    #[test]
    fn selected_replay_gives_running_and_background_enter_back_to_the_child() {
        let pending_running = SelectedReplayGuard {
            alternate_screen: false,
            prompt_ready: false,
            bracketed_paste: true,
            pending_input: true,
            prompt_empty: true,
        };
        assert!(matches!(
            prepare_selected_replay::<()>(pending_running, || {
                panic!("running ownership must be decided before aggregation")
            }),
            Err(SelectedReplayError::NotPromptReady)
        ));

        for guard in [
            SelectedReplayGuard {
                alternate_screen: false,
                prompt_ready: true,
                bracketed_paste: true,
                pending_input: true,
                prompt_empty: true,
            },
            SelectedReplayGuard {
                alternate_screen: false,
                prompt_ready: true,
                bracketed_paste: false,
                pending_input: false,
                prompt_empty: true,
            },
        ] {
            assert!(matches!(
                prepare_selected_replay::<()>(guard, || Err(SelectedReplayError::NoCommands)),
                Err(SelectedReplayError::NoCommands)
            ));
        }
    }

    #[test]
    fn selected_replay_rejects_an_idle_direct_write_while_its_route_is_barriered() {
        let barriered_idle_prompt = SelectedReplayGuard {
            alternate_screen: false,
            prompt_ready: true,
            bracketed_paste: true,
            // `try_reinput_selected_commands` folds both pending_input and
            // the session-scoped mouse/protocol gate into this field.
            pending_input: true,
            prompt_empty: true,
        };
        assert!(matches!(
            prepare_selected_replay(barriered_idle_prompt, || Ok("echo safe")),
            Err(SelectedReplayError::PendingInput)
        ));

        let edited_prompt = SelectedReplayGuard {
            alternate_screen: false,
            prompt_ready: true,
            bracketed_paste: true,
            pending_input: false,
            prompt_empty: false,
        };
        assert!(matches!(
            prepare_selected_replay(edited_prompt, || Ok("echo safe")),
            Err(SelectedReplayError::PromptNotEmpty)
        ));
        assert!(
            matches!(
                prepare_selected_replay::<()>(edited_prompt, || {
                    Err(SelectedReplayError::NoCommands)
                }),
                Err(SelectedReplayError::NoCommands)
            ),
            "an empty/background target must leave Enter to the child"
        );
    }

    #[test]
    fn accepted_replay_taints_agent_prompt_before_pty_echo() {
        let mut terminal = crate::terminal::TerminalState::new(40, 5);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let payload = replay_payload("printf safe", false).expect("safe replay");

        // Replay now routes through Session::queue_input, whose acceptance
        // contract records these bytes before any PTY echo can arrive.
        terminal.note_user_input(&payload);
        terminal.scroll_to_bottom();

        assert!(terminal.arm_agent_execution(1, "echo agent").is_err());
    }

    #[test]
    fn replay_payload_frames_the_command_and_clears_the_line_first() {
        // Ctrl+U, then the frame, then — only when running — a CR outside it.
        assert_eq!(
            replay_payload("git status", false).unwrap(),
            b"\x15\x1b[200~git status\x1b[201~"
        );
        assert_eq!(
            replay_payload("git status", true).unwrap(),
            b"\x15\x1b[200~git status\x1b[201~\r"
        );
    }

    /// A recorded command is untrusted OSC/journal data. C0/C1 controls are
    /// removed before framing, while newline keeps Fill's multiline semantics.
    #[test]
    fn replay_payload_strips_controls_and_never_embeds_a_terminator() {
        assert_eq!(
            replay_payload("printf '\x1b[31m'", false).unwrap(),
            b"\x15\x1b[200~printf '[31m'\x1b[201~"
        );
        assert_eq!(
            replay_payload("echo ok\x1b[201~\rrm -rf ~", false).unwrap(),
            b"\x15\x1b[200~echo ok[201~\nrm -rf ~\x1b[201~"
        );
    }

    /// A replayed command comes from a percent-decoded `OSC 133;C;cmd=`, i.e.
    /// from whatever program printed the prompt marker — raw ESC included. A
    /// nested terminator must not survive one de-fanging pass and be spliced back
    /// into the frame, which would close it early and run the remainder.
    #[test]
    fn a_nested_terminator_in_a_recorded_command_is_removed_to_a_fixed_point() {
        let payload = replay_payload("echo ok\x1b[\x1b[\x1b[201~201~201~\rrm -rf ~", true).unwrap();
        assert_eq!(
            payload
                .windows(b"\x1b[201~".len())
                .filter(|window| *window == b"\x1b[201~")
                .count(),
            1,
            "{payload:?}"
        );
        assert_eq!(
            payload,
            b"\x15\x1b[200~echo ok[[[201~201~201~\nrm -rf ~\x1b[201~\r"
        );
    }

    #[test]
    fn replay_rejects_visual_spoofing_and_preview_makes_it_explicit() {
        let command = "printf safe\u{202e}; rm -rf important";
        assert!(matches!(
            replay_payload(command, false),
            Err(crate::review_text::ReviewTextError::VisualSpoof)
        ));
        assert_eq!(
            single_line_command_preview(command, 100),
            "printf safe\\u{202E}; rm -rf important"
        );
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
        row.cwd = Some("/work/ember".to_owned());

        assert!(command_row_matches(&row, "cargo", CommandFilter::All));
        assert!(command_row_matches(&row, "ember", CommandFilter::All));
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
    fn sidebar_failure_filter_and_status_share_the_completed_contract() {
        let mut row = replay_test_row(true, false);
        row.command_preview = "false".to_owned();
        row.exit_code = Some(7);
        assert!(command_row_matches(&row, "", CommandFilter::Failed));
        assert_eq!(command_status(&row).2, "Failed");

        // A legacy/synthetic commandless row is Background even when it carries
        // a raw non-zero status; neither sidebar consumer may call it failed.
        row.command_preview.clear();
        assert!(!command_row_matches(&row, "", CommandFilter::Failed));
        assert_eq!(command_status(&row).2, "Completed");

        // Conversely, a resolved command with no reported status is Unknown,
        // never Success or Failed.
        row.command_preview = "command-without-status".to_owned();
        row.exit_code = None;
        assert!(!command_row_matches(&row, "", CommandFilter::Failed));
        assert_eq!(command_status(&row).2, "Completed");
    }

    #[test]
    fn replay_menu_guard_tracks_jsh_editor_safety_requirements() {
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
    fn execution_cwd_requires_an_independent_matching_local_process() {
        assert!(verified_local_command_cwd(
            "/workspace/ember",
            Some("/workspace/ember"),
            Some("/workspace/ember")
        ));
        assert!(verified_local_command_cwd(
            "/workspace/ember",
            None,
            Some("/workspace/ember")
        ));
        assert!(!verified_local_command_cwd(
            "/workspace/ember",
            Some("/workspace/spoofed"),
            Some("/workspace/ember")
        ));
        assert!(!verified_local_command_cwd(
            "/workspace/ember",
            Some("/workspace/ember"),
            Some("/local/ssh-wrapper")
        ));
        assert!(!verified_local_command_cwd(
            "/workspace/ember",
            Some("/workspace/ember"),
            None
        ));
        assert!(!verified_local_command_cwd(
            "relative/workspace",
            Some("relative/workspace"),
            Some("relative/workspace")
        ));
    }

    #[test]
    fn journal_output_matches_exact_live_fields_not_the_unrelated_prompt_sequence() {
        let mut matching_record = persisted_test_record();
        matching_record.id = "execution".to_owned();
        matching_record.seq = 1;
        let unmatched_record = persisted_test_record();
        let mut terminal = crate::terminal::TerminalState::new(80, 8);
        // The first blank prompt advances TerminalState's local sequence but
        // is not an accepted jsh command and therefore does not advance the
        // journal sequence.
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07\x1b]133;A\x07$ \x1b]133;B\x07\x1b]133;C;id=execution;cmdline_url=printf%20hi;cwd_url=%2Ftmp\x07hi\x1b]133;D;0;id=execution;duration_ms=12;cwd_url=%2Ftmp\x07",
        );
        let live = terminal.command_record("execution").unwrap();
        assert_ne!(live.sequence, matching_record.seq);

        let mut rows = vec![replay_test_row(true, false)];
        rows[0].command_summary = "printf hi".to_owned();
        rows[0].command_preview = "printf hi".to_owned();
        rows[0].cwd = Some("/tmp".to_owned());
        rows[0].exit_code = Some(0);
        rows[0].duration_ms = Some(12);

        enrich_current_tab_rows_from_history(
            &mut rows,
            terminal.command_records(),
            &[unmatched_record, matching_record],
        );

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
    fn captured_command_provenance_never_relabels_omitted_or_unsafe_as_background() {
        let (command, present, exact) = captured_command_provenance(None, false, false);
        let background = CapturedBlockText {
            command,
            command_present: present,
            command_exact: exact,
            command_truncated: false,
            output: Some(("motd".into(), false)),
        };
        assert!(background.is_background());

        let (command, present, exact) = captured_command_provenance(None, false, true);
        let omitted = CapturedBlockText {
            command,
            command_present: present,
            command_exact: exact,
            command_truncated: true,
            output: Some(("partial".into(), false)),
        };
        assert!(!omitted.is_background());
        assert!(omitted.command.is_none());
        assert!(omitted.command_truncated);

        let unsafe_raw = "echo safe\u{202e}hidden";
        let (command, present, exact) = captured_command_provenance(Some(unsafe_raw), true, false);
        let unsafe_command = CapturedBlockText {
            command,
            command_present: present,
            command_exact: exact,
            command_truncated: false,
            output: None,
        };
        assert!(!unsafe_command.is_background());
        assert!(unsafe_command.command_present);
        assert!(unsafe_command.command.is_none());
        assert!(!unsafe_command.command_exact);

        let (command, present, exact) =
            captured_command_provenance(Some("echo reconstructed"), false, false);
        assert_eq!(command.as_deref(), Some("echo reconstructed"));
        assert!(present);
        assert!(!exact, "Markdown may export it, but replay/copy cannot");
    }

    #[test]
    fn block_clipboard_aggregate_accepts_exact_family_cap_and_rejects_plus_one() {
        let exact = "x".repeat(MAX_BLOCK_CLIPBOARD_BYTES);
        let mut aggregate = String::new();
        assert_eq!(
            append_bounded_block_part(&mut aggregate, &exact, "\n\n", MAX_BLOCK_CLIPBOARD_BYTES,),
            Ok(())
        );
        assert_eq!(aggregate.len(), MAX_BLOCK_CLIPBOARD_BYTES);
        drop(aggregate);
        drop(exact);

        let over = "x".repeat(MAX_BLOCK_CLIPBOARD_BYTES + 1);
        assert_eq!(
            append_bounded_block_part(&mut String::new(), &over, "\n\n", MAX_BLOCK_CLIPBOARD_BYTES,),
            Err(BlockClipboardBuildError::TooLarge)
        );
    }

    #[test]
    fn markdown_aggregate_charges_document_separator_before_appending() {
        let mut aggregate = String::new();
        append_bounded_block_part(&mut aggregate, "abc", "\n\n---\n\n", 16).unwrap();
        append_bounded_block_part(&mut aggregate, "123456", "\n\n---\n\n", 16).unwrap();
        assert_eq!(aggregate.len(), 16);
        assert_eq!(
            append_bounded_block_part(&mut aggregate, "x", "\n\n---\n\n", 16),
            Err(BlockClipboardBuildError::TooLarge)
        );
        assert_eq!(aggregate.len(), 16, "oversize rejection is atomic");
    }

    #[test]
    fn clearing_a_block_selection_retires_terminal_and_sidebar_state_together() {
        let mut block_selection = Some(crate::block_mode::BlockSelection::single(
            "session".to_owned(),
            "execution".to_owned(),
        ));
        let mut sidebar_selection = Some(CommandTarget {
            session_id: "session".to_owned(),
            execution_id: "execution".to_owned(),
        });

        clear_block_selection_state(&mut block_selection, &mut sidebar_selection);

        assert_eq!(block_selection, None);
        assert_eq!(sidebar_selection, None);
    }

    #[test]
    fn session_scoped_selection_cleanup_does_not_touch_another_terminal() {
        let block_selection = crate::block_mode::BlockSelection::single(
            "selected-session".to_owned(),
            "execution".to_owned(),
        );
        let sidebar_selection = CommandTarget {
            session_id: "selected-session".to_owned(),
            execution_id: "execution".to_owned(),
        };
        assert!(block_selection_state_targets_session(
            Some(&block_selection),
            Some(&sidebar_selection),
            "selected-session"
        ));
        assert!(!block_selection_state_targets_session(
            Some(&block_selection),
            Some(&sidebar_selection),
            "other-session"
        ));

        let mut block_selection = Some(block_selection);
        let mut sidebar_selection = Some(sidebar_selection);
        clear_block_selection_state_for_session(
            &mut block_selection,
            &mut sidebar_selection,
            "other-session",
        );
        assert!(block_selection.is_some());
        assert!(sidebar_selection.is_some());
        clear_block_selection_state_for_session(
            &mut block_selection,
            &mut sidebar_selection,
            "selected-session",
        );
        assert!(block_selection.is_none());
        assert!(sidebar_selection.is_none());
    }

    #[test]
    fn only_accepted_sidebar_replays_retire_block_key_ownership() {
        assert!(replay_outcome_accepted(&ReplayOutcome::Filled));
        assert!(replay_outcome_accepted(&ReplayOutcome::Ran));
        assert!(!replay_outcome_accepted(&ReplayOutcome::PendingInput));
        assert!(!replay_outcome_accepted(&ReplayOutcome::NotPromptReady));
    }

    #[test]
    fn command_preview_is_single_line_and_bounded() {
        assert_eq!(
            single_line_command_preview("one\r\ntwo\nthree", 100),
            "one ↵ two ↵ three"
        );
        assert_eq!(single_line_command_preview("abcdef", 3), "abc…");
    }

    #[test]
    fn block_absence_diagnosis_only_blames_missing_shell_integration_without_marks() {
        assert_eq!(
            block_absence_message(true, "No failed command in this session"),
            "No failed command in this session"
        );
        let missing = block_absence_message(false, "No command block to copy");
        assert!(missing.starts_with("No command block to copy:"));
        assert!(missing.contains("not reporting commands"));
        assert!(missing.contains("Install or update jsh"));
    }

    #[test]
    fn block_search_continuous_review_advances_only_after_a_live_reveal() {
        assert_eq!(
            block_search_activation(true, false),
            BlockSearchActivation::Close
        );
        assert_eq!(
            block_search_activation(true, true),
            BlockSearchActivation::Advance
        );
        assert_eq!(
            block_search_activation(false, false),
            BlockSearchActivation::RejectStale
        );
        assert_eq!(
            block_search_activation(false, true),
            BlockSearchActivation::RejectStale
        );
    }

    #[test]
    fn invalid_query_defers_only_same_session_version_rebuilds() {
        let version = crate::block_search::BlockSearchRecordVersion {
            len: 1,
            oldest_sequence: Some(7),
            newest_sequence: Some(7),
        };
        let mut state = crate::block_search::BlockSearchState {
            query: "[".to_string(),
            regex: true,
            hits: vec![crate::block_mode::BlockSearchHit {
                record_id: "old-hit".to_string(),
                is_output_line: false,
                line_no: None,
                match_span: None,
                line_text: "old result".to_string(),
                command_preview: "old result".to_string(),
            }],
            cache: vec![crate::block_mode::CachedBlockSearchRecord::new(
                "old-hit",
                Some("old result"),
                None,
            )],
            selected_index: 0,
            session_id: Some("pane-a".to_string()),
            record_version: Some(version),
            computed_query: None,
            ..Default::default()
        };

        assert!(defer_same_session_block_search_rebuild_if_invalid(
            &mut state, "pane-a"
        ));
        assert!(state.query_error.is_some());
        assert_eq!(state.computed_query.as_deref(), Some("["));
        assert_eq!(state.record_version, Some(version));
        assert_eq!(state.hits[0].record_id, "old-hit");
        assert_eq!(state.cache[0].record_id, "old-hit");

        // The already-known error remains a constant-time deferral on later
        // frames, but changing the intent to a valid literal permits rebuild.
        assert!(defer_same_session_block_search_rebuild_if_invalid(
            &mut state, "pane-a"
        ));
        state.regex = false;
        state.computed_query = None;
        assert!(!defer_same_session_block_search_rebuild_if_invalid(
            &mut state, "pane-a"
        ));

        // A pane switch must never retain another terminal's identities, even
        // when the remembered expression is invalid there too.
        state.regex = true;
        state.computed_query = None;
        assert!(!defer_same_session_block_search_rebuild_if_invalid(
            &mut state, "pane-b"
        ));
    }

    #[test]
    fn metadata_browse_scopes_use_only_real_meaningful_text() {
        let background = crate::block_mode::CachedBlockSearchRecord::new(
            "background",
            None,
            Some("\n  \nfirst output\nsecond output".to_string()),
        );
        assert_eq!(
            metadata_browse_display(&background, crate::block_mode::BlockSearchScope::Command),
            None,
            "commandless background records must not synthesize Cmd text"
        );
        assert_eq!(
            metadata_browse_display(&background, crate::block_mode::BlockSearchScope::Output),
            Some(("first output", true, Some(3)))
        );
        assert_eq!(
            metadata_browse_display(&background, crate::block_mode::BlockSearchScope::All),
            Some(("first output", true, Some(3))),
            "All must fall back to retained output, never a fake Background output label"
        );

        let command_only = crate::block_mode::CachedBlockSearchRecord::new(
            "command",
            Some("  printf hi  \necho done"),
            Some("\n\t".to_string()),
        );
        assert_eq!(
            metadata_browse_display(&command_only, crate::block_mode::BlockSearchScope::All),
            Some(("  printf hi  \necho done", false, None))
        );
        assert_eq!(
            metadata_browse_display(&command_only, crate::block_mode::BlockSearchScope::Output),
            None,
            "blank retained output is not a browse hit"
        );

        let blank = crate::block_mode::CachedBlockSearchRecord::new(
            "blank",
            None,
            Some("\n \t\n".to_string()),
        );
        for scope in [
            crate::block_mode::BlockSearchScope::All,
            crate::block_mode::BlockSearchScope::Command,
            crate::block_mode::BlockSearchScope::Output,
        ] {
            assert_eq!(metadata_browse_display(&blank, scope), None);
        }
    }
}
