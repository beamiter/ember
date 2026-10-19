//! Read-only, bounded command inspection. Identities are terminal-owned sequences:
//! shell-supplied ids may be reused and must never retarget an open review.
use super::state::TerminalApp;
use crate::terminal::{CommandRecord, TerminalState};
use eframe::egui;
use std::cell::RefCell;
use std::sync::Arc;

const MAX_REVIEW_COMMANDS: usize = 64;
const MAX_REVIEW_COMMAND_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct BlockReview {
    pub session_id: String,
    pub sequences: Vec<u64>,
    pub active: usize,
    // Bounded original bytes, never escaped previews or mutable shell ids.
    commands: Option<Vec<Option<String>>>,
    armed: bool,
    invoker: Option<egui::Id>,
    error: Option<String>,
    comparison: Option<ReviewComparison>,
    show_comparison: bool,
    text_cache: RefCell<ReviewTextCache>,
    fold_cache: RefCell<Option<(u64, u64, ReviewFoldSummary)>>,
}

impl BlockReview {
    pub(super) fn capture(
        session_id: String,
        selected: &[String],
        terminal: &TerminalState,
    ) -> Option<Self> {
        let selected_set = selected
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        if selected_set.len() != selected.len() {
            return None;
        }
        let records: Vec<_> = terminal
            .command_records()
            .iter()
            .filter(|record| record.complete && selected_set.contains(record.id.as_str()))
            .collect();
        if records.is_empty() || records.len() != selected.len() {
            return None;
        }
        let sequences = records.iter().map(|record| record.sequence).collect();
        let mut bytes = 0usize;
        let bounded = records.len() <= MAX_REVIEW_COMMANDS
            && records.iter().all(|record| {
                bytes = bytes.saturating_add(record.command.as_ref().map_or(0, String::len));
                bytes <= MAX_REVIEW_COMMAND_BYTES
            });
        let commands = bounded.then(|| {
            records
                .iter()
                .map(|record| record.command.clone())
                .collect()
        });
        Some(Self {
            session_id,
            commands,
            sequences,
            active: 0,
            armed: false,
            invoker: None,
            error: None,
            comparison: None,
            show_comparison: false,
            text_cache: RefCell::default(),
            fold_cache: RefCell::default(),
        })
    }

    pub(super) fn matches_insertion_selection(
        &self,
        terminal: &TerminalState,
        ids: &[String],
    ) -> bool {
        self.resolve(terminal).is_some_and(|records| {
            self.command_copy_size(&records).is_ok()
                && records.len() == ids.len()
                && records.iter().zip(ids).all(|(record, id)| record.id == *id)
        })
    }

    fn copy_commands(&self, terminal: &TerminalState) -> Result<String, &'static str> {
        self.copy_from_records(terminal.command_records().iter())
    }

    fn copy_from_records<'a>(
        &self,
        records: impl Iterator<Item = &'a CommandRecord>,
    ) -> Result<String, &'static str> {
        let records = self.resolve_from_records(records).ok_or(
            "A reviewed block is no longer retained or its order changed. Close and review again; nothing copied.",
        )?;
        let bytes = self.command_copy_size(&records)?;
        let mut text = String::with_capacity(bytes);
        for command in records
            .iter()
            .filter_map(|record| record.command.as_deref())
        {
            if command.trim().is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(command);
        }
        Ok(text)
    }

    // Validation is also used during painting. Do not build and throw away a
    // 256 KiB clipboard payload on every unchanged review frame.
    fn command_copy_size(&self, records: &[&CommandRecord]) -> Result<usize, &'static str> {
        let Some(commands) = &self.commands else {
            return Err("Review exceeds 64 blocks or 256 KiB of commands. Select a smaller range; nothing copied.");
        };
        if records.len() != self.sequences.len() || commands.len() != records.len() {
            return Err(
                "A reviewed block is no longer retained. Close and review again; nothing copied.",
            );
        }
        let mut bytes = 0usize;
        for ((record, sequence), command) in records.iter().zip(&self.sequences).zip(commands) {
            if record.sequence != *sequence || record.command != *command {
                return Err("Reviewed commands or their order changed. Close and review again; nothing copied.");
            }
            if !record.complete || record.command_truncated {
                return Err("A reviewed command is incomplete or truncated; nothing copied.");
            }
            let Some(command) = command.as_deref() else {
                continue;
            };
            if command.chars().any(|ch| {
                ch != '\n'
                    && (ch.is_control()
                        || ch == '\u{fffd}'
                        || jterm_core::review_input::is_terminal_visual_spoofing_character(ch))
            }) {
                return Err(
                    "A command contains hidden controls or display substitutions; nothing copied.",
                );
            }
            if command.trim().is_empty() {
                continue;
            }
            if !record.command_exact {
                return Err(
                    "Exact command text is unavailable for part of the selection; nothing copied.",
                );
            }
            let added = command.len().saturating_add(usize::from(bytes != 0));
            if added > MAX_REVIEW_COMMAND_BYTES.saturating_sub(bytes) {
                return Err("Selected commands exceed the 256 KiB copy limit; nothing copied.");
            }
            bytes += added;
        }
        if bytes == 0 {
            Err("These blocks contain no commands; nothing copied.")
        } else {
            Ok(bytes)
        }
    }

    fn resolve<'a>(&self, terminal: &'a TerminalState) -> Option<Vec<&'a CommandRecord>> {
        self.resolve_from_records(terminal.command_records().iter())
    }

    fn compare_output(&mut self, terminal: &TerminalState) -> Result<(), &'static str> {
        let records = self.resolve(terminal).ok_or(
            "A reviewed block is no longer retained. Close and review the available blocks again.",
        )?;
        let comparison = ReviewComparison::capture(&records)?;
        self.comparison = Some(comparison);
        self.show_comparison = true;
        Ok(())
    }

    /// Both lists use terminal-owned monotonic sequences. Merge once instead
    /// of looking up every selected block from the beginning of the history.
    fn resolve_from_records<'a>(
        &self,
        records: impl Iterator<Item = &'a CommandRecord>,
    ) -> Option<Vec<&'a CommandRecord>> {
        if self.sequences.is_empty() || self.sequences.windows(2).any(|pair| pair[0] >= pair[1]) {
            return None;
        }
        let mut resolved = Vec::with_capacity(self.sequences.len());
        let mut targets = self.sequences.iter();
        let mut target = *targets.next()?;
        for record in records {
            if record.sequence < target {
                continue;
            }
            if record.sequence != target || !record.complete {
                return None;
            }
            resolved.push(record);
            let Some(next) = targets.next() else {
                return Some(resolved);
            };
            target = *next;
        }
        None
    }

    fn fold_summary(
        &self,
        terminal: &TerminalState,
        policy: &crate::terminal::ProjectionPolicy,
    ) -> ReviewFoldSummary {
        let source = (policy.revision(), terminal.finished_output_revision());
        if source.1 != 0 {
            if let Some((policy_revision, output_revision, summary)) = *self.fold_cache.borrow() {
                if (policy_revision, output_revision) == source {
                    return summary;
                }
            }
        }
        let ranges = terminal.finished_output_ranges(&self.sequences);
        let mut summary = ReviewFoldSummary::default();
        for sequence in &self.sequences {
            if policy.is_collapsed(*sequence) {
                summary.collapsed += 1;
            } else if ranges.contains_key(sequence) {
                summary.collapsible += 1;
            } else {
                summary.unavailable += 1;
            }
        }
        *self.fold_cache.borrow_mut() = (source.1 != 0).then_some((source.0, source.1, summary));
        summary
    }
}

#[derive(Clone, Debug)]
struct ReviewComparisonSource {
    sequence: u64,
    command: Option<String>,
    command_exact: bool,
    command_truncated: bool,
    cwd: Option<String>,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    state: crate::terminal::CommandState,
    finished_at: Option<std::time::SystemTime>,
    start_mark_seen: bool,
    completion_provenance: crate::block_mode::CompletionProvenance,
    output: crate::terminal::ExtractedText,
}

impl ReviewComparisonSource {
    fn capture(record: &CommandRecord) -> Result<Self, &'static str> {
        let output = record.captured_output.as_ref().ok_or(
            "Captured output is unavailable for one of these blocks. Missing output cannot be compared as empty output.",
        )?;
        if output.text.len() > crate::terminal::MAX_COMPLETED_COMMAND_OUTPUT_BYTES {
            return Err("Captured output exceeds the comparison limit.");
        }
        Ok(Self {
            sequence: record.sequence,
            command: record.command.clone(),
            command_exact: record.command_exact,
            command_truncated: record.command_truncated,
            cwd: record.cwd.clone(),
            exit_code: record.exit_code,
            duration_ms: record.duration_ms,
            state: record.state,
            finished_at: record.finished_at,
            start_mark_seen: record.start_mark_seen,
            completion_provenance: record.completion_provenance,
            output: output.clone(),
        })
    }

    fn matches(&self, record: &CommandRecord) -> bool {
        record.complete
            && self.sequence == record.sequence
            && self.command == record.command
            && self.command_exact == record.command_exact
            && self.command_truncated == record.command_truncated
            && self.cwd == record.cwd
            && self.exit_code == record.exit_code
            && self.duration_ms == record.duration_ms
            && self.state == record.state
            && self.finished_at == record.finished_at
            && self.start_mark_seen == record.start_mark_seen
            && self.completion_provenance == record.completion_provenance
            && record.captured_output.as_ref() == Some(&self.output)
    }
}

#[derive(Clone, Debug)]
struct ReviewComparisonDisplay {
    before: Arc<str>,
    after: Arc<str>,
    summary: Arc<str>,
    text: Arc<str>,
    lines: Arc<Vec<std::ops::Range<usize>>>,
    complete: bool,
}

#[derive(Clone, Debug)]
struct ReviewComparison {
    sources: [ReviewComparisonSource; 2],
    display: ReviewComparisonDisplay,
}

fn comparison_issue(records: &[&CommandRecord]) -> Option<&'static str> {
    if records.len() != 2 {
        Some("Select exactly two completed command blocks to compare their captured output.")
    } else if !records.iter().all(|record| record.complete)
        || records[0].sequence >= records[1].sequence
    {
        Some("The comparison needs two distinct completed blocks in terminal order.")
    } else if records.iter().any(|record| {
        !record.command_truncated
            && record
                .command
                .as_deref()
                .is_none_or(|command| command.trim().is_empty())
    }) {
        Some("Output comparison needs two command blocks; background-only blocks have no command to compare.")
    } else if records
        .iter()
        .any(|record| record.captured_output.is_none())
    {
        Some("Captured output is unavailable for one of these blocks. Missing output cannot be compared as empty output.")
    } else {
        None
    }
}

fn comparison_heading(label: &str, record: &CommandRecord) -> Arc<str> {
    format!(
        "{label} · block #{} · {}\n{}\n{}",
        record.sequence,
        super::rendering::block_workspace_status(record, false).0,
        display_text_bounded(
            record
                .command
                .as_deref()
                .unwrap_or("Command text unavailable"),
            1024
        ),
        display_text_bounded(
            record
                .cwd
                .as_deref()
                .unwrap_or("Working directory not reported"),
            1024
        ),
    )
    .into()
}

impl ReviewComparison {
    fn capture(records: &[&CommandRecord]) -> Result<Self, &'static str> {
        if let Some(issue) = comparison_issue(records) {
            return Err(issue);
        }
        let sources = [
            ReviewComparisonSource::capture(records[0])?,
            ReviewComparisonSource::capture(records[1])?,
        ];
        let result = jterm_core::output_compare::compare(
            jterm_core::output_compare::OutputSnapshot {
                text: &sources[0].output.text,
                truncated: sources[0].output.truncated
                    || sources[0].output.total_bytes > sources[0].output.text.len(),
            },
            jterm_core::output_compare::OutputSnapshot {
                text: &sources[1].output.text,
                truncated: sources[1].output.truncated
                    || sources[1].output.total_bytes > sources[1].output.text.len(),
            },
        );
        let complete = result.is_complete();
        let summary = result.summary().into();
        let text: Arc<str> = result.text.into();
        let mut start = 0;
        let lines = text
            .split_inclusive('\n')
            .map(|line| {
                let range = start..start + line.trim_end_matches('\n').len();
                start += line.len();
                range
            })
            .collect();
        Ok(Self {
            sources,
            display: ReviewComparisonDisplay {
                before: comparison_heading("Before", records[0]),
                after: comparison_heading("After", records[1]),
                summary,
                text,
                lines: Arc::new(lines),
                complete,
            },
        })
    }

    fn matches(&self, records: &[&CommandRecord]) -> bool {
        records.len() == 2
            && self
                .sources
                .iter()
                .zip(records)
                .all(|(source, record)| source.matches(record))
    }
}

/// At most the active record's bounded text is duplicated. Exact source
/// equality invalidates the display cache even when byte length is unchanged.
#[derive(Clone, Debug, Default)]
struct ReviewTextCache {
    sequence: Option<u64>,
    command: Option<String>,
    cwd: Option<String>,
    output: Option<crate::terminal::ExtractedText>,
    shown_command: Arc<str>,
    shown_cwd: Arc<str>,
    shown_output: Arc<str>,
}

impl ReviewTextCache {
    fn update(&mut self, record: &CommandRecord) {
        let changed_record = self.sequence != Some(record.sequence);
        if changed_record || self.command != record.command {
            self.shown_command = display_text(
                record
                    .command
                    .as_deref()
                    .unwrap_or("No command text retained"),
            )
            .into();
            self.command.clone_from(&record.command);
        }
        if changed_record || self.cwd != record.cwd {
            self.shown_cwd = display_text(
                record
                    .cwd
                    .as_deref()
                    .unwrap_or("Working directory not reported"),
            )
            .into();
            self.cwd.clone_from(&record.cwd);
        }
        if changed_record || self.output != record.captured_output {
            self.shown_output = record
                .captured_output
                .as_ref()
                .map(|output| display_text(&output.text))
                .unwrap_or_default()
                .into();
            self.output.clone_from(&record.captured_output);
        }
        self.sequence = Some(record.sequence);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ReviewFoldSummary {
    collapsed: usize,
    collapsible: usize,
    unavailable: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReviewFoldResult {
    pub changed: usize,
    pub unavailable: usize,
}

/// Resolve every captured identity before any policy mutation. Folding remains
/// available for review-only sessions and inexact commands: it never writes to
/// the PTY or removes raw output. Full-screen apps retain their own geometry.
pub(super) fn set_reviewed_output_collapsed(
    review: &BlockReview,
    terminal: &mut TerminalState,
    policy: &mut crate::terminal::ProjectionPolicy,
    collapsed: bool,
) -> Result<ReviewFoldResult, &'static str> {
    if terminal.is_alt_buffer_active() {
        return Err("Block output controls resume after the full-screen app closes.");
    }
    let records = review.resolve(terminal).ok_or("A selected block is no longer retained. Select the available blocks again; no output changed.")?;
    let ranges = collapsed.then(|| terminal.finished_output_ranges(&review.sequences));
    let targets = records
        .iter()
        .filter(|record| {
            !collapsed
                || policy.is_collapsed(record.sequence)
                || ranges
                    .as_ref()
                    .is_some_and(|ranges| ranges.contains_key(&record.sequence))
        })
        .map(|record| record.sequence)
        .collect::<Vec<_>>();
    let unavailable = review.sequences.len() - targets.len();
    let changed = policy.set_collapsed_batch(&targets, collapsed);
    if changed != 0 {
        terminal.clear_text_selection();
    }
    Ok(ReviewFoldResult {
        changed,
        unavailable,
    })
}

/// Preserve meaningful line breaks but expose terminal controls and bidi text.
/// The output and command panes each retain at most 256 KiB of display text.
fn display_text(text: &str) -> String {
    display_text_bounded(text, MAX_REVIEW_COMMAND_BYTES)
}

fn display_text_bounded(text: &str, limit: usize) -> String {
    use std::fmt::Write as _;

    // This runs on every review frame while the terminal is locked. Write
    // scalars directly instead of allocating a temporary String per character.
    let mut shown = String::with_capacity(text.len().min(limit));
    for character in text.chars() {
        let escape = character != '\n'
            && (character.is_control()
                || jterm_core::review_input::is_terminal_visual_spoofing_character(character));
        let part_len = if escape {
            // `\\u{HEX}` has four punctuation bytes plus at least one digit.
            4 + (character as u32).checked_ilog(16).unwrap_or(0) as usize + 1
        } else {
            character.len_utf8()
        };
        if part_len > limit.saturating_sub(shown.len()) {
            while "…".len() > limit.saturating_sub(shown.len()) && !shown.is_empty() {
                shown.pop();
            }
            if "…".len() <= limit.saturating_sub(shown.len()) {
                shown.push('…');
            }
            break;
        }
        if escape {
            write!(shown, "\\u{{{:X}}}", character as u32).expect("writing to String cannot fail");
        } else {
            shown.push(character);
        }
    }
    shown
}

#[derive(Default)]
pub(crate) struct ReadingHistory {
    seen: std::collections::HashMap<String, u64>,
}

impl ReadingHistory {
    pub fn retain_sessions<'a>(&mut self, sessions: impl Iterator<Item = &'a str>) {
        let live = sessions.collect::<std::collections::HashSet<_>>();
        self.seen
            .retain(|session, _| live.contains(session.as_str()));
    }

    pub fn update(
        &mut self,
        session_id: &str,
        records: &std::collections::VecDeque<CommandRecord>,
        reading: bool,
    ) -> usize {
        let newest = records
            .iter()
            .filter(|r| r.complete)
            .map(|r| r.sequence)
            .max()
            .unwrap_or(0);
        let seen = self.seen.entry(session_id.to_owned()).or_insert(newest);
        if !reading {
            *seen = newest;
        }
        records
            .iter()
            .filter(|r| {
                r.complete
                    && r.sequence > *seen
                    && (r.command_truncated
                        || r.command.as_ref().is_some_and(|c| !c.trim().is_empty()))
            })
            .count()
    }
}

#[derive(Default)]
struct ReviewView {
    rows: Vec<String>,
    summary: String,
    command: Arc<str>,
    cwd: Arc<str>,
    status: String,
    provenance: String,
    output: Arc<str>,
    notice: String,
    ready: bool,
    insertion_issue: Option<String>,
    copy_issue: Option<String>,
    unavailable: bool,
    fold: ReviewFoldSummary,
    alternate_screen: bool,
    comparison: Option<ReviewComparisonDisplay>,
    comparison_issue: Option<String>,
    comparison_stale: bool,
    export_busy: bool,
}

fn review_view(
    review: &BlockReview,
    terminal: &TerminalState,
    writable: bool,
    policy: &crate::terminal::ProjectionPolicy,
) -> ReviewView {
    let Some(records) = review.resolve(terminal) else {
        return ReviewView {
            unavailable: true,
            ..Default::default()
        };
    };
    let record = records[review.active.min(records.len() - 1)];
    let mut text_cache = review.text_cache.borrow_mut();
    text_cache.update(record);
    let output = record.captured_output.as_ref();
    let notice = match output {
        None => {
            "Output snapshot unavailable or evicted. No output has been reconstructed.".to_owned()
        }
        Some(output) if output.truncated => format!(
            "Bounded snapshot: {} bytes including omission markers; original normalized output: {} bytes. Some output was not retained.",
            output.text.len(),
            output.total_bytes
        ),
        Some(output) => format!("Retained output snapshot · {} bytes", output.text.len()),
    };
    let exact = if record.command_truncated {
        "Command text was truncated; insertion is unavailable."
    } else if record.command_exact {
        "Exact shell-reported command"
    } else {
        "Screen-derived or unavailable command; insertion requires exact shell metadata."
    };
    let copy_issue = review.command_copy_size(&records).err().map(str::to_owned);
    let insertion_issue = super::commands::review_replay_error(&records).or_else(|| {
        copy_issue
            .as_ref()
            .map(|issue| issue.replace("nothing copied", "nothing inserted"))
    });
    let comparison = review
        .comparison
        .as_ref()
        .filter(|comparison| comparison.matches(&records));
    ReviewView {
        summary: format!(
            "{} commands · {} failures · {} background blocks",
            records
                .iter()
                .filter(|r| r.command.as_ref().is_some_and(|c| !c.trim().is_empty()))
                .count(),
            records
                .iter()
                .filter(|r| matches!(
                    crate::block_mode::classify_outcome(
                        r.command.as_deref(),
                        r.command_truncated,
                        r.exit_code,
                        r.state,
                        r.complete,
                        false
                    ),
                    crate::block_mode::BlockOutcome::Failed(_)
                ))
                .count(),
            records
                .iter()
                .filter(|r| r.command.as_ref().is_none_or(|c| c.trim().is_empty())
                    && !r.command_truncated)
                .count()
        ),
        rows: records
            .iter()
            .enumerate()
            .map(|(index, r)| {
                format!(
                    "{}. {}",
                    index + 1,
                    display_text_bounded(r.command.as_deref().unwrap_or("Background output"), 160)
                )
            })
            .collect(),
        command: Arc::clone(&text_cache.shown_command),
        cwd: Arc::clone(&text_cache.shown_cwd),
        status: super::rendering::block_workspace_status(record, false).0,
        provenance: format!(
            "{}\n{}\nStable block #{}",
            exact,
            crate::block_mode::lifecycle_detail(
                record.start_mark_seen,
                record.completion_provenance
            ),
            record.sequence
        ),
        output: Arc::clone(&text_cache.shown_output),
        notice,
        ready: writable
            && !terminal.is_alt_buffer_active()
            && terminal.shell_is_prompt_ready()
            && terminal.prompt_input_is_empty()
            && terminal.is_bracketed_paste_enabled()
            && insertion_issue.is_none(),
        insertion_issue,
        copy_issue,
        unavailable: false,
        fold: review.fold_summary(terminal, policy),
        alternate_screen: terminal.is_alt_buffer_active(),
        comparison: comparison.map(|comparison| comparison.display.clone()),
        comparison_issue: comparison_issue(&records).map(str::to_owned),
        comparison_stale: review.comparison.is_some() && comparison.is_none(),
        export_busy: false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReviewAction {
    Close,
    Copy,
    Insert,
    Collapse,
    Expand,
    Compare,
    CopyComparison,
    ExportMarkdown,
    ExportJson,
}

fn draw_review_comparison(ui: &mut egui::Ui, comparison: &ReviewComparisonDisplay) {
    ui.separator();
    ui.heading("Output comparison");
    ui.add(
        egui::Label::new(comparison.before.as_ref())
            .selectable(true)
            .wrap(),
    );
    ui.add(
        egui::Label::new(comparison.after.as_ref())
            .selectable(true)
            .wrap(),
    );
    if comparison.complete {
        ui.strong(comparison.summary.as_ref());
    } else {
        ui.colored_label(ui.visuals().warn_fg_color, comparison.summary.as_ref());
    }
    ui.weak("− removed from the earlier run · + added in the later run");
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    egui::ScrollArea::both()
        .id_salt("review-comparison-output")
        .max_height(300.0)
        .show_rows(ui, row_height, comparison.lines.len(), |ui, range| {
            for index in range {
                let line = &comparison.text[comparison.lines[index].clone()];
                let color = match (line.as_bytes().first(), ui.visuals().dark_mode) {
                    (Some(b'+'), true) => egui::Color32::from_rgb(100, 205, 130),
                    (Some(b'+'), false) => egui::Color32::from_rgb(25, 115, 65),
                    (Some(b'-'), true) => egui::Color32::from_rgb(235, 130, 130),
                    (Some(b'-'), false) => egui::Color32::from_rgb(170, 35, 35),
                    _ => ui.visuals().text_color(),
                };
                ui.add(
                    egui::Label::new(egui::RichText::new(line).monospace().color(color))
                        .selectable(true),
                );
            }
        });
}

fn draw_review_order(ui: &mut egui::Ui, review: &mut BlockReview, rows: &[String], armed: bool) {
    egui::ScrollArea::vertical()
        .id_salt("review-order")
        .max_height(120.0)
        .show_rows(ui, 24.0, rows.len(), |ui, range| {
            for index in range {
                if ui
                    .add_enabled_ui(armed, |ui| {
                        ui.add_sized(
                            [ui.available_width(), 24.0],
                            egui::Button::new(&rows[index])
                                .selected(index == review.active)
                                .truncate(),
                        )
                    })
                    .inner
                    .clicked()
                {
                    review.active = index;
                }
            }
        });
}

fn draw_review(
    ctx: &egui::Context,
    review: &mut BlockReview,
    view: &ReviewView,
) -> Option<ReviewAction> {
    let mut action = None;
    // Enter belongs to neither copy nor insertion while reviewing, even when
    // egui synthesizes a click for a keyboard-focused button.
    let enter_pressed = ctx.input(|input| input.key_pressed(egui::Key::Enter));
    let displayed_index = review.active;
    let armed = review.armed;
    let size = ctx.content_rect().size();
    let response = egui::Modal::new(egui::Id::new("block-review")).show(ctx, |ui| {
        ui.set_width((size.x - 48.0).clamp(80.0, 760.0));
        ui.heading("Review command blocks");
        ui.weak(format!("{} selected · terminal order · nothing runs here", review.sequences.len()));
        if !view.unavailable { ui.label(&view.summary); }
        ui.separator();
        if view.unavailable {
            ui.colored_label(ui.visuals().warn_fg_color, "A reviewed block is no longer retained. Close and select the available blocks again.");
        } else {
            egui::ScrollArea::vertical().id_salt(("review-body", review.show_comparison)).max_height((size.y - if size.x < 480.0 { 350.0 } else { 320.0 }).max(60.0)).show(ui, |ui| {
                if view.comparison.is_some() {
                    ui.horizontal_wrapped(|ui| {
                        ui.selectable_value(&mut review.show_comparison, false, "Block output");
                        ui.selectable_value(&mut review.show_comparison, true, "Comparison");
                    });
                }
                if let Some(comparison) = view.comparison.as_ref().filter(|_| review.show_comparison) {
                    draw_review_comparison(ui, comparison);
                } else {
                    if view.rows.len() > 1 {
                        if size.x < 480.0 {
                            ui.collapsing("Selection order", |ui| draw_review_order(ui, review, &view.rows, armed));
                        } else {
                            ui.strong("Selection order");
                            draw_review_order(ui, review, &view.rows, armed);
                        }
                        ui.separator();
                    }
                    ui.horizontal_wrapped(|ui| {
                        if ui.add_enabled(armed && displayed_index > 0, egui::Button::new("Previous block")).clicked() { review.active = displayed_index - 1; }
                        if ui.add_enabled(armed && displayed_index + 1 < view.rows.len(), egui::Button::new("Next block")).clicked() { review.active = displayed_index + 1; }
                        ui.strong(format!("{} / {}", displayed_index + 1, view.rows.len()));
                    });
                    ui.label(&view.status);
                    ui.add(egui::Label::new(egui::RichText::new(view.command.as_ref()).monospace()).selectable(true).wrap());
                    ui.add(egui::Label::new(egui::RichText::new(view.cwd.as_ref()).weak()).selectable(true).wrap());
                    ui.collapsing("Command provenance", |ui| { ui.label(&view.provenance); });
                    ui.separator();
                    if view.comparison_stale {
                        ui.colored_label(ui.visuals().warn_fg_color, "The compared snapshots changed or were evicted. Compare the retained output again.");
                    }
                    ui.strong("Captured output");
                    ui.weak(&view.notice);
                    ui.small("Display capped at 256 KiB; control and bidi characters are shown escaped.");
                    if !view.output.is_empty() {
                        ui.add(egui::Label::new(egui::RichText::new(view.output.as_ref()).monospace()).selectable(true).wrap());
                    }
                }
            });
        }
        ui.separator();
        if let Some(error) = &review.error { ui.colored_label(ui.visuals().warn_fg_color, error); }
        if let Some(issue) = &view.copy_issue { ui.weak(issue); }
        if !view.ready && !view.unavailable && view.copy_issue.is_none() { ui.weak(view.insertion_issue.as_deref().unwrap_or("Insertion needs an empty, idle bracketed-paste prompt with no pending input.")); }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Close review").clicked() && armed { action = Some(ReviewAction::Close); }
            if ui.add_enabled(armed && view.copy_issue.is_none() && !view.unavailable, egui::Button::new("Copy commands")).on_disabled_hover_text(view.copy_issue.as_deref().unwrap_or("Reviewed blocks are unavailable")).clicked() && !enter_pressed { action = Some(ReviewAction::Copy); }
            if ui.add_enabled(armed && view.ready && !view.unavailable, egui::Button::new("Insert at prompt")).clicked() && !enter_pressed { action = Some(ReviewAction::Insert); }
        });
        if !view.unavailable {
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(armed && view.comparison_issue.is_none(), egui::Button::new("Compare output")).on_disabled_hover_text(view.comparison_issue.as_deref().unwrap_or("Select two completed command blocks")).clicked() && !enter_pressed {
                    action = Some(ReviewAction::Compare);
                }
                if ui.add_enabled(armed && view.comparison.is_some(), egui::Button::new("Copy comparison")).clicked() && !enter_pressed {
                    action = Some(ReviewAction::CopyComparison);
                }
                if ui.add_enabled(armed && !view.export_busy, egui::Button::new("Export selected Markdown")).clicked() && !enter_pressed {
                    action = Some(ReviewAction::ExportMarkdown);
                }
                if ui.add_enabled(armed && !view.export_busy, egui::Button::new("Export selected JSON")).clicked() && !enter_pressed {
                    action = Some(ReviewAction::ExportJson);
                }
            });
            if view.export_busy {
                ui.weak("An export is in progress. Its completion and path appear in the status bar.");
            }
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(armed && !view.alternate_screen && view.fold.collapsible > 0, egui::Button::new("Collapse selected output")).on_hover_text("Collapse retained output for this exact selection. Captured output and commands remain available.").clicked() && !enter_pressed {
                    action = Some(ReviewAction::Collapse);
                }
                if ui.add_enabled(armed && !view.alternate_screen && view.fold.collapsed > 0, egui::Button::new("Expand selected output")).clicked() && !enter_pressed {
                    action = Some(ReviewAction::Expand);
                }
            });
            if view.alternate_screen {
                ui.weak("Output controls resume after the full-screen app closes.");
            } else {
                let mut status = format!("{} collapsed · {} can collapse", view.fold.collapsed, view.fold.collapsible);
                if view.fold.unavailable > 0 {
                    status.push_str(&format!(" · {} without exact retained output", view.fold.unavailable));
                }
                ui.weak(status);
            }
        }
        ui.weak("Insertion fills the current shell prompt without changing folder. Review it there before Enter. Esc closes review.");
    });
    if armed && response.should_close() {
        action = Some(ReviewAction::Close);
    }
    review.armed = true;
    action
}

fn restore_review_invoker(ctx: &egui::Context, invoker: Option<egui::Id>) -> bool {
    if let Some(id) = invoker.filter(|id| {
        // The modal temporarily disables the underlying workspace. Presence,
        // rather than this frame's enabled flag, determines restoration.
        ctx.read_response(*id)
            .is_some_and(|response| response.rect.is_positive())
    }) {
        ctx.memory_mut(|memory| memory.request_focus(id));
        true
    } else {
        false
    }
}

fn copy_review_commands(
    ctx: &egui::Context,
    review: &BlockReview,
    terminal: &TerminalState,
) -> Result<(), &'static str> {
    let text = review.copy_commands(terminal)?;
    ctx.copy_text(text);
    Ok(())
}

fn copy_review_comparison(
    ctx: &egui::Context,
    review: &BlockReview,
    terminal: &TerminalState,
) -> Result<(), &'static str> {
    let records = review
        .resolve(terminal)
        .ok_or("A compared block is no longer retained. Compare again before copying.")?;
    let comparison = review
        .comparison
        .as_ref()
        .filter(|comparison| comparison.matches(&records))
        .ok_or(
            "The compared snapshots changed. Compare the retained output again before copying.",
        )?;
    let display = &comparison.display;
    ctx.copy_text(format!(
        "{}\n\n{}\n\n{}\n\n{}",
        display.before, display.after, display.summary, display.text,
    ));
    Ok(())
}

impl TerminalApp {
    pub(crate) fn block_compare_selected(&mut self) {
        let Some(mut review) = self.capture_selected_output_review() else {
            self.set_status("Select two completed command blocks in this pane to compare output.");
            return;
        };
        let session = &self.session_manager.sessions()[self.session_manager.active_index()];
        if let Err(error) = review.compare_output(&session.terminal.lock()) {
            review.error = Some(error.to_owned());
        }
        self.block_review = Some(review);
        self.claim_enter_for_prompt_recall();
    }

    pub(crate) fn block_export_selected(
        &mut self,
        format: crate::block_export::SessionExportFormat,
    ) {
        let Some(review) = self.capture_selected_output_review() else {
            self.set_status("Select retained completed blocks in this pane before exporting.");
            return;
        };
        match self.start_review_export(&review, format) {
            Ok(()) => self.set_status(format!("Exporting {} selected blocks…", format.label())),
            Err(error) => self.set_status(error),
        }
    }

    fn start_review_export(
        &mut self,
        review: &BlockReview,
        format: crate::block_export::SessionExportFormat,
    ) -> Result<(), String> {
        if self.pending_session_export.is_some() {
            return Err("An export is already in progress.".to_owned());
        }
        let session = self
            .session_manager
            .sessions()
            .get(self.session_manager.active_index())
            .filter(|session| {
                session.metadata.session_id == review.session_id
                    && self.config.command_cards_enabled()
            })
            .ok_or("The reviewed pane is no longer active; nothing exported.")?;
        let snapshot = crate::block_export::snapshot_selected(
            &session.terminal.lock(),
            &review.session_id,
            &review.sequences,
        )
        .map_err(|error| error.to_string())?;
        self.pending_session_export = Some(
            crate::block_export::start_session_export(snapshot, format)
                .map_err(|error| format!("Could not start export: {error}"))?,
        );
        Ok(())
    }

    fn capture_selected_output_review(&self) -> Option<BlockReview> {
        if !self.config.command_cards_enabled() {
            return None;
        }
        let session = self
            .session_manager
            .sessions()
            .get(self.session_manager.active_index())?;
        let selection = self.block_selection.as_ref().filter(|selection| {
            selection.session_id == session.metadata.session_id
                && selection.selected_ids.contains(&selection.active_id)
        })?;
        let terminal = session.terminal.lock();
        if terminal.is_alt_buffer_active() {
            return None;
        }
        BlockReview::capture(
            selection.session_id.clone(),
            &selection.selected_ids,
            &terminal,
        )
    }

    pub(super) fn selected_output_action_available(&self, collapsed: bool) -> bool {
        let Some(review) = self.capture_selected_output_review() else {
            return false;
        };
        let session = &self.session_manager.sessions()[self.session_manager.active_index()];
        let summary = review.fold_summary(&session.terminal.lock(), &session.projection_policy);
        if collapsed {
            summary.collapsible > 0
        } else {
            summary.collapsed > 0
        }
    }

    pub(crate) fn block_set_selected_output_collapsed(&mut self, collapsed: bool) {
        let Some(review) = self.capture_selected_output_review() else {
            self.set_status(
                "Select retained completed blocks in this pane before changing output.",
            );
            return;
        };
        let message = self.apply_reviewed_output_state(&review, collapsed);
        self.set_status(message);
    }

    fn apply_reviewed_output_state(&mut self, review: &BlockReview, collapsed: bool) -> String {
        if !self.config.command_cards_enabled() {
            return "Block output controls require Block Mode.".to_owned();
        }
        let active = self.session_manager.active_index();
        let Some(session) = self
            .session_manager
            .sessions_mut()
            .get_mut(active)
            .filter(|session| session.metadata.session_id == review.session_id)
        else {
            return "The selected pane is no longer active; no output changed.".to_owned();
        };
        let result = set_reviewed_output_collapsed(
            review,
            &mut session.terminal.lock(),
            &mut session.projection_policy,
            collapsed,
        );
        let result = match result {
            Ok(result) => result,
            Err(error) => return error.to_owned(),
        };
        if result.changed > 0 {
            self.smooth_scroll_velocity = 0.0;
            self.smooth_scroll_pixel_offset = 0.0;
            if self.search_state.is_open {
                self.reveal_current_search_match();
            }
        }
        format!(
            "{} {} selected block outputs{}",
            if collapsed { "Collapsed" } else { "Expanded" },
            result.changed,
            if result.unavailable == 0 {
                String::new()
            } else {
                format!(
                    " · {} without exact retained output skipped",
                    result.unavailable
                )
            }
        )
    }

    pub(super) fn remember_block_review_invoker(&mut self, ctx: &egui::Context) {
        if !self.block_chrome_owns_keyboard(ctx) {
            return;
        }
        if let Some(review) = self
            .block_review
            .as_mut()
            .filter(|review| !review.armed && review.invoker.is_none())
        {
            review.invoker = ctx.memory(|memory| memory.focused());
        }
    }

    pub(crate) fn open_block_review(&mut self) {
        let session = self.session_manager.get_active_session_mut();
        let Some(selection) = self
            .block_selection
            .as_ref()
            .filter(|s| s.session_id == session.metadata.session_id)
        else {
            return;
        };
        self.block_review = BlockReview::capture(
            selection.session_id.clone(),
            &selection.selected_ids,
            &session.terminal.lock(),
        );
        if self.block_review.is_some() {
            // A keyboard-opened review owns its opening Enter through release;
            // repeats must not click a newly focused modal control.
            self.claim_enter_for_prompt_recall();
        }
    }

    pub(crate) fn render_block_review(&mut self, ctx: &egui::Context) {
        let Some(mut review) = self.block_review.take() else {
            return;
        };
        let active = self.session_manager.active_index();
        let Some(session) = self.session_manager.sessions().get(active).filter(|s| {
            s.metadata.session_id == review.session_id && self.config.command_cards_enabled()
        }) else {
            self.return_focus_to_terminal(ctx);
            return;
        };
        if !review.armed && review.invoker.is_none() && self.block_chrome_owns_keyboard(ctx) {
            review.invoker = ctx.memory(|memory| memory.focused());
        }
        let mut view = review_view(
            &review,
            &session.terminal.lock(),
            session.purpose != crate::session::SessionPurpose::RetainedCommand
                && session.pending_input.is_empty()
                && !self.direct_input_is_blocked_for_session(&review.session_id),
            &session.projection_policy,
        );
        view.export_busy = self.pending_session_export.is_some();
        let draw_action = draw_review(ctx, &mut review, &view);
        match draw_action {
            Some(ReviewAction::Close) => {
                egui::Popup::close_all(ctx);
                if view.unavailable || !restore_review_invoker(ctx, review.invoker) {
                    self.return_focus_to_terminal(ctx);
                }
            }
            Some(ReviewAction::Copy) => {
                // Revalidate all retained identities and bytes before a single
                // clipboard write. This path cannot queue input or touch the PTY.
                match copy_review_commands(ctx, &review, &session.terminal.lock()) {
                    Ok(()) => {
                        review.error = Some("Commands copied. Nothing inserted or run.".into())
                    }
                    Err(error) => review.error = Some(error.into()),
                }
                self.block_review = Some(review);
            }
            Some(ReviewAction::Compare) => {
                review.error = review
                    .compare_output(&session.terminal.lock())
                    .err()
                    .map(str::to_owned);
                self.block_review = Some(review);
                ctx.request_repaint();
            }
            Some(ReviewAction::CopyComparison) => {
                review.error = Some(
                    match copy_review_comparison(ctx, &review, &session.terminal.lock()) {
                        Ok(()) => "Comparison copied.".to_owned(),
                        Err(error) => error.to_owned(),
                    },
                );
                self.block_review = Some(review);
            }
            Some(ReviewAction::ExportMarkdown | ReviewAction::ExportJson) => {
                let format = if matches!(draw_action, Some(ReviewAction::ExportMarkdown)) {
                    crate::block_export::SessionExportFormat::Markdown
                } else {
                    crate::block_export::SessionExportFormat::Json
                };
                review.error = Some(match self.start_review_export(&review, format) {
                    Ok(()) => format!("Exporting {} selected blocks. The saved path will appear in the status bar.", format.label()),
                    Err(error) => error,
                });
                self.block_review = Some(review);
                ctx.request_repaint();
            }
            Some(ReviewAction::Insert) => {
                // Resolve every sequence again at action time. Never use a stale
                // display snapshot or a shell-controlled id to authorize insertion.
                let ids = {
                    let terminal = session.terminal.lock();
                    review
                        .copy_commands(&terminal)
                        .ok()
                        .and_then(|_| review.resolve(&terminal))
                        .map(|records| records.iter().map(|r| r.id.clone()).collect::<Vec<_>>())
                };
                if let Some(ids) = ids {
                    let old = self.block_selection.take();
                    self.block_selection =
                        crate::block_mode::BlockSelection::all(review.session_id.clone(), ids);
                    match self.try_reinput_reviewed_commands(&review) {
                        Ok(_) => {
                            self.set_status(
                                "Reviewed commands inserted at prompt; nothing executed",
                            );
                            self.return_focus_to_terminal(ctx);
                            return;
                        }
                        Err(error) => {
                            self.block_selection = old;
                            review.error =
                                Some(super::commands::selected_replay_error_message(&error));
                        }
                    }
                } else {
                    review.error = Some(
                        "A reviewed command changed or is unavailable. Nothing was inserted."
                            .into(),
                    );
                }
                self.block_review = Some(review);
            }
            Some(ReviewAction::Collapse | ReviewAction::Expand) => {
                let collapsed = matches!(draw_action, Some(ReviewAction::Collapse));
                review.error = Some(self.apply_reviewed_output_state(&review, collapsed));
                self.block_review = Some(review);
                ctx.request_repaint();
            }
            None => self.block_review = Some(review),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish(terminal: &mut TerminalState, id: &str, command: &str, exit: i32) {
        let output = if exit == 0 {
            "Build finished successfully\r\n"
        } else {
            "running 42 tests\r\ntest block_layout ... ok\r\ntest narrow_toolbar ... FAILED\r\nExpected controls inside the pane\r\n41 passed; 1 failed\r\n"
        };
        let displayed = command.replace("%20", " ");
        terminal.process_input(format!("\x1b]133;A\x07$ \x1b]133;B;jsh_id={id}\x07{displayed}\r\n\x1b]133;C;jsh_id={id};cmdline_url={command};cwd_url=%2Fhome%2Fdev%2Fember\x07{output}\x1b]133;D;{exit};jsh_id={id}\x07\r\n").as_bytes());
    }

    fn fixture() -> (TerminalState, BlockReview) {
        let mut terminal = TerminalState::new(90, 30);
        finish(&mut terminal, "first", "cargo%20check", 0);
        finish(&mut terminal, "second", "cargo%20test", 101);
        terminal.process_input(b"\x1b[?2004h\x1b]133;A\x07$ \x1b]133;B;jsh_id=prompt\x07");
        let review =
            BlockReview::capture("pane".into(), &["second".into(), "first".into()], &terminal)
                .unwrap();
        (terminal, review)
    }

    #[test]
    fn review_comparison_uses_terminal_order_and_rejects_changed_capture() {
        let (terminal, mut review) = fixture();
        review.compare_output(&terminal).unwrap();
        let compared = review.comparison.as_ref().unwrap();
        assert!(review.show_comparison);
        assert!(compared.display.before.contains("cargo check"));
        assert!(compared.display.after.contains("cargo test"));
        assert!(compared
            .display
            .text
            .contains("Build finished successfully"));
        let mut records = terminal
            .command_records()
            .iter()
            .filter(|record| record.complete)
            .cloned()
            .collect::<Vec<_>>();
        assert!(compared.matches(&records.iter().collect::<Vec<_>>()));
        let original = records[0].captured_output.clone();
        // Equal-length replacement is still a different captured result.
        records[0]
            .captured_output
            .as_mut()
            .unwrap()
            .text
            .replace_range(..5, "Other");
        assert!(!compared.matches(&records.iter().collect::<Vec<_>>()));
        records[0].captured_output = original;
        records[0].cwd = Some("/tmp/another-project".to_owned());
        assert!(!compared.matches(&records.iter().collect::<Vec<_>>()));
        records[1].captured_output = None;
        assert!(ReviewComparison::capture(&records.iter().collect::<Vec<_>>()).is_err());
        assert!(ReviewComparison::capture(&[&records[0]]).is_err());
    }

    #[test]
    fn review_comparison_retains_partial_evidence_and_empty_output_is_available() {
        let (terminal, review) = fixture();
        let mut records = review
            .resolve(&terminal)
            .unwrap()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        records[0].captured_output = Some(crate::terminal::ExtractedText::default());
        records[1].captured_output = Some(crate::terminal::ExtractedText {
            text: "tail\n".to_owned(),
            truncated: true,
            total_bytes: 9000,
        });
        let compared = ReviewComparison::capture(&records.iter().collect::<Vec<_>>()).unwrap();
        assert!(!compared.display.complete);
        assert!(compared.display.text.contains("tail"));
        records[1].captured_output.as_mut().unwrap().truncated = false;
        let compared = ReviewComparison::capture(&records.iter().collect::<Vec<_>>()).unwrap();
        assert!(
            !compared.display.complete,
            "retention byte evidence also discloses omission"
        );
        records[1].sequence = records[0].sequence;
        assert!(ReviewComparison::capture(&records.iter().collect::<Vec<_>>()).is_err());
    }

    #[test]
    fn copied_comparison_is_read_only_and_revalidates_retained_blocks() {
        let ctx = egui::Context::default();
        let (mut terminal, mut review) = fixture();
        terminal.scroll_offset = 4;
        terminal.note_user_input(b"keep draft");
        review.compare_output(&terminal).unwrap();
        let copied = test_frame(&ctx, raw_input(vec![]), |_ui| {
            copy_review_comparison(&ctx, &review, &terminal).unwrap();
        });
        assert!(copied.platform_output.commands.iter().any(|command|
            matches!(command, egui::OutputCommand::CopyText(text)
                if text.contains("Before") && text.contains("After") && text.contains("cargo check"))));
        assert_eq!(terminal.scroll_offset, 4);
        assert!(!terminal.prompt_input_is_empty());
        terminal.clear_completed_blocks();
        let rejected = test_frame(&ctx, raw_input(vec![]), |_ui| {
            assert!(copy_review_comparison(&ctx, &review, &terminal).is_err());
        });
        assert!(!rejected
            .platform_output
            .commands
            .iter()
            .any(|command| matches!(command, egui::OutputCommand::CopyText(_))));
    }

    #[test]
    fn review_compare_and_export_buttons_require_explicit_actions() {
        for (label, expected) in [
            ("Compare output", ReviewAction::Compare),
            ("Export selected Markdown", ReviewAction::ExportMarkdown),
            ("Export selected JSON", ReviewAction::ExportJson),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let (mut terminal, mut review) = fixture();
            terminal.note_user_input(b"keep draft");
            let view = review_view(
                &review,
                &terminal,
                false,
                &crate::terminal::ProjectionPolicy::new(),
            );
            let mut output = None;
            for _ in 0..2 {
                output = Some(test_frame(&ctx, raw_input(vec![]), |_ui| {
                    assert!(draw_review(&ctx, &mut review, &view).is_none());
                }));
            }
            let button = button_node(output.as_ref().unwrap(), label);
            let _ = test_frame(
                &ctx,
                raw_input(vec![action_event(button, egui::accesskit::Action::Focus)]),
                |_ui| {
                    assert!(draw_review(&ctx, &mut review, &view).is_none());
                },
            );
            let _ = test_frame(
                &ctx,
                raw_input(vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }]),
                |_ui| {
                    assert!(draw_review(&ctx, &mut review, &view).is_none());
                },
            );
            let _ = test_frame(
                &ctx,
                raw_input(vec![action_event(button, egui::accesskit::Action::Click)]),
                |_ui| {
                    assert!(draw_review(&ctx, &mut review, &view) == Some(expected));
                },
            );
            assert!(!terminal.prompt_input_is_empty());
        }
    }

    #[test]
    fn review_resolves_stable_terminal_order_without_changing_viewport() {
        let (mut terminal, review) = fixture();
        terminal.scroll_offset = 4;
        let view = review_view(
            &review,
            &terminal,
            true,
            &crate::terminal::ProjectionPolicy::new(),
        );
        assert!(view.rows[0].contains("cargo check"));
        assert!(view.rows[1].contains("cargo test"));
        assert!(view.provenance.contains("Exact shell-reported"));
        assert_eq!(terminal.scroll_offset, 4);
        assert!(view.ready);
        assert!(BlockReview::capture("pane".into(), &["missing".into()], &terminal).is_none());
    }

    #[test]
    fn batch_output_actions_preserve_raw_output_and_only_change_selected_blocks() {
        let (mut terminal, review) = fixture();
        finish(&mut terminal, "unselected", "pwd", 0);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B;jsh_id=live\x07\x1b]133;C;jsh_id=live;cmdline_url=sleep\x07running\r\n");
        let unselected = terminal.command_record("unselected").unwrap().sequence;
        let live = terminal.command_record("live").unwrap().sequence;
        let raw_version = terminal.get_grid_version();
        let snapshots = terminal
            .command_records()
            .iter()
            .map(|record| {
                (
                    record.sequence,
                    record.command.clone(),
                    record.captured_output.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut policy = crate::terminal::ProjectionPolicy::new();
        let revision = policy.revision();
        let result =
            set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, true).unwrap();
        assert_eq!(
            result,
            ReviewFoldResult {
                changed: 2,
                unavailable: 0
            }
        );
        assert_eq!(
            policy.revision(),
            revision + 1,
            "one projection transaction for the batch"
        );
        assert!(review
            .sequences
            .iter()
            .all(|sequence| policy.is_collapsed(*sequence)));
        assert!(!policy.is_collapsed(unselected));
        assert!(!policy.is_collapsed(live));
        let mut state = crate::terminal::ProjectionViewState::new();
        let folded = terminal.projected_viewport_with_state(
            crate::terminal::HistoryProjection::identity(),
            true,
            &policy,
            &mut state,
        );
        assert_eq!(folded.effective_collapsed().len(), 2);
        assert_eq!(review.fold_summary(&terminal, &policy).collapsed, 2);
        assert_eq!(
            set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, true)
                .unwrap()
                .changed,
            0
        );
        assert_eq!(policy.revision(), revision + 1);
        assert_eq!(
            set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, false)
                .unwrap()
                .changed,
            2
        );
        let expanded = terminal.projected_viewport_with_state(
            crate::terminal::HistoryProjection::identity(),
            true,
            &policy,
            &mut state,
        );
        assert!(expanded.effective_collapsed().is_empty());
        assert_eq!(terminal.get_grid_version(), raw_version);
        assert_eq!(
            terminal
                .command_records()
                .iter()
                .map(|record| (
                    record.sequence,
                    record.command.clone(),
                    record.captured_output.clone()
                ))
                .collect::<Vec<_>>(),
            snapshots
        );
        assert_eq!(
            review.copy_commands(&terminal).unwrap(),
            "cargo check\ncargo test"
        );
        assert!(!terminal.command_record("live").unwrap().complete);
    }

    #[test]
    fn batch_output_revalidates_the_whole_identity_set_before_mutating() {
        let (mut terminal, mut review) = fixture();
        let mut policy = crate::terminal::ProjectionPolicy::new();
        policy.collapse(review.sequences[0]);
        let revision = policy.revision();
        review.sequences.push(u64::MAX);
        assert!(set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, false).is_err());
        assert_eq!(policy.revision(), revision);
        assert!(policy.is_collapsed(review.sequences[0]));
        review.sequences.pop();
        terminal.clear_completed_blocks();
        finish(&mut terminal, "first", "replacement", 0);
        finish(&mut terminal, "second", "replacement", 0);
        assert!(set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, true).is_err());
        assert_eq!(policy.revision(), revision);
        assert!(terminal
            .command_records()
            .iter()
            .all(|record| !policy.is_collapsed(record.sequence)));
    }

    #[test]
    fn batch_output_skips_unavailable_ranges_and_pauses_for_alternate_screen() {
        let (mut terminal, _) = fixture();
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B;jsh_id=empty\x07true\r\n\x1b]133;C;jsh_id=empty;cmdline_url=true\x07\x1b]133;D;0;jsh_id=empty\x07");
        let review = BlockReview::capture(
            "pane".into(),
            &["first".into(), "second".into(), "empty".into()],
            &terminal,
        )
        .unwrap();
        let mut policy = crate::terminal::ProjectionPolicy::new();
        assert_eq!(
            review.fold_summary(&terminal, &policy),
            ReviewFoldSummary {
                collapsed: 0,
                collapsible: 2,
                unavailable: 1
            }
        );
        assert_eq!(
            set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, true).unwrap(),
            ReviewFoldResult {
                changed: 2,
                unavailable: 1
            }
        );
        let revision = policy.revision();
        terminal.process_input(b"\x1b[?1049h");
        assert!(set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, false).is_err());
        assert_eq!(policy.revision(), revision);
        terminal.process_input(b"\x1b[?1049l");
        assert_eq!(
            set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, false)
                .unwrap()
                .changed,
            2
        );
    }

    #[test]
    fn sparse_and_large_review_selections_use_one_ordered_scan() {
        let mut terminal = TerminalState::new(80, 12);
        for index in 0..160 {
            finish(&mut terminal, &format!("block-{index}"), "pwd", 0);
        }
        let selected = (0..160)
            .filter(|index| index % 2 == 0)
            .map(|index| format!("block-{index}"))
            .collect::<Vec<_>>();
        let review = BlockReview::capture("pane".into(), &selected, &terminal).unwrap();
        assert!(
            review.commands.is_none(),
            "large review keeps its existing copy budget"
        );
        let visits = std::cell::Cell::new(0);
        let resolved = review
            .resolve_from_records(
                terminal
                    .command_records()
                    .iter()
                    .inspect(|_| visits.set(visits.get() + 1)),
            )
            .unwrap();
        assert_eq!(resolved.len(), 80);
        assert!(visits.get() <= terminal.command_records().len());
        assert_eq!(
            resolved.iter().map(|record| &record.id).collect::<Vec<_>>(),
            selected.iter().collect::<Vec<_>>()
        );
        let mut policy = crate::terminal::ProjectionPolicy::new();
        let result =
            set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, true).unwrap();
        assert!(
            result.changed > MAX_REVIEW_COMMANDS,
            "folding does not inherit the command-copy limit"
        );
        assert!(terminal
            .command_records()
            .iter()
            .filter(|record| !selected.contains(&record.id))
            .all(|record| !policy.is_collapsed(record.sequence)));
        assert!(BlockReview::capture(
            "pane".into(),
            &[selected[0].clone(), selected[0].clone()],
            &terminal
        )
        .is_none());
    }

    #[test]
    fn review_text_cache_reuses_unchanged_output_and_refreshes_equal_length_changes() {
        let (terminal, review) = fixture();
        let policy = crate::terminal::ProjectionPolicy::new();
        let first = review_view(&review, &terminal, false, &policy);
        let second = review_view(&review, &terminal, false, &policy);
        assert!(Arc::ptr_eq(&first.output, &second.output));
        assert!(Arc::ptr_eq(&first.command, &second.command));
        let mut record = terminal.command_record("first").unwrap().clone();
        let mut cache = review.text_cache.borrow_mut();
        let old_len = record.captured_output.as_ref().unwrap().text.len();
        record
            .captured_output
            .as_mut()
            .unwrap()
            .text
            .replace_range(..5, "Other");
        assert_eq!(record.captured_output.as_ref().unwrap().text.len(), old_len);
        cache.update(&record);
        assert!(!Arc::ptr_eq(&first.output, &cache.shown_output));
        assert!(cache.shown_output.starts_with("Other"));
        record.captured_output = None;
        cache.update(&record);
        assert!(
            cache.shown_output.is_empty(),
            "eviction cannot display stale captured bytes"
        );
    }

    #[test]
    fn review_rejects_evicted_identity_even_when_shell_id_is_reused() {
        let (mut terminal, review) = fixture();
        terminal.clear_completed_blocks();
        finish(&mut terminal, "first", "different", 0);
        assert!(review.resolve(&terminal).is_none());
        assert!(
            review_view(
                &review,
                &terminal,
                true,
                &crate::terminal::ProjectionPolicy::new()
            )
            .unavailable
        );
    }

    #[test]
    fn review_is_read_only_for_dirty_busy_alt_or_retained_prompt() {
        let (mut terminal, review) = fixture();
        assert!(
            !review_view(
                &review,
                &terminal,
                false,
                &crate::terminal::ProjectionPolicy::new()
            )
            .ready
        );
        terminal.note_user_input(b"x");
        assert!(
            !review_view(
                &review,
                &terminal,
                true,
                &crate::terminal::ProjectionPolicy::new()
            )
            .ready
        );
        let (mut terminal, review) = fixture();
        terminal.process_input(b"\x1b]133;C;jsh_id=prompt;cmdline_url=sleep\x07");
        assert!(
            !review_view(
                &review,
                &terminal,
                true,
                &crate::terminal::ProjectionPolicy::new()
            )
            .ready
        );
        let (mut terminal, review) = fixture();
        terminal.process_input(b"\x1b[?1049h");
        assert!(
            !review_view(
                &review,
                &terminal,
                true,
                &crate::terminal::ProjectionPolicy::new()
            )
            .ready
        );
    }

    #[test]
    fn display_text_preserves_newlines_and_literal_escapes_without_bidi() {
        assert_eq!(
            display_text("line\n\\n literal\u{202e}"),
            "line\n\\n literal\\u{202E}"
        );
        assert!(display_text(&"界".repeat(100_000)).len() <= 256 * 1024);
    }

    #[test]
    fn bounded_review_display_preserves_every_scalar_and_budget_boundary() {
        fn original(text: &str, limit: usize) -> String {
            let mut shown = String::new();
            for character in text.chars() {
                let part = if character != '\n'
                    && (character.is_control()
                        || jterm_core::review_input::is_terminal_visual_spoofing_character(
                            character,
                        )) {
                    format!("\\u{{{:X}}}", character as u32)
                } else {
                    character.to_string()
                };
                if part.len() > limit.saturating_sub(shown.len()) {
                    while "…".len() > limit.saturating_sub(shown.len()) && !shown.is_empty() {
                        shown.pop();
                    }
                    if "…".len() <= limit.saturating_sub(shown.len()) {
                        shown.push('…');
                    }
                    break;
                }
                shown.push_str(&part);
            }
            shown
        }
        // Exhaust the scalar space, including every escaping-width transition.
        for character in (0..=0x10ffff).filter_map(char::from_u32) {
            let text = character.to_string();
            assert_eq!(display_text_bounded(&text, 12), original(&text, 12));
        }
        for text in [
            "",
            "ascii newline\n",
            "编译🙂",
            "\0\u{f}\u{10}\u{7f}\u{80}\u{9f}",
            "\u{202e}bidi\u{fff9}\u{e0001}",
            "prefix\t尾\n\u{1b}suffix",
        ] {
            for limit in 0..=64 {
                let actual = display_text_bounded(text, limit);
                assert!(actual.len() <= limit);
                assert_eq!(actual, original(text, limit), "{text:?}, budget {limit}");
            }
        }
    }

    #[test]
    fn reading_awareness_only_counts_retained_new_completions_and_never_scrolls() {
        let (mut terminal, _) = fixture();
        let mut state = ReadingHistory::default();
        assert_eq!(state.update("pane", terminal.command_records(), false), 0);
        assert_eq!(state.update("pane", terminal.command_records(), true), 0);
        finish(&mut terminal, "third", "pwd", 0);
        assert_eq!(state.update("pane", terminal.command_records(), true), 1);
        assert_eq!(state.update("pane", terminal.command_records(), true), 1);
        assert_eq!(state.update("pane", terminal.command_records(), false), 0);
        finish(&mut terminal, "fourth", "ls", 0);
        assert_eq!(
            state.update("other-pane", terminal.command_records(), true),
            0
        );
    }

    #[test]
    fn reading_awareness_survives_pane_switch_and_prunes_closed_sessions() {
        let (mut a, _) = fixture();
        let (b, _) = fixture();
        let mut state = ReadingHistory::default();
        state.update("a", a.command_records(), false);
        state.update("b", b.command_records(), false);
        finish(&mut a, "later", "pwd", 0);
        assert_eq!(state.update("a", a.command_records(), true), 1);
        state.retain_sessions(["a"].into_iter());
        assert_eq!(state.seen.len(), 1);
        assert!(state.seen.contains_key("a"));
    }

    #[test]
    fn review_uses_real_replay_validation_for_unsafe_inexact_and_oversized_commands() {
        let (terminal, _) = fixture();
        let mut record = terminal.command_record("first").unwrap().clone();
        record.command = Some("echo \u{202e}unsafe".into());
        assert!(super::super::commands::review_replay_error(&[&record])
            .unwrap()
            .contains("formatting"));
        record.command = Some("pwd".into());
        record.command_exact = false;
        assert!(super::super::commands::review_replay_error(&[&record])
            .unwrap()
            .contains("Exact"));
        record.command_exact = true;
        record.command = Some("x".repeat(60_000));
        let records = (0..5)
            .map(|index| {
                let mut r = record.clone();
                r.id = index.to_string();
                r
            })
            .collect::<Vec<_>>();
        let refs = records.iter().collect::<Vec<_>>();
        assert!(super::super::commands::review_replay_error(&refs)
            .unwrap()
            .contains("262144"));
    }

    fn test_frame(
        ctx: &egui::Context,
        input: egui::RawInput,
        run: impl FnMut(&mut egui::Ui),
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(input, run);
        output.textures_delta.clear();
        output
    }

    fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(360.0, 640.0),
            )),
            events,
            ..Default::default()
        }
    }

    fn action_event(id: egui::accesskit::NodeId, action: egui::accesskit::Action) -> egui::Event {
        egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action,
            target_tree: egui::accesskit::TreeId::ROOT,
            target_node: id,
            data: None,
        })
    }

    fn button_node(output: &egui::FullOutput, label: &str) -> egui::accesskit::NodeId {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find_map(|(id, node)| {
                (node.role() == egui::accesskit::Role::Button && node.label() == Some(label))
                    .then_some(*id)
            })
            .unwrap_or_else(|| panic!("missing button {label}"))
    }

    #[test]
    fn review_cancel_restores_present_invoker_and_rejects_removed_control() {
        let ctx = egui::Context::default();
        let mut id = None;
        let _ = test_frame(&ctx, raw_input(vec![]), |ui| {
            id = Some(ui.add_enabled(false, egui::Button::new("Review")).id);
        });
        assert!(restore_review_invoker(&ctx, id));
        assert_eq!(ctx.memory(|memory| memory.focused()), id);
        for _ in 0..2 {
            let _ = test_frame(&ctx, raw_input(vec![]), |_ui| {});
        }
        assert!(!restore_review_invoker(&ctx, id));
        assert!(!restore_review_invoker(&ctx, None));
    }

    #[test]
    fn review_capture_budget_keeps_inspection_but_disables_partial_actions() {
        let mut terminal = TerminalState::new(90, 30);
        let ids = (0..=MAX_REVIEW_COMMANDS)
            .map(|i| format!("block-{i}"))
            .collect::<Vec<_>>();
        for id in &ids {
            finish(&mut terminal, id, "pwd", 0);
        }
        let review = BlockReview::capture("pane".into(), &ids, &terminal).unwrap();
        assert!(review.resolve(&terminal).is_some());
        assert!(review.copy_commands(&terminal).is_err());
        let view = review_view(
            &review,
            &terminal,
            true,
            &crate::terminal::ProjectionPolicy::new(),
        );
        assert!(!view.unavailable);
        assert!(!view.ready);
        assert!(view.copy_issue.unwrap().contains("64 blocks"));
    }

    #[test]
    fn review_copy_revalidates_every_identity_text_order_and_capture_flag() {
        let (terminal, review) = fixture();
        assert_eq!(
            review.copy_commands(&terminal).unwrap(),
            "cargo check\ncargo test"
        );
        assert!(review.matches_insertion_selection(&terminal, &["first".into(), "second".into()]));
        assert!(!review.matches_insertion_selection(&terminal, &["second".into(), "first".into()]));
        assert!(!review.matches_insertion_selection(&terminal, &["reused".into(), "second".into()]));
        let originals = terminal
            .command_records()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for change in 0..6 {
            let mut records = originals.clone();
            match change {
                0 => {
                    records[0].sequence += 10_000;
                }
                1 => {
                    records[0].command = Some("different".into());
                }
                2 => {
                    records.swap(0, 1);
                }
                3 => {
                    records[0].complete = false;
                }
                4 => {
                    records[0].command_truncated = true;
                }
                _ => {
                    records[0].command_exact = false;
                }
            }
            assert!(
                review.copy_from_records(records.iter()).is_err(),
                "mutation {change}"
            );
        }
    }

    #[test]
    fn review_copy_rejects_controls_bidi_replacement_and_oversize_atomically() {
        let (terminal, original) = fixture();
        for command in [
            "\t",
            "echo\tescaped",
            "echo\rreturn",
            "echo\u{1b}escape",
            "echo\u{7}bell",
            "echo\u{202e}bidi",
            "echo\u{fff9}annotation",
            "echo\u{fffd}replacement",
        ] {
            let mut records = terminal
                .command_records()
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            let mut review = original.clone();
            records[1].command = Some(command.into());
            review.commands.as_mut().unwrap()[1] = Some(command.into());
            assert!(
                review.copy_from_records(records.iter()).is_err(),
                "{command:?}"
            );
            if !command.contains('\u{fffd}') {
                assert_ne!(display_text(command), command);
            }
        }
        let mut records = terminal
            .command_records()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut review = original.clone();
        for (index, record) in records.iter_mut().take(2).enumerate() {
            record.command = Some("x".repeat(MAX_REVIEW_COMMAND_BYTES / 2));
            review.commands.as_mut().unwrap()[index] = record.command.clone();
        }
        assert!(
            review.copy_from_records(records.iter()).is_err(),
            "separator counts in aggregate budget"
        );
        review.commands = None;
        assert!(review.copy_from_records(records.iter()).is_err());
    }

    #[test]
    fn review_copy_preserves_unicode_multiline_and_skips_true_background_only() {
        let (terminal, mut review) = fixture();
        let mut records = terminal
            .command_records()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        records[0].command = Some("printf '编译🙂'\nprintf done".into());
        records[1].command = None;
        review.commands = Some(
            records
                .iter()
                .take(2)
                .map(|record| record.command.clone())
                .collect(),
        );
        assert_eq!(
            review.copy_from_records(records.iter()).unwrap(),
            "printf '编译🙂'\nprintf done"
        );
        records[1].command_truncated = true;
        assert!(
            review.copy_from_records(records.iter()).is_err(),
            "missing truncated command is not background"
        );
        records[1].command_truncated = false;
        records[0].command = None;
        review.commands.as_mut().unwrap()[0] = None;
        assert!(
            review.copy_from_records(records.iter()).is_err(),
            "background-only cannot overwrite clipboard"
        );
    }

    #[test]
    fn review_copy_real_egui_action_is_prompt_independent_and_atomic() {
        for busy in [false, true] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let (mut terminal, mut review) = fixture();
            terminal.scroll_offset = 4;
            if busy {
                terminal.process_input(b"\x1b]133;C;jsh_id=prompt;cmdline_url=sleep\x07");
            } else {
                terminal.note_user_input(b"draft");
            }
            let view = review_view(
                &review,
                &terminal,
                false,
                &crate::terminal::ProjectionPolicy::new(),
            );
            assert!(!view.ready);
            assert!(view.copy_issue.is_none());
            let mut output = None;
            for _ in 0..2 {
                output = Some(test_frame(&ctx, raw_input(vec![]), |_ui| {
                    assert!(draw_review(&ctx, &mut review, &view).is_none());
                }));
            }
            let copy = button_node(output.as_ref().unwrap(), "Copy commands");
            let copied = test_frame(
                &ctx,
                raw_input(vec![action_event(copy, egui::accesskit::Action::Click)]),
                |_ui| {
                    assert!(draw_review(&ctx, &mut review, &view) == Some(ReviewAction::Copy));
                    copy_review_commands(&ctx, &review, &terminal).unwrap();
                },
            );
            assert!(copied.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(text) if text == "cargo check\ncargo test")));
            assert_eq!(terminal.scroll_offset, 4);
            terminal.clear_completed_blocks();
            let rejected = test_frame(&ctx, raw_input(vec![]), |_ui| {
                assert!(copy_review_commands(&ctx, &review, &terminal).is_err());
            });
            assert!(!rejected
                .platform_output
                .commands
                .iter()
                .any(|command| matches!(command, egui::OutputCommand::CopyText(_))));
        }
    }

    #[test]
    fn review_batch_buttons_dispatch_real_accessible_actions_without_enter_or_prompt_writes() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let (mut terminal, mut review) = fixture();
        terminal.note_user_input(b"keep my draft");
        let mut policy = crate::terminal::ProjectionPolicy::new();
        let view = review_view(&review, &terminal, false, &policy);
        let mut output = None;
        for _ in 0..2 {
            output = Some(test_frame(&ctx, raw_input(vec![]), |_ui| {
                assert!(draw_review(&ctx, &mut review, &view).is_none());
            }));
        }
        let collapse = button_node(output.as_ref().unwrap(), "Collapse selected output");
        let _ = test_frame(
            &ctx,
            raw_input(vec![action_event(collapse, egui::accesskit::Action::Focus)]),
            |_ui| {
                assert!(draw_review(&ctx, &mut review, &view).is_none());
            },
        );
        let _ = test_frame(
            &ctx,
            raw_input(vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]),
            |_ui| {
                assert!(draw_review(&ctx, &mut review, &view).is_none());
            },
        );
        let _ = test_frame(
            &ctx,
            raw_input(vec![action_event(collapse, egui::accesskit::Action::Click)]),
            |_ui| {
                assert!(draw_review(&ctx, &mut review, &view) == Some(ReviewAction::Collapse));
                assert_eq!(
                    set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, true)
                        .unwrap()
                        .changed,
                    2
                );
            },
        );
        let view = review_view(&review, &terminal, false, &policy);
        assert_eq!(view.fold.collapsed, 2);
        assert_eq!(view.fold.collapsible, 0);
        let output = test_frame(&ctx, raw_input(vec![]), |_ui| {
            assert!(draw_review(&ctx, &mut review, &view).is_none());
        });
        let expand = button_node(&output, "Expand selected output");
        let _ = test_frame(
            &ctx,
            raw_input(vec![action_event(expand, egui::accesskit::Action::Click)]),
            |_ui| {
                assert!(draw_review(&ctx, &mut review, &view) == Some(ReviewAction::Expand));
                assert_eq!(
                    set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, false)
                        .unwrap()
                        .changed,
                    2
                );
            },
        );
        assert!(!terminal.prompt_input_is_empty());
        assert!(!policy.collapsed_zone_ids().any(|_| true));
    }

    #[test]
    fn review_enter_never_inserts_even_with_insert_button_focused() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let (terminal, mut review) = fixture();
        let view = review_view(
            &review,
            &terminal,
            true,
            &crate::terminal::ProjectionPolicy::new(),
        );
        let mut output = None;
        for _ in 0..2 {
            output = Some(test_frame(&ctx, raw_input(vec![]), |_ui| {
                assert!(draw_review(&ctx, &mut review, &view).is_none());
            }));
        }
        let insert = button_node(output.as_ref().unwrap(), "Insert at prompt");
        let _ = test_frame(
            &ctx,
            raw_input(vec![action_event(insert, egui::accesskit::Action::Focus)]),
            |_ui| {
                assert!(draw_review(&ctx, &mut review, &view).is_none());
            },
        );
        let _ = test_frame(
            &ctx,
            raw_input(vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]),
            |_ui| {
                assert!(draw_review(&ctx, &mut review, &view).is_none());
            },
        );
        let _ = test_frame(
            &ctx,
            raw_input(vec![action_event(insert, egui::accesskit::Action::Click)]),
            |_ui| {
                assert!(draw_review(&ctx, &mut review, &view) == Some(ReviewAction::Insert));
            },
        );
    }

    #[test]
    fn block_review_offscreen_visual_and_escape_reopen() {
        let destination = std::env::var_os("EMBER_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        if let Some(path) = &destination {
            std::fs::create_dir_all(path).unwrap();
        }
        for (name, width, height, evicted) in [
            ("review-wide", 1000, 800, false),
            ("review-narrow", 360, 640, false),
            ("review-evicted", 800, 640, true),
            ("review-dirty", 360, 640, false),
            ("review-unsafe", 360, 640, false),
            ("review-unicode", 360, 640, false),
            ("review-copied", 360, 640, false),
            ("review-collapsed", 360, 640, false),
            ("review-comparison", 360, 640, false),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            crate::apply_theme_visuals(&ctx, &crate::theme::Theme::default());
            let (mut terminal, mut review) = fixture();
            let mut renderer = crate::ui::TerminalRenderer::new(
                14.0,
                8.0,
                1.35,
                crate::config::ScrollbarVisibility::Always,
                crate::theme::Theme::default(),
            );
            renderer.gpu_rendering = false;
            review.active = 1;
            if evicted {
                terminal.clear_completed_blocks();
            }
            if name == "review-dirty" {
                terminal.note_user_input(b"draft");
            }
            if name == "review-copied" {
                review.error = Some("Commands copied. Nothing inserted or run.".into());
            }
            if name == "review-comparison" {
                review.compare_output(&terminal).unwrap();
            }
            if matches!(name, "review-unsafe" | "review-unicode") {
                let command = if name == "review-unsafe" {
                    "echo%20%E2%80%AEspoofed".into()
                } else {
                    format!("printf%20'{}'", "编译🙂%20".repeat(40))
                };
                finish(&mut terminal, "display", &command, 0);
                review =
                    BlockReview::capture("pane".into(), &["display".into()], &terminal).unwrap();
            }
            let mut policy = crate::terminal::ProjectionPolicy::new();
            if name == "review-collapsed" {
                set_reviewed_output_collapsed(&review, &mut terminal, &mut policy, true).unwrap();
            }
            let view = review_view(&review, &terminal, true, &policy);
            let mut capture = super::super::visual_test_support::OffscreenCapture::default();
            for pass in 0..3 {
                let mut result = None;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        time: Some(pass as f64 * 0.25),
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width as f32, height as f32),
                        )),
                        events: if pass == 2 {
                            vec![egui::Event::Key {
                                key: egui::Key::Escape,
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
                        renderer.render(
                            ui,
                            &mut terminal,
                            false,
                            false,
                            &crate::search::SearchState::default(),
                            &[],
                            &None,
                        );
                        result = draw_review(&ctx, &mut review, &view);
                    },
                );
                if pass == 1 {
                    for (_, node) in &output
                        .platform_output
                        .accesskit_update
                        .as_ref()
                        .unwrap()
                        .nodes
                    {
                        if matches!(
                            node.label(),
                            Some(
                                "Copy commands"
                                    | "Insert at prompt"
                                    | "Close review"
                                    | "Collapse selected output"
                                    | "Expand selected output"
                                    | "Compare output"
                                    | "Copy comparison"
                                    | "Export selected Markdown"
                                    | "Export selected JSON"
                            )
                        ) {
                            let bounds = node.bounds().expect("footer action bounds");
                            assert!(
                                bounds.x0 >= 0.0
                                    && bounds.y0 >= 0.0
                                    && bounds.x1 <= width as f64
                                    && bounds.y1 <= height as f64,
                                "{name}: footer {bounds:?} outside {width}x{height}"
                            );
                        }
                    }
                    if name == "review-comparison" {
                        let nodes = &output
                            .platform_output
                            .accesskit_update
                            .as_ref()
                            .unwrap()
                            .nodes;
                        let footer = nodes
                            .iter()
                            .find(|(_, node)| node.label() == Some("Close review"))
                            .and_then(|(_, node)| node.bounds())
                            .expect("visible comparison footer");
                        let removed = nodes
                            .iter()
                            .find(|(_, node)| {
                                node.label() == Some("-Build finished successfully")
                                    || node.value() == Some("-Build finished successfully")
                            })
                            .and_then(|(_, node)| node.bounds())
                            .expect("first changed line is rendered");
                        assert!(removed.y0 >= 0.0 && removed.y1 <= footer.y0,
                            "comparison changes must be visible on opening, above the footer: {removed:?}, {footer:?}");
                    }
                }
                if pass == 2 {
                    assert!(result == Some(ReviewAction::Close));
                } else {
                    assert!(result.is_none());
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
            }
            let (_, reopened) = fixture();
            assert!(
                !reopened.armed,
                "a new review never inherits its predecessor's armed state"
            );
        }
    }
}
