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

/// Width of the per-block gutter stripe, in pixels.
pub const GUTTER_STRIPE_WIDTH: f32 = 3.0;
/// Wider stripe for the selected block.
pub const GUTTER_STRIPE_SELECTED_WIDTH: f32 = 4.0;
/// Horizontal band at `content_rect.left()` where a click selects a block
/// instead of moving the cursor or clearing the text selection.
pub const GUTTER_CLICK_BAND_PX: f32 = 6.0;

/// What a block's gutter/badge should communicate. `Unknown` (the shell never
/// reported an exit code) must never render as success: `exit_code == None`
/// is not `Some(0)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockOutcome {
    /// The live prompt block (OSC 133 A/B without C yet): separator only,
    /// no gutter stripe.
    Prompt,
    /// The newest record is between C and D: accent-colored stripe, no badge.
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
/// `TerminalState::running_command`.
pub fn classify_outcome(
    command: Option<&str>,
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
            let has_command = command.is_some_and(|command| !command.trim().is_empty());
            if !has_command {
                return BlockOutcome::Background;
            }
            match exit_code {
                Some(0) => BlockOutcome::Success,
                Some(code) => BlockOutcome::Failed(code),
                None => BlockOutcome::Unknown,
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

/// Badge text for the block's first row, or `None` for outcomes that carry no
/// badge (running, live prompt, background). `Unknown` is a bare `?` glyph:
/// anvil/4 show no exit badge for an unreported status.
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
        BlockOutcome::Unknown => Some("?".to_string()),
        BlockOutcome::Prompt | BlockOutcome::Running | BlockOutcome::Background => None,
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
}

/// Compute the viewport intersection of every block. `prompt_line_ids` are
/// the records' `prompt_start.line_id` values in record order; block *i* ends
/// on the line before block *i+1* starts, and the last block extends to the
/// viewport bottom (the live tail).
pub fn visible_block_spans(
    prompt_line_ids: &[u64],
    top_line_id: u64,
    viewport_rows: usize,
) -> Vec<VisibleBlockSpan> {
    let mut spans = Vec::new();
    if viewport_rows == 0 {
        return spans;
    }
    let bottom_line_id = top_line_id + viewport_rows as u64 - 1;
    for (record_index, &start) in prompt_line_ids.iter().enumerate() {
        let end = match prompt_line_ids.get(record_index + 1) {
            Some(next_start) => {
                let Some(end) = next_start.checked_sub(1).filter(|end| *end >= start) else {
                    continue; // 相邻 prompt 落在同一行:该块没有可见行。
                };
                end
            }
            None => bottom_line_id.max(start),
        };
        if end < top_line_id || start > bottom_line_id {
            continue;
        }
        spans.push(VisibleBlockSpan {
            record_index,
            first_row: (start.max(top_line_id) - top_line_id) as usize,
            last_row: (end.min(bottom_line_id) - top_line_id) as usize,
            starts_in_viewport: start >= top_line_id,
        });
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

/// Index of the OLDEST record that completed with a nonzero exit code.
/// `None` exit codes are unreported, not failures.
pub fn oldest_failed_index<I>(exit_codes: I) -> Option<usize>
where
    I: IntoIterator<Item = Option<i32>>,
{
    exit_codes
        .into_iter()
        .position(|code| matches!(code, Some(code) if code != 0))
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
        let complete = |command: Option<&str>, exit: Option<i32>| {
            classify_outcome(command, exit, CommandState::Complete, true, true)
        };
        assert_eq!(complete(Some("cargo test"), Some(0)), BlockOutcome::Success);
        assert_eq!(
            complete(Some("cargo test"), Some(101)),
            BlockOutcome::Failed(101)
        );
        // `None` must never be treated as 0.
        assert_eq!(complete(Some("cargo test"), None), BlockOutcome::Unknown);
        assert_ne!(complete(Some("cargo test"), None), BlockOutcome::Success);
        // No/empty command → background, even with a reported exit code.
        assert_eq!(complete(None, Some(0)), BlockOutcome::Background);
        assert_eq!(complete(Some("  \t"), Some(1)), BlockOutcome::Background);
    }

    #[test]
    fn only_the_newest_incomplete_record_counts_as_running() {
        assert_eq!(
            classify_outcome(Some("sleep 9"), None, CommandState::Running, false, true),
            BlockOutcome::Running
        );
        // 老的 Running 记录意味着它的 D 丢了,按 Unknown 处理。
        assert_eq!(
            classify_outcome(Some("sleep 9"), None, CommandState::Running, false, false),
            BlockOutcome::Unknown
        );
        for state in [CommandState::Prompt, CommandState::Editing] {
            assert_eq!(
                classify_outcome(None, None, state, false, true),
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
        // Unreported exit: bare `?`, no exit badge.
        assert_eq!(
            badge_text(BlockOutcome::Unknown, Some(2_300)),
            Some("?".to_string())
        );
        assert_eq!(badge_text(BlockOutcome::Running, Some(10)), None);
        assert_eq!(badge_text(BlockOutcome::Prompt, None), None);
        assert_eq!(badge_text(BlockOutcome::Background, Some(10)), None);
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
                },
                VisibleBlockSpan {
                    record_index: 1,
                    first_row: 3,
                    last_row: 6,
                    starts_in_viewport: true,
                },
                // 最后一个块一直延伸到 viewport 底部(live tail)。
                VisibleBlockSpan {
                    record_index: 2,
                    first_row: 7,
                    last_row: 9,
                    starts_in_viewport: true,
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
                },
                VisibleBlockSpan {
                    record_index: 1,
                    first_row: 5,
                    last_row: 9,
                    starts_in_viewport: true,
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
            }]
        );
        assert!(visible_block_spans(&[10], 100, 0).is_empty());
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
                },
                VisibleBlockSpan {
                    record_index: 1,
                    first_row: 2,
                    last_row: 4,
                    starts_in_viewport: true,
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
    fn failed_jump_picks_the_oldest_failure() {
        assert_eq!(
            oldest_failed_index([Some(0), None, Some(2), Some(130)]),
            Some(2)
        );
        assert_eq!(oldest_failed_index([Some(0), None]), None);
        assert_eq!(oldest_failed_index([]), None);
    }
}
