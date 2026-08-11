//! Warp/anvil/forge-style command-block chrome: outcome classification,
//! duration/badge composition and visible-span math.
//!
//! Everything here is pure so the contract can be pinned by unit tests; the
//! egui painter calls stay thin in `ui.rs`. Blocks are derived from the
//! semantic [`CommandRecord`](crate::terminal::CommandRecord) timeline: block
//! *i* spans from its `prompt_start` line to the line before the next record's
//! `prompt_start`, so late output after `OSC 133;D` still belongs visually to
//! the block that produced it (finalize happens at the next `A`, as in
//! anvil/forge).

use crate::terminal::CommandState;
use jterm_core::block_contract::{classify_completed, CompletedBlockOutcome};

/// Presence marker used only when the semantic record knows a command existed
/// but its bounded text was deliberately omitted. It is never rendered or
/// persisted; it prevents `command_truncated` from masquerading as background
/// output when the shared classifier receives the resolved frontend facts.
const TRUNCATED_COMMAND_PRESENCE: &str = "[truncated command]";

/// Width of the per-block gutter stripe, in pixels.
pub const GUTTER_STRIPE_WIDTH: f32 = 3.0;
/// Wider stripe for the selected block.
pub const GUTTER_STRIPE_SELECTED_WIDTH: f32 = 4.0;
/// Layout-owned horizontal band immediately before `content_rect.left()` where
/// a click selects a block instead of moving the cursor or clearing the text
/// selection.
pub const GUTTER_CLICK_BAND_PX: f32 = 8.0;
/// Minimum visual height of the newest editable/running command card. This is
/// paint metadata only: the terminal grid and PTY keep their viewport size.
pub const MIN_INPUT_ROWS: usize = 6;

/// What a block's gutter/badge should communicate. `Unknown` (the shell never
/// reported an exit code) must never render as success: `exit_code == None`
/// is not `Some(0)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockOutcome {
    /// The live prompt block (OSC 133 A/B without C yet): accent card and
    /// stripe, but no badge so input remains unobscured.
    Prompt,
    /// The newest record is between C and D: accent-colored stripe and live
    /// elapsed-time badge when it fits.
    Running,
    /// Completed cycle with no/empty command (e.g. Enter on an empty prompt).
    Background,
    Success,
    Failed(i32),
    /// Completed without a reported exit code, or a stale `Running` record
    /// whose D was lost.
    Unknown,
}

/// Classify one record. `is_newest` distinguishes the genuinely running
/// command from an older record still marked `Running` (its D was lost);
/// only the newest incomplete record can be running, mirroring
/// `TerminalState::running_command`. `command` is the canonical semantic
/// record after OSC metadata and screen fallback, never a raw metadata field;
/// `command_truncated` preserves the fact that an omitted command did exist.
pub fn classify_outcome(
    command: Option<&str>,
    command_truncated: bool,
    exit_code: Option<i32>,
    state: CommandState,
    complete: bool,
    is_newest: bool,
) -> BlockOutcome {
    match state {
        CommandState::Prompt | CommandState::Editing => BlockOutcome::Prompt,
        CommandState::Running if is_newest && !complete => BlockOutcome::Running,
        CommandState::Running => BlockOutcome::Unknown,
        CommandState::Complete => {
            let resolved_command =
                command.or_else(|| command_truncated.then_some(TRUNCATED_COMMAND_PRESENCE));
            match classify_completed(resolved_command, exit_code) {
                CompletedBlockOutcome::Background => BlockOutcome::Background,
                CompletedBlockOutcome::Success => BlockOutcome::Success,
                CompletedBlockOutcome::Failed(code) => BlockOutcome::Failed(code),
                CompletedBlockOutcome::Unknown => BlockOutcome::Unknown,
            }
        }
    }
}

/// forge's `format_block_duration` contract, ported verbatim: `<1s` →
/// `"743ms"`, `<60s` → `"12.3s"`, `<1h` → `"1m32s"` (seconds retained; a
/// zero remainder collapses to `"1m"`), else `"2h05m"` (or `"1h"`).
pub fn format_block_duration(dur_ms: u64) -> String {
    if dur_ms < 1000 {
        format!("{dur_ms}ms")
    } else if dur_ms < 60_000 {
        format!("{:.1}s", dur_ms as f64 / 1000.0)
    } else if dur_ms < 3_600_000 {
        let m = dur_ms / 60_000;
        let s = (dur_ms % 60_000) / 1000;
        if s == 0 {
            format!("{m}m")
        } else {
            format!("{m}m{s:02}s")
        }
    } else {
        let h = dur_ms / 3_600_000;
        let m = (dur_ms % 3_600_000) / 60_000;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h{m:02}m")
        }
    }
}

/// Compact live badge for an OSC 133 command whose `C` arrived but whose `D`
/// has not. The arrow is deliberately distinct from the final success/failure
/// glyphs so an in-flight command can never look completed.
pub fn running_badge_text(elapsed_ms: u64) -> String {
    format!("▶ {}", format_block_duration(elapsed_ms))
}

/// Family refresh cadence for a visible running badge: keep feedback moving
/// once per second, then slow to once per minute when the formatter drops its
/// seconds component at one hour.
pub fn running_badge_refresh_interval(elapsed_ms: u64) -> std::time::Duration {
    if elapsed_ms < 3_600_000 {
        std::time::Duration::from_secs(1)
    } else {
        std::time::Duration::from_secs(60)
    }
}

/// Badge text for the block's first row, or `None` for the editable prompt
/// (running needs a live elapsed duration). Background output and an
/// unreported command status stay explicit and use the same compact wording as
/// the other native frontends.
pub fn badge_text(outcome: BlockOutcome, duration_ms: Option<u64>) -> Option<String> {
    match outcome {
        BlockOutcome::Success => Some(match duration_ms {
            Some(ms) => format!("✓ {}", format_block_duration(ms)),
            None => "✓".to_string(),
        }),
        BlockOutcome::Failed(code) => {
            let mut text = format!("✗ exit:{code}");
            if let Some(signal) = jterm_core::exit_status::signal_name_for_exit(code) {
                text.push(' ');
                text.push_str(signal);
            }
            if let Some(ms) = duration_ms {
                text.push_str(&format!(" · {}", format_block_duration(ms)));
            }
            Some(text)
        }
        BlockOutcome::Unknown => Some("? exit:?".to_string()),
        BlockOutcome::Running => duration_ms.map(running_badge_text),
        BlockOutcome::Background => Some("↻ Background".to_string()),
        BlockOutcome::Prompt => None,
    }
}

/// The row a prompt anchor actually renders on. `current_buffer_anchor`
/// records a pending-wrap position as `column == cols` ("end of this row"),
/// which means the next glyph — the prompt itself — lands on the following
/// row. Span computation must use that row, or the previous block's last
/// output row gets claimed by the next block. This normalization lives here
/// rather than in `current_buffer_anchor` because text extraction relies on
/// the end-of-row semantic.
pub fn prompt_row_line_id(anchor_line_id: u64, anchor_column: usize, cols: usize) -> u64 {
    if cols > 0 && anchor_column >= cols {
        anchor_line_id.saturating_add(1)
    } else {
        anchor_line_id
    }
}

/// A block's intersection with the current viewport, in viewport rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibleBlockSpan {
    /// Index into the record list the prompt line ids were taken from.
    pub record_index: usize,
    /// First visible viewport row of the block (inclusive, clamped).
    pub first_row: usize,
    /// Last visible viewport row of the block (inclusive, clamped).
    pub last_row: usize,
    /// Whether the block's own first row (the prompt row) is inside the
    /// viewport. Separator and badge are only drawn when it is.
    pub starts_in_viewport: bool,
    /// Whether the block's target end is in the viewport. For finished blocks
    /// this is the row before the next prompt; for the newest live block it is
    /// the bounded visual growth edge. Renderers use this to avoid a fake
    /// rounded/bordered bottom edge when a block is clipped by the window.
    pub ends_in_viewport: bool,
}

/// Exclusive visual end of the newest editable/running card. Idle input keeps
/// a six-row surface; multiline input or command output grows it through the
/// cursor row. This does not resize the terminal grid or PTY.
pub fn live_block_end_exclusive(prompt_start: u64, cursor_line_id: u64) -> u64 {
    prompt_start
        .saturating_add(MIN_INPUT_ROWS as u64)
        .max(cursor_line_id.saturating_add(1))
}

/// Intersect the newest live card's bounded visual extent with a viewport.
/// `ends_in_viewport` describes the real target end, not merely the viewport
/// clip, so scrolling cannot manufacture a bottom border.
pub fn visible_live_block_span(
    record_index: usize,
    prompt_start: u64,
    cursor_line_id: u64,
    top_line_id: u64,
    viewport_rows: usize,
) -> Option<VisibleBlockSpan> {
    visible_block_span(
        record_index,
        prompt_start,
        Some(live_block_end_exclusive(prompt_start, cursor_line_id)),
        top_line_id,
        viewport_rows,
    )
}

/// Intersect one block boundary pair with a viewport. Kept separate from the
/// collection walk so frontends can binary-search their native record store
/// and remain `O(visible blocks)` without first allocating every prompt id.
pub fn visible_block_span(
    record_index: usize,
    start: u64,
    next_start: Option<u64>,
    top_line_id: u64,
    viewport_rows: usize,
) -> Option<VisibleBlockSpan> {
    if viewport_rows == 0 {
        return None;
    }
    let bottom_line_id = top_line_id.saturating_add(viewport_rows as u64 - 1);
    let (end, has_semantic_end) = match next_start {
        Some(next_start) => (next_start.checked_sub(1).filter(|end| *end >= start)?, true),
        None => (bottom_line_id.max(start), false),
    };
    if end < top_line_id || start > bottom_line_id {
        return None;
    }
    Some(VisibleBlockSpan {
        record_index,
        first_row: (start.max(top_line_id) - top_line_id) as usize,
        last_row: (end.min(bottom_line_id) - top_line_id) as usize,
        starts_in_viewport: start >= top_line_id,
        ends_in_viewport: has_semantic_end && end <= bottom_line_id,
    })
}

/// Compute the viewport intersection of every block. `prompt_line_ids` are
/// the records' `prompt_start.line_id` values in record order; block *i* ends
/// on the line before block *i+1* starts, and the last block extends to the
/// viewport bottom (the live tail).
#[cfg(test)]
pub fn visible_block_spans(
    prompt_line_ids: &[u64],
    top_line_id: u64,
    viewport_rows: usize,
) -> Vec<VisibleBlockSpan> {
    let mut spans = Vec::new();
    if viewport_rows == 0 {
        return spans;
    }
    let bottom_line_id = top_line_id.saturating_add(viewport_rows as u64 - 1);
    // At most the last block that starts at/before the viewport can intersect
    // its top. Skip older retained history in logarithmic time, then stop once
    // prompt starts pass the bottom.
    let first_after_top = prompt_line_ids.partition_point(|start| *start <= top_line_id);
    let first_candidate = first_after_top.saturating_sub(1);
    for record_index in first_candidate..prompt_line_ids.len() {
        let start = prompt_line_ids[record_index];
        if start > bottom_line_id {
            break;
        }
        if let Some(span) = visible_block_span(
            record_index,
            start,
            prompt_line_ids.get(record_index + 1).copied(),
            top_line_id,
            viewport_rows,
        ) {
            spans.push(span);
        }
    }
    spans
}

/// Whether a right-aligned badge starting at `start_col` would cover only
/// blank cells. Callers must map wide-character continuation cells to a
/// non-blank character so a badge never paints over half a glyph.
pub fn badge_covers_only_blank_cells(row_chars: &[char], start_col: usize) -> bool {
    row_chars
        .iter()
        .skip(start_col)
        .all(|ch| matches!(ch, ' ' | '\0'))
}

/// Direction for `block:select_prev` / `block:select_next`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectStep {
    Older,
    Newer,
}

/// One pane-local Warp-style block selection. `selected_ids` is always kept
/// in terminal order; `anchor_id` stays fixed while Shift+Up/Down moves the
/// `active_id` edge and rebuilds the inclusive range between them.
///
/// Keeping the session identity in the value makes a renderer reused by a
/// different tab fail closed instead of painting stale ids from the previous
/// pane. The app mirrors only `active_id` into the single-row Commands sidebar
/// selection; every block in `selected_ids` is still outlined in the terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSelection {
    pub session_id: String,
    pub selected_ids: Vec<String>,
    pub active_id: String,
    pub anchor_id: String,
}

impl BlockSelection {
    /// Replace any previous range with one active block.
    pub fn single(session_id: String, record_id: String) -> Self {
        Self {
            session_id,
            selected_ids: vec![record_id.clone()],
            active_id: record_id.clone(),
            anchor_id: record_id,
        }
    }

    /// Select every supplied id. The ids must be in terminal order; the oldest
    /// becomes the fixed anchor and the newest the active edge, matching
    /// anvil/forge's Select-all contract. Empty input has no selection.
    pub fn all(session_id: String, selected_ids: Vec<String>) -> Option<Self> {
        let anchor_id = selected_ids.first()?.clone();
        let active_id = selected_ids.last()?.clone();
        Some(Self {
            session_id,
            selected_ids,
            active_id,
            anchor_id,
        })
    }

    /// Move the active edge to `target` and select the inclusive range from
    /// the fixed anchor. A stale anchor degrades to a single target rather than
    /// manufacturing a discontinuous or cross-session selection.
    pub fn extend_to(&mut self, ordered_ids: &[String], target: &str) {
        self.selected_ids = selected_id_range(ordered_ids, &self.anchor_id, target);
        self.active_id = target.to_string();
        if !ordered_ids.iter().any(|id| id == &self.anchor_id) {
            self.anchor_id = target.to_string();
        }
    }
}

/// Inclusive terminal-order range used by Shift+Up/Down. Missing endpoints
/// fail closed to the target only; callers can therefore use a snapshot of the
/// current record list even while old records are being evicted.
pub fn selected_id_range(ordered_ids: &[String], anchor: &str, target: &str) -> Vec<String> {
    let Some(anchor_index) = ordered_ids.iter().position(|id| id == anchor) else {
        return vec![target.to_string()];
    };
    let Some(target_index) = ordered_ids.iter().position(|id| id == target) else {
        return vec![target.to_string()];
    };
    let (start, end) = if anchor_index <= target_index {
        (anchor_index, target_index)
    } else {
        (target_index, anchor_index)
    };
    ordered_ids[start..=end].to_vec()
}

/// Keyboard block navigation over the same selectable set as gutter clicks
/// (`outcome != Prompt`; `Running` included). `current` is the resolved index
/// of the currently selected record, or `None` when nothing is selected or
/// the selected id no longer resolves (evicted → treated as no selection, in
/// which case both directions pick the NEWEST selectable block). Returns the
/// index to select, or `None` when the selection must not change: clamped at
/// either end (silent no-op), or no selectable block exists.
pub fn next_selected_index(
    outcomes: &[BlockOutcome],
    current: Option<usize>,
    step: SelectStep,
) -> Option<usize> {
    let selectable: Vec<usize> = outcomes
        .iter()
        .enumerate()
        .filter(|(_, outcome)| **outcome != BlockOutcome::Prompt)
        .map(|(index, _)| index)
        .collect();
    let position =
        current.and_then(|current| selectable.iter().position(|&index| index == current));
    match position {
        None => selectable.last().copied(),
        Some(position) => match step {
            SelectStep::Older => position.checked_sub(1).map(|older| selectable[older]),
            SelectStep::Newer => selectable.get(position + 1).copied(),
        },
    }
}

/// Keyboard failed-block navigation for `block:jump_prev_failed` /
/// `block:jump_next_failed`: step only over FAILED blocks (the same
/// classification the scrollbar markers use). `current` is the resolved index
/// of the currently selected record — any block, failed or not — or `None`
/// when nothing is selected (dangling/cross-session selections resolve to
/// `None`), in which case both directions pick the NEWEST failed block.
/// Returns the index to select, or `None` when nothing qualifies: no failed
/// block strictly older/newer than `current` (silent no-op), or no failed
/// block at all (the caller toasts).
pub fn next_failed_index(
    outcomes: &[BlockOutcome],
    current: Option<usize>,
    step: SelectStep,
) -> Option<usize> {
    let mut failed = outcomes
        .iter()
        .enumerate()
        .filter(|(_, outcome)| matches!(outcome, BlockOutcome::Failed(_)))
        .map(|(index, _)| index);
    match current {
        None => failed.next_back(),
        Some(current) => match step {
            SelectStep::Older => failed.take_while(|&index| index < current).last(),
            SelectStep::Newer => failed.find(|&index| index > current),
        },
    }
}

/// Hard cap on cross-block search hits. Scanning stops at the cap, and the
/// caller feeds records NEWEST FIRST so the cap keeps recent history.
pub const MAX_BLOCK_SEARCH_HITS: usize = 500;
/// Display clip for a hit's matching line.
pub const BLOCK_SEARCH_LINE_TEXT_CHARS: usize = 200;
/// Display clip for a hit's command preview.
pub const BLOCK_SEARCH_COMMAND_PREVIEW_CHARS: usize = 80;

/// One `block:search` result row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSearchHit {
    pub record_id: String,
    /// False for a hit on the record's command text.
    pub is_output_line: bool,
    /// 1-based line number within the record's output; `None` for command
    /// hits. Numbering restarts at 1 for every record.
    pub line_no: Option<usize>,
    /// The matching line, single-line clipped to
    /// [`BLOCK_SEARCH_LINE_TEXT_CHARS`].
    pub line_text: String,
    /// The record's command, single-line clipped to
    /// [`BLOCK_SEARCH_COMMAND_PREVIEW_CHARS`]; `"(no command)"` for
    /// background blocks.
    pub command_preview: String,
}

/// One record's precomputed text for [`search_blocks`]. Built once per
/// picker-open (and rebuilt on a session switch while open), so every
/// keystroke scans these strings instead of re-extracting terminal output.
/// The lowercase copies are precomputed here so a query run allocates
/// nothing beyond its needle and its hits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CachedBlockSearchRecord {
    pub record_id: String,
    /// The record's command, original case; `None` for background blocks
    /// (absent or blank command).
    pub command: Option<String>,
    /// Lowercased `command`, for case-insensitive matching.
    pub command_lowercase: Option<String>,
    /// The record's output, original case; `None` when it has none.
    pub output: Option<String>,
    /// Lowercased `output`. Unicode lowercasing never adds or removes line
    /// breaks, so `output.lines()` and `output_lowercase.lines()` stay in
    /// step and hits can report original-case line text.
    pub output_lowercase: Option<String>,
}

impl CachedBlockSearchRecord {
    /// Normalize one record at cache-build time: a blank command counts as
    /// none (background block), and the lowercase copies are precomputed.
    pub fn new(record_id: &str, command: Option<&str>, output: Option<String>) -> Self {
        let command = command
            .filter(|command| !command.trim().is_empty())
            .map(str::to_string);
        let command_lowercase = command.as_deref().map(str::to_lowercase);
        let output_lowercase = output.as_deref().map(str::to_lowercase);
        Self {
            record_id: record_id.to_string(),
            command,
            command_lowercase,
            output,
            output_lowercase,
        }
    }
}

/// [`search_blocks`] result: hits in scan order (records newest first;
/// within a record, the command hit before its output hits).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockSearchResults {
    pub hits: Vec<BlockSearchHit>,
    /// True when scanning stopped at [`MAX_BLOCK_SEARCH_HITS`].
    pub capped: bool,
}

/// Collapse `text` to a single line (newlines/CR/tabs become spaces) and clip
/// it to `max_chars` characters, appending `…` when cut.
pub fn single_line_clip(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (count, character) in text.chars().enumerate() {
        if count >= max_chars {
            out.push('…');
            return out;
        }
        out.push(match character {
            '\n' | '\r' | '\t' => ' ',
            character => character,
        });
    }
    out
}

/// Case-insensitive substring search (no regex) across the cached command
/// text and output lines of every offered record. `query` is trimmed; an
/// empty query matches nothing. Scanning stops as soon as the hit cap is
/// reached, and `capped` reports that stop — i.e. that older content went
/// unscanned — regardless of whether it would have matched. Beyond the
/// lowercased needle, allocations happen only for hits.
pub fn search_blocks<'a, I>(records: I, query: &str) -> BlockSearchResults
where
    I: IntoIterator<Item = &'a CachedBlockSearchRecord>,
{
    let needle = query.trim().to_lowercase();
    let mut results = BlockSearchResults::default();
    if needle.is_empty() {
        return results;
    }
    for record in records {
        if results.hits.len() >= MAX_BLOCK_SEARCH_HITS {
            results.capped = true;
            return results;
        }
        // Shared by the record's hits; computed only when one exists.
        let mut command_preview: Option<String> = None;
        let mut preview = |command: Option<&str>| -> String {
            command_preview
                .get_or_insert_with(|| {
                    command
                        .map(|command| {
                            single_line_clip(command, BLOCK_SEARCH_COMMAND_PREVIEW_CHARS)
                        })
                        .unwrap_or_else(|| "(no command)".to_string())
                })
                .clone()
        };
        if let (Some(command), Some(lowercase)) = (
            record.command.as_deref(),
            record.command_lowercase.as_deref(),
        ) {
            if lowercase.contains(&needle) {
                let command_preview = preview(Some(command));
                results.hits.push(BlockSearchHit {
                    record_id: record.record_id.clone(),
                    is_output_line: false,
                    line_no: None,
                    line_text: single_line_clip(command, BLOCK_SEARCH_LINE_TEXT_CHARS),
                    command_preview,
                });
            }
        }
        let (Some(output), Some(output_lowercase)) =
            (record.output.as_deref(), record.output_lowercase.as_deref())
        else {
            continue;
        };
        for (line_index, (line, line_lowercase)) in
            output.lines().zip(output_lowercase.lines()).enumerate()
        {
            if line_lowercase.contains(&needle) {
                if results.hits.len() >= MAX_BLOCK_SEARCH_HITS {
                    results.capped = true;
                    return results;
                }
                let command_preview = preview(record.command.as_deref());
                results.hits.push(BlockSearchHit {
                    record_id: record.record_id.clone(),
                    is_output_line: true,
                    line_no: Some(line_index + 1),
                    line_text: single_line_clip(line.trim_end(), BLOCK_SEARCH_LINE_TEXT_CHARS),
                    command_preview,
                });
            }
        }
    }
    results
}

/// anvil's `markdown_fence` rule: the fence must be strictly longer than any
/// backtick run inside the fenced body, and never shorter than the CommonMark
/// minimum of three.
pub fn markdown_fence(body: &str) -> String {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in body.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// One fenced code section: per-body fence, body normalized to exactly one
/// trailing newline before the closing fence (no doubling).
fn fenced(body: &str) -> String {
    let fence = markdown_fence(body);
    let body = body.strip_suffix('\n').unwrap_or(body);
    if body.is_empty() {
        format!("{fence}\n{fence}")
    } else {
        format!("{fence}\n{body}\n{fence}")
    }
}

fn is_c0_or_c1(character: char) -> bool {
    matches!(character as u32, 0x00..=0x1f | 0x7f..=0x9f)
}

/// One meta-line value (`- Cwd: …`). The value is PTY-controlled (OSC 133
/// metadata is percent-decoded without filtering), so an embedded newline
/// could forge extra meta lines and an ESC could smuggle terminal control
/// sequences onto the clipboard. Tabs become spaces; every other C0/C1
/// control (newlines, CR, ESC, the whole C1 range) is stripped, leaving a
/// single-line, control-free value.
pub fn sanitize_meta_line_value(value: &str) -> String {
    value
        .chars()
        .filter_map(|character| match character {
            '\t' => Some(' '),
            character if is_c0_or_c1(character) => None,
            character => Some(character),
        })
        .collect()
}

/// A fenced code body (command or output): newlines keep their structure,
/// every other C0/C1 control (ESC and the C1 range included) is stripped so
/// no terminal control sequence survives onto the clipboard.
pub fn sanitize_fenced_body(body: &str) -> String {
    body.chars()
        .filter(|character| *character == '\n' || !is_c0_or_c1(*character))
        .collect()
}

/// Input for [`block_markdown`]. `command: None` marks a background block
/// (no command line): the Command section and the Exit line are omitted,
/// following the anvil/forge `block_clipboard_text` family rule.
pub struct MarkdownBlock<'a> {
    pub command: Option<&'a str>,
    /// True when `command` is the byte-exact, untruncated OSC 133 text. A
    /// screen-reconstructed command still exports, flagged by a Note line.
    pub command_exact: bool,
    pub output: &'a str,
    /// True when `output` was cut at the capture limit (Note line).
    pub output_truncated: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    /// Preformatted finish timestamp (see [`format_local_datetime`]).
    pub finished: Option<&'a str>,
    pub cwd: Option<&'a str>,
}

/// `block:copy_markdown` document. frost ships the byte-identical format, so
/// the exact shape is pinned by unit tests here. All embedded values are
/// sanitized here — not at the call site — so the tests pin the security
/// contract too.
pub fn block_markdown(block: &MarkdownBlock<'_>) -> String {
    let command = block
        .command
        .map(sanitize_fenced_body)
        .filter(|command| !command.trim().is_empty());
    let output = sanitize_fenced_body(block.output);
    let mut doc = String::from("## Command Block\n");
    let mut meta = String::new();
    if command.is_some() {
        match block.exit_code {
            Some(code) => {
                meta.push_str(&format!("- Exit: {code}"));
                if let Some(signal) = jterm_core::exit_status::signal_name_for_exit(code) {
                    meta.push(' ');
                    meta.push_str(signal);
                }
                meta.push('\n');
            }
            None => meta.push_str("- Exit: not reported\n"),
        }
    }
    if let Some(ms) = block.duration_ms {
        meta.push_str(&format!("- Duration: {}\n", format_block_duration(ms)));
    }
    if let Some(finished) = block.finished {
        meta.push_str(&format!(
            "- Finished: {}\n",
            sanitize_meta_line_value(finished)
        ));
    }
    if let Some(cwd) = block.cwd {
        meta.push_str(&format!("- Cwd: {}\n", sanitize_meta_line_value(cwd)));
    }
    if command.is_some() && !block.command_exact {
        meta.push_str("- Note: command reconstructed from screen\n");
    }
    if block.output_truncated {
        meta.push_str("- Note: output truncated\n");
    }
    if !meta.is_empty() {
        doc.push('\n');
        doc.push_str(&meta);
    }
    if let Some(command) = &command {
        doc.push_str("\nCommand:\n\n");
        doc.push_str(&fenced(command));
        doc.push('\n');
    }
    doc.push_str("\nOutput:\n\n");
    doc.push_str(&fenced(&output));
    doc.push('\n');
    doc
}

/// `SystemTime` → whole seconds since the Unix epoch (`None` for pre-epoch).
pub fn epoch_secs(time: std::time::SystemTime) -> Option<u64> {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// Days since the epoch → (year, month, day); Howard Hinnant's
/// `civil_from_days`, exact for the whole proleptic Gregorian range.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Civil date/time parts at a fixed UTC offset (seconds east). Signed math
/// throughout so an offset can roll the civil day across the epoch.
fn local_parts(epoch_secs: u64, offset_secs: i32) -> (i64, u32, u32, u64, u64, u64) {
    let local_secs = epoch_secs as i64 + i64::from(offset_secs);
    let (year, month, day) = civil_from_days(local_secs.div_euclid(86_400));
    let second_of_day = local_secs.rem_euclid(86_400) as u64;
    (
        year,
        month,
        day,
        second_of_day / 3_600,
        (second_of_day % 3_600) / 60,
        second_of_day % 60,
    )
}

/// `±HH:MM` suffix for a UTC offset in seconds east (`+08:00`, `-05:30`,
/// `+00:00`).
pub fn format_utc_offset(offset_secs: i32) -> String {
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let magnitude = offset_secs.unsigned_abs();
    format!(
        "{sign}{:02}:{:02}",
        magnitude / 3_600,
        (magnitude % 3_600) / 60
    )
}

/// `YYYY-MM-DD HH:MM:SS ±HH:MM` at a fixed UTC offset. Pure so tests can pin
/// any offset without touching the process timezone; the runtime offset comes
/// from [`local_utc_offset_secs`].
pub fn format_local_datetime(epoch_secs: u64, offset_secs: i32) -> String {
    let (year, month, day, hour, minute, second) = local_parts(epoch_secs, offset_secs);
    format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} {}",
        format_utc_offset(offset_secs)
    )
}

/// Time-of-day only (`HH:MM:SS`) at a fixed UTC offset, for the
/// selected-block badge suffix.
pub fn format_local_time_of_day(epoch_secs: u64, offset_secs: i32) -> String {
    let (_, _, _, hour, minute, second) = local_parts(epoch_secs, offset_secs);
    format!("{hour:02}:{minute:02}:{second:02}")
}

/// Local-timezone UTC offset (seconds east) in effect at `epoch_secs`, via
/// libc `localtime_r` — the one deliberately impure function in this module
/// (the formatters above take the offset as a parameter so tests never read
/// the process timezone). Falls back to 0 (UTC) when conversion fails.
pub fn local_utc_offset_secs(epoch_secs: u64) -> i32 {
    let time = epoch_secs.min(i64::MAX as u64) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let converted = unsafe { libc::localtime_r(&time, &mut tm) };
    if converted.is_null() {
        0
    } else {
        tm.tm_gmtoff as i32
    }
}

/// Fractional position of a block's first row within the retained buffer,
/// for the scrollbar failure markers: 0.0 = oldest retained line (track
/// top), 1.0 = newest grid line (track bottom). `None` once the row has been
/// evicted from scrollback.
pub fn scrollbar_marker_fraction(
    line_id: u64,
    oldest_line_id: u64,
    newest_line_id: u64,
) -> Option<f32> {
    if line_id < oldest_line_id || line_id > newest_line_id {
        return None;
    }
    let span = newest_line_id - oldest_line_id;
    if span == 0 {
        return Some(0.0);
    }
    Some((line_id - oldest_line_id) as f32 / span as f32)
}

/// Index of the OLDEST FAILED block — the same [`classify_outcome`]-based
/// definition the scrollbar markers and `next_failed_index` use (complete,
/// non-empty command, nonzero exit), so every failed-block feature agrees.
/// Unreported exit codes are `Unknown`, and an empty-command completion with
/// a nonzero exit is `Background`; neither is a failure.
pub fn oldest_failed_index(outcomes: &[BlockOutcome]) -> Option<usize> {
    outcomes
        .iter()
        .position(|outcome| matches!(outcome, BlockOutcome::Failed(_)))
}

/// Result of a click routed through block-mode hit testing. `Select` carries
/// the record id of the block whose gutter band was clicked; `Clear` is any
/// other plain content click (block selection follows real interaction, like
/// anvil's precedent that real input clears it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockClick {
    Select(String),
    Clear,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_none_is_unknown_never_success_and_empty_command_is_background() {
        let complete = |command: Option<&str>, command_truncated: bool, exit: Option<i32>| {
            classify_outcome(
                command,
                command_truncated,
                exit,
                CommandState::Complete,
                true,
                true,
            )
        };
        assert_eq!(
            complete(Some("cargo test"), false, Some(0)),
            BlockOutcome::Success
        );
        assert_eq!(
            complete(Some("cargo test"), false, Some(101)),
            BlockOutcome::Failed(101)
        );
        // `None` must never be treated as 0.
        assert_eq!(
            complete(Some("cargo test"), false, None),
            BlockOutcome::Unknown
        );
        assert_ne!(
            complete(Some("cargo test"), false, None),
            BlockOutcome::Success
        );
        // No/empty command → background, even with a reported exit code.
        assert_eq!(complete(None, false, Some(7)), BlockOutcome::Background);
        assert_eq!(
            complete(Some("  \t"), false, Some(1)),
            BlockOutcome::Background
        );
        // A bounded semantic record can know a command existed even after its
        // text was omitted; that is not background output.
        assert_eq!(complete(None, true, Some(7)), BlockOutcome::Failed(7));
    }

    #[test]
    fn only_the_newest_incomplete_record_counts_as_running() {
        assert_eq!(
            classify_outcome(
                Some("sleep 9"),
                false,
                None,
                CommandState::Running,
                false,
                true,
            ),
            BlockOutcome::Running
        );
        // 老的 Running 记录意味着它的 D 丢了,按 Unknown 处理。
        assert_eq!(
            classify_outcome(
                Some("sleep 9"),
                false,
                None,
                CommandState::Running,
                false,
                false,
            ),
            BlockOutcome::Unknown
        );
        for state in [CommandState::Prompt, CommandState::Editing] {
            assert_eq!(
                classify_outcome(None, false, None, state, false, true),
                BlockOutcome::Prompt
            );
        }
    }

    #[test]
    fn duration_format_matches_the_forge_contract() {
        assert_eq!(format_block_duration(743), "743ms");
        assert_eq!(format_block_duration(999), "999ms");
        assert_eq!(format_block_duration(1_000), "1.0s");
        assert_eq!(format_block_duration(12_300), "12.3s");
        // Seconds are retained below one hour; zero remainders collapse.
        assert_eq!(format_block_duration(92_000), "1m32s");
        assert_eq!(format_block_duration(3_599_000), "59m59s");
        assert_eq!(format_block_duration(7_500_000), "2h05m");
        // forge's own pins (block_view/blocks.rs), byte-identical.
        assert_eq!(format_block_duration(250), "250ms");
        assert_eq!(format_block_duration(2500), "2.5s");
        assert_eq!(format_block_duration(59_940), "59.9s");
        assert_eq!(format_block_duration(60_000), "1m");
        assert_eq!(format_block_duration(61_000), "1m01s");
        assert_eq!(format_block_duration(179_000), "2m59s");
        assert_eq!(format_block_duration(3_600_000), "1h");
        assert_eq!(format_block_duration(3_840_000), "1h04m");
    }

    #[test]
    fn badge_text_states_and_signal_names() {
        assert_eq!(
            badge_text(BlockOutcome::Success, Some(1_200)),
            Some("✓ 1.2s".to_string())
        );
        assert_eq!(
            badge_text(BlockOutcome::Success, None),
            Some("✓".to_string())
        );
        assert_eq!(
            badge_text(BlockOutcome::Failed(2), Some(2_300)),
            Some("✗ exit:2 · 2.3s".to_string())
        );
        // 128+n carries the signal name, same source as the bottom bar.
        assert_eq!(
            badge_text(BlockOutcome::Failed(130), Some(2_300)),
            Some("✗ exit:130 SIGINT · 2.3s".to_string())
        );
        // Unreported exit and commandless background output remain explicit.
        assert_eq!(
            badge_text(BlockOutcome::Unknown, Some(2_300)),
            Some("? exit:?".to_string())
        );
        assert_eq!(
            badge_text(BlockOutcome::Running, Some(1_250)),
            Some("▶ 1.2s".to_string())
        );
        assert_eq!(badge_text(BlockOutcome::Running, None), None);
        assert_eq!(badge_text(BlockOutcome::Prompt, None), None);
        assert_eq!(
            badge_text(BlockOutcome::Background, Some(10)),
            Some("↻ Background".to_string())
        );
    }

    #[test]
    fn running_badge_is_compact_and_uses_bounded_refresh_cadence() {
        assert_eq!(running_badge_text(0), "▶ 0ms");
        assert_eq!(running_badge_text(92_000), "▶ 1m32s");
        assert!(!running_badge_text(1_250).contains('✓'));
        assert!(!running_badge_text(1_250).contains('✗'));
        assert_eq!(
            running_badge_refresh_interval(3_599_999),
            std::time::Duration::from_secs(1)
        );
        assert_eq!(
            running_badge_refresh_interval(3_600_000),
            std::time::Duration::from_secs(60)
        );
    }

    #[test]
    fn spans_end_at_the_next_prompt_and_the_last_block_reaches_the_bottom() {
        // Viewport rows 0..=9 cover line ids 100..=109.
        let spans = visible_block_spans(&[100, 103, 107], 100, 10);
        assert_eq!(
            spans,
            vec![
                VisibleBlockSpan {
                    record_index: 0,
                    first_row: 0,
                    last_row: 2,
                    starts_in_viewport: true,
                    ends_in_viewport: true,
                },
                VisibleBlockSpan {
                    record_index: 1,
                    first_row: 3,
                    last_row: 6,
                    starts_in_viewport: true,
                    ends_in_viewport: true,
                },
                // 最后一个块一直延伸到 viewport 底部(live tail)。
                VisibleBlockSpan {
                    record_index: 2,
                    first_row: 7,
                    last_row: 9,
                    starts_in_viewport: true,
                    ends_in_viewport: false,
                },
            ]
        );
    }

    #[test]
    fn spans_clip_to_the_viewport_and_skip_offscreen_blocks() {
        // Block 0 starts above the viewport: clipped, prompt row not visible.
        let spans = visible_block_spans(&[95, 105, 120], 100, 10);
        assert_eq!(
            spans,
            vec![
                VisibleBlockSpan {
                    record_index: 0,
                    first_row: 0,
                    last_row: 4,
                    starts_in_viewport: false,
                    ends_in_viewport: true,
                },
                VisibleBlockSpan {
                    record_index: 1,
                    first_row: 5,
                    last_row: 9,
                    starts_in_viewport: true,
                    ends_in_viewport: false,
                },
                // record 2 (line 120) 完全在 viewport 之下:跳过。
            ]
        );
        // Block 0 (10..=19) is fully above the viewport; the last block still
        // reaches down into it because it extends to the live bottom.
        assert_eq!(
            visible_block_spans(&[10, 20], 100, 10),
            vec![VisibleBlockSpan {
                record_index: 1,
                first_row: 0,
                last_row: 9,
                starts_in_viewport: false,
                ends_in_viewport: false,
            }]
        );
        assert!(visible_block_spans(&[10], 100, 0).is_empty());
    }

    #[test]
    fn live_span_keeps_six_rows_grows_to_cursor_and_preserves_clip_state() {
        assert_eq!(live_block_end_exclusive(100, 100), 106);
        assert_eq!(live_block_end_exclusive(100, 108), 109);

        // Idle input owns exactly the six-row visual floor when it fits.
        assert_eq!(
            visible_live_block_span(3, 100, 100, 100, 10),
            Some(VisibleBlockSpan {
                record_index: 3,
                first_row: 0,
                last_row: 5,
                starts_in_viewport: true,
                ends_in_viewport: true,
            })
        );
        // Running output grows through the cursor row.
        assert_eq!(
            visible_live_block_span(3, 100, 108, 100, 12),
            Some(VisibleBlockSpan {
                record_index: 3,
                first_row: 0,
                last_row: 8,
                starts_in_viewport: true,
                ends_in_viewport: true,
            })
        );
        // A scrolled/clipped intersection must not claim the real target end.
        assert_eq!(
            visible_live_block_span(3, 100, 108, 104, 4),
            Some(VisibleBlockSpan {
                record_index: 3,
                first_row: 0,
                last_row: 3,
                starts_in_viewport: false,
                ends_in_viewport: false,
            })
        );
    }

    #[test]
    fn pending_wrap_prompt_anchor_belongs_to_the_next_row() {
        // Repro shape (10x5 grid): record 0 prompts on line 0, its output
        // "0123456789" exactly fills line 1 (pending_wrap), so record 1's A
        // is anchored at (line 1, column 10) — but its prompt renders on
        // line 2. Line 1 must stay with block 0.
        let cols = 10;
        assert_eq!(prompt_row_line_id(0, 0, cols), 0);
        assert_eq!(prompt_row_line_id(1, 10, cols), 2);

        let prompt_line_ids = [
            prompt_row_line_id(0, 0, cols),
            prompt_row_line_id(1, 10, cols),
        ];
        assert_eq!(
            visible_block_spans(&prompt_line_ids, 0, 5),
            vec![
                VisibleBlockSpan {
                    record_index: 0,
                    first_row: 0,
                    last_row: 1,
                    starts_in_viewport: true,
                    ends_in_viewport: true,
                },
                VisibleBlockSpan {
                    record_index: 1,
                    first_row: 2,
                    last_row: 4,
                    starts_in_viewport: true,
                    ends_in_viewport: false,
                },
            ]
        );
        // A non-wrapped anchor is untouched, and cols == 0 never shifts.
        assert_eq!(prompt_row_line_id(7, 3, cols), 7);
        assert_eq!(prompt_row_line_id(7, 3, 0), 7);
    }

    #[test]
    fn badge_is_suppressed_when_covered_cells_hold_text() {
        let row: Vec<char> = "ls -la      ".chars().collect();
        assert!(badge_covers_only_blank_cells(&row, 6));
        assert!(!badge_covers_only_blank_cells(&row, 5));
        // '\0' padding cells count as blank.
        assert!(badge_covers_only_blank_cells(&['\0', ' ', '\0'], 0));
        // Start beyond the row is trivially blank.
        assert!(badge_covers_only_blank_cells(&row, row.len() + 4));
    }

    #[test]
    fn selection_starts_at_the_newest_selectable_block() {
        use BlockOutcome::*;
        // The live prompt block is never selectable; Running is (like the
        // gutter). A dangling selection resolves to `current: None` and both
        // directions restart at the newest selectable block.
        let outcomes = [Success, Failed(1), Background, Prompt];
        assert_eq!(
            next_selected_index(&outcomes, None, SelectStep::Older),
            Some(2)
        );
        assert_eq!(
            next_selected_index(&outcomes, None, SelectStep::Newer),
            Some(2)
        );
        let with_running = [Success, Running];
        assert_eq!(
            next_selected_index(&with_running, None, SelectStep::Older),
            Some(1)
        );
        // Nothing selectable at all.
        assert_eq!(
            next_selected_index(&[Prompt], None, SelectStep::Newer),
            None
        );
        assert_eq!(next_selected_index(&[], None, SelectStep::Older), None);
    }

    #[test]
    fn selection_moves_between_selectable_blocks_and_clamps_at_the_ends() {
        use BlockOutcome::*;
        let outcomes = [Success, Failed(1), Background, Prompt];
        assert_eq!(
            next_selected_index(&outcomes, Some(2), SelectStep::Older),
            Some(1)
        );
        assert_eq!(
            next_selected_index(&outcomes, Some(1), SelectStep::Newer),
            Some(2)
        );
        // Clamped: `None` means "keep the selection, no toast".
        assert_eq!(
            next_selected_index(&outcomes, Some(0), SelectStep::Older),
            None
        );
        assert_eq!(
            next_selected_index(&outcomes, Some(2), SelectStep::Newer),
            None
        );
        // Prompt blocks are skipped even in the middle of the timeline.
        let with_gap = [Success, Prompt, Failed(1)];
        assert_eq!(
            next_selected_index(&with_gap, Some(2), SelectStep::Older),
            Some(0)
        );
        assert_eq!(
            next_selected_index(&with_gap, Some(0), SelectStep::Newer),
            Some(2)
        );
    }

    #[test]
    fn block_selection_tracks_anchor_active_edge_and_terminal_order() {
        let ids = ["oldest", "middle", "newest"].map(str::to_string).to_vec();
        let mut selection =
            BlockSelection::all("session".to_string(), ids.clone()).expect("non-empty range");

        assert_eq!(selection.anchor_id, "oldest");
        assert_eq!(selection.active_id, "newest");
        assert_eq!(selection.selected_ids, ids);
        assert!(selection.selected_ids.iter().any(|id| id == "middle"));

        // Select-all followed by Shift+Up contracts at the newest edge.
        selection.extend_to(&ids, "middle");
        assert_eq!(selection.anchor_id, "oldest");
        assert_eq!(selection.active_id, "middle");
        assert_eq!(selection.selected_ids, ids[..=1]);

        // Moving back across the anchor remains an inclusive ordered range.
        let reverse_ids = ["zero", "oldest", "middle", "newest"]
            .map(str::to_string)
            .to_vec();
        selection.extend_to(&reverse_ids, "zero");
        assert_eq!(selection.selected_ids, reverse_ids[..=1]);
        assert_eq!(selection.active_id, "zero");
        assert_eq!(selection.anchor_id, "oldest");
    }

    #[test]
    fn stale_selection_endpoints_fail_closed_to_one_target() {
        let ids = ["a", "b", "c"].map(str::to_string).to_vec();
        assert_eq!(selected_id_range(&ids, "missing", "b"), vec!["b"]);
        assert_eq!(selected_id_range(&ids, "a", "missing"), vec!["missing"]);

        let mut selection = BlockSelection::single("session".into(), "evicted".into());
        selection.extend_to(&ids, "c");
        assert_eq!(selection.selected_ids, vec!["c"]);
        assert_eq!(selection.anchor_id, "c");
        assert_eq!(selection.active_id, "c");
        assert!(BlockSelection::all("session".into(), Vec::new()).is_none());
    }

    #[test]
    fn failed_navigation_starts_at_the_newest_failed_block() {
        use BlockOutcome::*;
        let outcomes = [Failed(1), Success, Failed(2), Background, Prompt];
        // No selection (or a dangling/cross-session one resolved to None):
        // both directions jump to the NEWEST failed block.
        assert_eq!(
            next_failed_index(&outcomes, None, SelectStep::Older),
            Some(2)
        );
        assert_eq!(
            next_failed_index(&outcomes, None, SelectStep::Newer),
            Some(2)
        );
        // Zero failed blocks → None, whether or not something is selected.
        let no_failures = [Success, Background, Running];
        assert_eq!(
            next_failed_index(&no_failures, None, SelectStep::Older),
            None
        );
        assert_eq!(
            next_failed_index(&no_failures, Some(1), SelectStep::Newer),
            None
        );
        assert_eq!(next_failed_index(&[], None, SelectStep::Older), None);
    }

    #[test]
    fn failed_navigation_steps_strictly_older_or_newer_and_clamps() {
        use BlockOutcome::*;
        let outcomes = [Failed(1), Success, Failed(2), Failed(3), Prompt];
        // From a failed block: the nearest failed strictly beyond it.
        assert_eq!(
            next_failed_index(&outcomes, Some(2), SelectStep::Older),
            Some(0)
        );
        assert_eq!(
            next_failed_index(&outcomes, Some(2), SelectStep::Newer),
            Some(3)
        );
        // Clamped at either end: silent no-op.
        assert_eq!(
            next_failed_index(&outcomes, Some(0), SelectStep::Older),
            None
        );
        assert_eq!(
            next_failed_index(&outcomes, Some(3), SelectStep::Newer),
            None
        );
        // From a NON-failed selection: strict comparison still applies.
        assert_eq!(
            next_failed_index(&outcomes, Some(1), SelectStep::Older),
            Some(0)
        );
        assert_eq!(
            next_failed_index(&outcomes, Some(1), SelectStep::Newer),
            Some(2)
        );
    }

    fn search_record(
        record_id: &str,
        command: Option<&str>,
        output: Option<&str>,
    ) -> CachedBlockSearchRecord {
        CachedBlockSearchRecord::new(record_id, command, output.map(str::to_string))
    }

    #[test]
    fn block_search_matches_commands_and_output_lines_case_insensitively() {
        let records = vec![
            search_record("new", Some("Cargo Test"), Some("running\nerror: FAILED\n")),
            search_record("old", Some("ls"), Some("cargo.toml\n")),
        ];
        let results = search_blocks(&records, "cARGo");
        assert!(!results.capped);
        assert_eq!(
            results.hits,
            vec![
                // Records are scanned in the given (newest-first) order; the
                // command hit precedes the record's output hits.
                BlockSearchHit {
                    record_id: "new".to_string(),
                    is_output_line: false,
                    line_no: None,
                    line_text: "Cargo Test".to_string(),
                    command_preview: "Cargo Test".to_string(),
                },
                BlockSearchHit {
                    record_id: "old".to_string(),
                    is_output_line: true,
                    line_no: Some(1),
                    line_text: "cargo.toml".to_string(),
                    command_preview: "ls".to_string(),
                },
            ]
        );
    }

    #[test]
    fn block_search_line_numbers_are_one_based_and_restart_per_record() {
        let records = vec![
            search_record("b", None, Some("x\nmatch here\nx\nmatch again\n")),
            search_record("a", Some("echo match"), Some("match\n")),
        ];
        let results = search_blocks(&records, "match");
        let numbering: Vec<(&str, Option<usize>, bool)> = results
            .hits
            .iter()
            .map(|hit| (hit.record_id.as_str(), hit.line_no, hit.is_output_line))
            .collect();
        assert_eq!(
            numbering,
            vec![
                ("b", Some(2), true),
                ("b", Some(4), true),
                ("a", None, false),
                ("a", Some(1), true),
            ]
        );
        // A background record (no command) still labels its hits.
        assert_eq!(results.hits[0].command_preview, "(no command)");
    }

    #[test]
    fn block_search_caps_hits_and_keeps_the_newest_records() {
        let many_lines = "match\n".repeat(MAX_BLOCK_SEARCH_HITS + 50);
        let records = vec![
            search_record("newest", None, Some(many_lines.as_str())),
            search_record("older", None, Some("match\n")),
            search_record("oldest", None, Some("match\n")),
        ];
        let results = search_blocks(&records, "match");
        assert_eq!(results.hits.len(), MAX_BLOCK_SEARCH_HITS);
        // `capped` means the scan stopped early: older content went
        // unscanned, whether or not it would have matched.
        assert!(results.capped);
        // Newest-first scan: the cap keeps recent history only.
        assert!(results.hits.iter().all(|hit| hit.record_id == "newest"));

        let records = vec![search_record("only", None, Some("match\n"))];
        let uncapped = search_blocks(&records, "match");
        assert_eq!(uncapped.hits.len(), 1);
        assert!(!uncapped.capped);
    }

    #[test]
    fn cache_build_precomputes_lowercase_and_drops_blank_commands() {
        let record = CachedBlockSearchRecord::new(
            "id",
            Some("Cargo TEST"),
            Some("Mixed Case\nSECOND".to_string()),
        );
        assert_eq!(record.record_id, "id");
        assert_eq!(record.command.as_deref(), Some("Cargo TEST"));
        assert_eq!(record.command_lowercase.as_deref(), Some("cargo test"));
        assert_eq!(record.output.as_deref(), Some("Mixed Case\nSECOND"));
        assert_eq!(
            record.output_lowercase.as_deref(),
            Some("mixed case\nsecond")
        );

        // Blank commands normalize to None (background block), and a record
        // without output caches none.
        let background = CachedBlockSearchRecord::new("bg", Some("  \t"), None);
        assert_eq!(background.command, None);
        assert_eq!(background.command_lowercase, None);
        assert_eq!(background.output, None);
        assert_eq!(background.output_lowercase, None);
    }

    #[test]
    fn block_search_ignores_empty_queries_and_clips_long_lines() {
        let records = vec![search_record("a", Some("ls"), Some("ls\n"))];
        assert_eq!(search_blocks(&records, "  ").hits, Vec::new());
        let long = format!("{}needle", "x".repeat(BLOCK_SEARCH_LINE_TEXT_CHARS));
        let records = vec![search_record("a", Some(long.as_str()), None)];
        let results = search_blocks(&records, "needle");
        // The match is beyond the clip, but the hit still reports it; the
        // display texts are clipped with an ellipsis.
        assert_eq!(results.hits.len(), 1);
        assert_eq!(
            results.hits[0].line_text,
            format!("{}…", "x".repeat(BLOCK_SEARCH_LINE_TEXT_CHARS))
        );
        assert_eq!(
            results.hits[0].command_preview,
            format!("{}…", "x".repeat(BLOCK_SEARCH_COMMAND_PREVIEW_CHARS))
        );
        // Multiline commands collapse to one display line.
        assert_eq!(single_line_clip("a\nb\tc", 10), "a b c");
        assert_eq!(single_line_clip("short", 10), "short");
    }

    #[test]
    fn markdown_fence_grows_past_the_longest_backtick_run() {
        assert_eq!(markdown_fence("plain text"), "```");
        assert_eq!(markdown_fence("inline `code` span"), "```");
        // A body containing ``` must get a 4-backtick fence.
        assert_eq!(markdown_fence("```rust\nfn x() {}\n```"), "````");
        assert_eq!(markdown_fence("````"), "`````");
        assert_eq!(markdown_fence(""), "```");
    }

    #[test]
    fn markdown_document_shape_is_pinned() {
        let doc = block_markdown(&MarkdownBlock {
            command: Some("cargo test"),
            command_exact: true,
            output: "ok\n",
            output_truncated: false,
            exit_code: Some(0),
            duration_ms: Some(1_200),
            finished: Some("2026-08-05 15:00:00 +08:00"),
            cwd: Some("/home/u/projects"),
        });
        assert_eq!(
            doc,
            "## Command Block\n\
             \n\
             - Exit: 0\n\
             - Duration: 1.2s\n\
             - Finished: 2026-08-05 15:00:00 +08:00\n\
             - Cwd: /home/u/projects\n\
             \n\
             Command:\n\
             \n\
             ```\n\
             cargo test\n\
             ```\n\
             \n\
             Output:\n\
             \n\
             ```\n\
             ok\n\
             ```\n"
        );
    }

    #[test]
    fn markdown_meta_values_cannot_forge_lines_or_smuggle_controls() {
        // PTY-controlled cwd with a forged extra meta line, an OSC sequence,
        // a C1 control, and a tab; ESC also hidden inside the command body.
        let doc = block_markdown(&MarkdownBlock {
            command: Some("echo hi\u{1b}[31m"),
            command_exact: true,
            output: "ok\n",
            output_truncated: false,
            exit_code: Some(0),
            duration_ms: None,
            finished: None,
            cwd: Some("/tmp/evil\n- Exit: 0\u{1b}]0;t\u{7}\u{9b}a\tb"),
        });
        // No control byte of any kind survives onto the clipboard.
        assert!(!doc.contains('\u{1b}'));
        assert!(!doc.contains('\u{7}'));
        assert!(!doc.contains('\u{9b}'));
        assert!(!doc.contains('\r'));
        // The embedded newline cannot mint a new `- ` meta line: the forged
        // text is folded into the single Cwd line.
        let meta_lines: Vec<&str> = doc.lines().filter(|line| line.starts_with("- ")).collect();
        assert_eq!(
            meta_lines,
            ["- Exit: 0", "- Cwd: /tmp/evil- Exit: 0]0;ta b"]
        );
        // The ESC in the command body is stripped, the rest survives.
        assert!(doc.contains("```\necho hi[31m\n```\n"));
    }

    #[test]
    fn markdown_fenced_bodies_keep_newlines_but_drop_other_controls() {
        // The ESC byte goes; its printable CSI remainder stays inert text.
        assert_eq!(
            sanitize_fenced_body("a\nb\u{1b}[2Jc\rd\u{85}e"),
            "a\nb[2Jcde"
        );
        assert_eq!(sanitize_fenced_body("tab\there"), "tabhere");
        assert_eq!(sanitize_meta_line_value("a\tb\nc\rd\u{1b}e"), "a bcde");
    }

    #[test]
    fn markdown_note_lines_flag_truncation_and_reconstruction() {
        // Reconstructed command still exports (unlike copy_block's refusal),
        // and the truncation note is the LAST meta line, after Cwd.
        let doc = block_markdown(&MarkdownBlock {
            command: Some("make build"),
            command_exact: false,
            output: "partial",
            output_truncated: true,
            exit_code: Some(0),
            duration_ms: None,
            finished: None,
            cwd: Some("/src"),
        });
        let meta_lines: Vec<&str> = doc.lines().filter(|line| line.starts_with("- ")).collect();
        assert_eq!(
            meta_lines,
            [
                "- Exit: 0",
                "- Cwd: /src",
                "- Note: command reconstructed from screen",
                "- Note: output truncated",
            ]
        );
        // A background block (no command) never carries the reconstruction
        // note, even with `command_exact: false`.
        let background = block_markdown(&MarkdownBlock {
            command: None,
            command_exact: false,
            output: "motd\n",
            output_truncated: false,
            exit_code: None,
            duration_ms: None,
            finished: None,
            cwd: None,
        });
        assert!(!background.contains("reconstructed"));
    }

    #[test]
    fn markdown_exit_line_reports_signals_and_missing_codes() {
        let doc = block_markdown(&MarkdownBlock {
            command: Some("sleep 100"),
            command_exact: true,
            output: "",
            output_truncated: false,
            exit_code: Some(130),
            duration_ms: None,
            finished: None,
            cwd: None,
        });
        // Same signal-name source as the badge; unknown metadata lines are
        // omitted, and an empty output body collapses to an empty fence pair.
        assert_eq!(
            doc,
            "## Command Block\n\
             \n\
             - Exit: 130 SIGINT\n\
             \n\
             Command:\n\
             \n\
             ```\n\
             sleep 100\n\
             ```\n\
             \n\
             Output:\n\
             \n\
             ```\n\
             ```\n"
        );
        let unreported = block_markdown(&MarkdownBlock {
            command: Some("true"),
            command_exact: true,
            output: "",
            output_truncated: false,
            exit_code: None,
            duration_ms: None,
            finished: None,
            cwd: None,
        });
        assert!(unreported.contains("- Exit: not reported\n"));
    }

    #[test]
    fn markdown_background_block_omits_command_section_and_exit_line() {
        let doc = block_markdown(&MarkdownBlock {
            command: None,
            command_exact: true,
            output: "motd\n",
            output_truncated: false,
            exit_code: Some(0),
            duration_ms: None,
            finished: None,
            cwd: None,
        });
        assert_eq!(
            doc,
            "## Command Block\n\
             \n\
             Output:\n\
             \n\
             ```\n\
             motd\n\
             ```\n"
        );
    }

    #[test]
    fn markdown_output_with_backticks_gets_a_longer_fence() {
        let doc = block_markdown(&MarkdownBlock {
            command: None,
            command_exact: true,
            output: "```rust\nfn x() {}\n```\n",
            output_truncated: false,
            exit_code: None,
            duration_ms: None,
            finished: None,
            cwd: None,
        });
        assert_eq!(
            doc,
            "## Command Block\n\
             \n\
             Output:\n\
             \n\
             ````\n\
             ```rust\nfn x() {}\n```\n\
             ````\n"
        );
    }

    #[test]
    fn local_datetime_formatting_pins_fixed_offsets() {
        assert_eq!(format_local_datetime(0, 0), "1970-01-01 00:00:00 +00:00");
        // 1_000_000_000 is 2001-09-09 01:46:40 UTC.
        assert_eq!(
            format_local_datetime(1_000_000_000, 28_800),
            "2001-09-09 09:46:40 +08:00"
        );
        assert_eq!(
            format_local_datetime(1_000_000_000, -19_800),
            "2001-09-08 20:16:40 -05:30"
        );
        // Leap day.
        assert_eq!(
            format_local_datetime(951_782_400, 0),
            "2000-02-29 00:00:00 +00:00"
        );
        assert_eq!(format_utc_offset(0), "+00:00");
        assert_eq!(format_utc_offset(28_800), "+08:00");
        assert_eq!(format_utc_offset(-19_800), "-05:30");
    }

    #[test]
    fn local_time_rolls_the_civil_day_across_the_offset() {
        // 1970-01-01 23:30:00 UTC + 8h = 07:30 on the NEXT civil day.
        assert_eq!(
            format_local_datetime(84_600, 28_800),
            "1970-01-02 07:30:00 +08:00"
        );
        // 1970-01-01 01:00:00 UTC − 5:30 = 19:30 the PREVIOUS civil day
        // (negative days through the epoch).
        assert_eq!(
            format_local_datetime(3_600, -19_800),
            "1969-12-31 19:30:00 -05:30"
        );
        assert_eq!(format_local_time_of_day(84_600, 28_800), "07:30:00");
        assert_eq!(format_local_time_of_day(3_600, -19_800), "19:30:00");
        assert_eq!(format_local_time_of_day(86_399, 0), "23:59:59");
    }

    #[test]
    fn marker_fraction_spans_the_retained_buffer() {
        assert_eq!(scrollbar_marker_fraction(100, 100, 200), Some(0.0));
        assert_eq!(scrollbar_marker_fraction(200, 100, 200), Some(1.0));
        assert_eq!(scrollbar_marker_fraction(150, 100, 200), Some(0.5));
        // Evicted (older than the retained range) or out-of-range rows draw
        // no marker.
        assert_eq!(scrollbar_marker_fraction(99, 100, 200), None);
        assert_eq!(scrollbar_marker_fraction(201, 100, 200), None);
        // Degenerate single-line buffer.
        assert_eq!(scrollbar_marker_fraction(7, 7, 7), Some(0.0));
    }

    #[test]
    fn failed_jump_picks_the_oldest_classified_failure() {
        use BlockOutcome::*;
        assert_eq!(
            oldest_failed_index(&[Success, Unknown, Failed(2), Failed(130)]),
            Some(2)
        );
        // Same failed set as the scrollbar markers and prev/next_failed:
        // Background (empty-command completion, even with a nonzero exit)
        // and Unknown (unreported exit) are NOT jump targets.
        assert_eq!(
            oldest_failed_index(&[Background, Unknown, Success, Prompt]),
            None
        );
        assert_eq!(oldest_failed_index(&[]), None);
    }
}
