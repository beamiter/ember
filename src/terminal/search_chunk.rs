//! Search-only bounded raw extraction. No totals, head/tail traversal or
//! history-wide identity search. Copy/render extraction is deliberately separate.
use super::{BufferAnchor, RawRowId, TerminalState, MAX_COMMAND_MARKS, MAX_OSC_133_ID_BYTES};
use crate::block_mode::{block_search_literal_normalize, block_search_literal_scalar};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);
pub(super) fn next_source_epoch() -> u64 {
    NEXT_SOURCE
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchSourceVersion {
    pub epoch: u64,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct SearchChunkBudget {
    pub rows: usize,
    pub cells: usize,
    pub bytes: usize,
    pub encoded_bytes: usize,
}
impl Default for SearchChunkBudget {
    fn default() -> Self {
        Self {
            rows: 32,
            cells: 16_384,
            bytes: 65_536,
            encoded_bytes: 262_144,
        }
    }
}
impl SearchChunkBudget {
    fn bounded(self) -> Option<Self> {
        (self.rows > 0 && self.cells > 0 && self.bytes >= 4 && self.encoded_bytes > 0).then_some(
            Self {
                rows: self.rows.min(256),
                cells: self.cells.min(65_536),
                bytes: self.bytes.min(65_536),
                encoded_bytes: self.encoded_bytes.min(262_144),
            },
        )
    }
}

/// Charged work: full row admission precedes any decode. Metadata lookups are
/// capped by MAX_COMMAND_MARKS; emitted bytes never require scanning a suffix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchChunkWork {
    pub records_examined: usize,
    pub rows_decoded: usize,
    pub cells_charged: usize,
    pub encoded_bytes_charged: usize,
    pub bytes_emitted: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawSearchCursor {
    pub source: SearchSourceVersion,
    pub record_sequence: u64,
    pub record_id: String,
    pub start: BufferAnchor,
    pub end: BufferAnchor,
    pub next: BufferAnchor,
    pub row_id: RawRowId,
    pub at_logical_line_start: bool,
}
/// Seal for a completed range entirely in immutable primary scrollback.
/// Only append and prefix eviction preserve this epoch. All fields are private
/// so callers cannot mint a seal or reinterpret a mutable-source cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawSearchSeal {
    source: SearchSourceVersion,
    record_sequence: u64,
    record_id: String,
    start: BufferAnchor,
    end: BufferAnchor,
    start_row: RawRowId,
    end_row: RawRowId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawSearchSpan {
    pub bytes: std::ops::Range<usize>,
    pub row_id: RawRowId,
    pub anchor: BufferAnchor,
}
#[derive(Clone, Debug)]
pub struct RawSearchChunk {
    pub record_sequence: u64,
    pub record_id: String,
    pub start: BufferAnchor,
    pub text: String,
    pub spans: Vec<RawSearchSpan>,
    pub source: SearchSourceVersion,
    pub starts_logical_line: bool,
    pub ends_logical_line: bool,
    pub continuation: Option<RawSearchCursor>,
    pub work: SearchChunkWork,
}
impl RawSearchChunk {
    /// Existing per-logical-line regex semantics are valid only when both
    /// boundary lines are complete. A partial line is explicitly unsupported
    /// by that matcher, never a complete negative result.
    pub fn supports_complete_line_regex(&self) -> bool {
        self.starts_logical_line && self.ends_logical_line
    }

    /// Byte ranges for complete logical lines, preserving existing regex
    /// behavior even when another boundary line in this chunk is partial.
    /// Consumers must still disclose skipped partial boundary lines.
    pub fn complete_logical_lines(&self) -> Vec<std::ops::Range<usize>> {
        let mut ranges = Vec::new();
        let mut start = 0;
        let mut eligible = self.starts_logical_line;
        for (end, _) in self.text.match_indices('\n') {
            if eligible {
                ranges.push(start..end);
            }
            start = end + 1;
            eligible = true;
        }
        if eligible && self.ends_logical_line && start < self.text.len() {
            ranges.push(start..self.text.len());
        }
        ranges
    }
}
#[derive(Clone, Debug)]
pub enum SearchChunkOutcome {
    Chunk(RawSearchChunk),
    Stale,
    Unavailable,
    BudgetTooSmall,
    Unsupported(&'static str),
}

impl TerminalState {
    pub(super) fn invalidate_search_scrollback_seals(&mut self) {
        // Globally unique epochs prevent reset/cross-pane ABA. Exhaustion is
        // sticky: no old seal may become valid after the allocator runs out.
        self.search_scrollback_epoch = next_source_epoch();
    }

    pub fn seal_search_output(
        &self,
        sequence: u64,
        id: &str,
    ) -> Result<RawSearchSeal, SearchChunkOutcome> {
        self.seal_search_output_with_work(sequence, id)
            .map(|(seal, _)| seal)
    }

    pub(super) fn seal_search_output_with_work(
        &self,
        sequence: u64,
        id: &str,
    ) -> Result<(RawSearchSeal, usize), SearchChunkOutcome> {
        use SearchChunkOutcome::*;
        if self.use_alt_buffer
            || self.search_scrollback_epoch == 0
            || self.total_lines_scrolled == u64::MAX
        {
            return Err(Unsupported("unsealable scrollback source"));
        }
        let mut records_examined = 0;
        let record = self
            .command_records
            .iter()
            .take(MAX_COMMAND_MARKS)
            .find(|record| {
                records_examined += 1;
                record.sequence == sequence && record.id == id
            })
            .ok_or(Unavailable)?;
        if !record.complete || id.len() > MAX_OSC_133_ID_BYTES {
            return Err(Unsupported("incomplete record or oversized identity"));
        }
        let (start, end) = record
            .output_start
            .zip(record.output_end)
            .ok_or(Unavailable)?;
        if end < start {
            return Err(Unsupported("invalid output range"));
        }
        let endpoint = |anchor| {
            let (absolute, column) = self.buffer_anchor_to_absolute(anchor).ok_or(Unavailable)?;
            if absolute >= self.scrollback.len() {
                return Err(Unsupported("output still touches mutable grid"));
            }
            let (row, width) = self.retained_raw_row(absolute).ok_or(Unavailable)?;
            if !row.is_tracked() || width > super::MAX_TERMINAL_COLS || column > width {
                return Err(Unsupported("invalid retained endpoint"));
            }
            Ok(row)
        };
        Ok((
            RawSearchSeal {
                source: SearchSourceVersion {
                    epoch: self.search_scrollback_epoch,
                    revision: 1,
                },
                record_sequence: sequence,
                record_id: id.to_owned(),
                start,
                end,
                start_row: endpoint(start)?,
                end_row: endpoint(end)?,
            },
            records_examined,
        ))
    }

    pub fn validate_search_seal(&self, seal: &RawSearchSeal) -> Result<(), SearchChunkOutcome> {
        self.validate_search_seal_with_work(seal).map(|_| ())
    }

    fn validate_search_seal_with_work(
        &self,
        seal: &RawSearchSeal,
    ) -> Result<usize, SearchChunkOutcome> {
        if seal.source.epoch != self.search_scrollback_epoch || seal.source.epoch == 0 {
            return Err(SearchChunkOutcome::Stale);
        }
        let (current, records_examined) =
            self.seal_search_output_with_work(seal.record_sequence, &seal.record_id)?;
        if current != *seal {
            return Err(SearchChunkOutcome::Stale);
        }
        Ok(records_examined)
    }

    pub fn search_sealed_output_chunk(
        &self,
        seal: &RawSearchSeal,
        cursor: Option<&RawSearchCursor>,
        budget: SearchChunkBudget,
    ) -> SearchChunkOutcome {
        let records_examined = match self.validate_search_seal_with_work(seal) {
            Ok(work) => work,
            Err(outcome) => return outcome,
        };
        let mut outcome = self.search_output_chunk_with_source(
            seal.record_sequence,
            &seal.record_id,
            cursor,
            budget,
            seal.source,
        );
        if let SearchChunkOutcome::Chunk(chunk) = &mut outcome {
            chunk.work.records_examined += records_examined;
        }
        outcome
    }

    pub(super) fn invalidate_search_source(&mut self) {
        if self.search_source_revision != 0 {
            self.search_source_revision = self.search_source_revision.checked_add(1).unwrap_or(0);
        }
    }
    pub fn search_source_version(&self) -> SearchSourceVersion {
        SearchSourceVersion {
            epoch: self.search_source_epoch,
            revision: self.search_source_revision,
        }
    }

    /// One bounded raw subwindow for one retained completed record. A cursor
    /// can only continue against the exact source instance/revision/record.
    /// No capture fallback, total-byte computation, or global RawRowId walk.
    pub fn search_output_chunk(
        &self,
        sequence: u64,
        id: &str,
        cursor: Option<&RawSearchCursor>,
        budget: SearchChunkBudget,
    ) -> SearchChunkOutcome {
        self.search_output_chunk_with_source(
            sequence,
            id,
            cursor,
            budget,
            self.search_source_version(),
        )
    }

    fn search_output_chunk_with_source(
        &self,
        sequence: u64,
        id: &str,
        cursor: Option<&RawSearchCursor>,
        budget: SearchChunkBudget,
        source: SearchSourceVersion,
    ) -> SearchChunkOutcome {
        use SearchChunkOutcome::*;
        let Some(budget) = budget.bounded() else {
            return BudgetTooSmall;
        };
        if source.epoch == 0 || source.revision == 0 {
            return Unsupported("source revision exhausted");
        }
        if self.use_alt_buffer {
            return Unsupported("alternate screen");
        }
        if cursor.is_some_and(|c| {
            c.source != source || c.record_sequence != sequence || c.record_id != id
        }) {
            return Stale;
        }
        let mut work = SearchChunkWork::default();
        let record = self
            .command_records
            .iter()
            .take(MAX_COMMAND_MARKS)
            .find(|record| {
                work.records_examined += 1;
                record.sequence == sequence && record.id == id
            });
        let Some(record) = record else {
            return Unavailable;
        };
        if !record.complete || record.id.len() > MAX_OSC_133_ID_BYTES {
            return Unsupported("incomplete record or oversized identity");
        }
        let Some((start, end)) = record.output_start.zip(record.output_end) else {
            return Unavailable;
        };
        if end < start {
            return Unsupported("invalid output range");
        }
        if cursor
            .is_some_and(|c| c.start != start || c.end != end || c.next < start || c.next > end)
        {
            return Stale;
        }
        // Both anchors must still belong to retained raw data. Do not silently
        // clamp an evicted prefix into a complete-coverage claim.
        if self.buffer_anchor_to_absolute(start).is_none()
            || self.buffer_anchor_to_absolute(end).is_none()
        {
            return Unavailable;
        }
        let mut next = cursor.map_or(start, |c| c.next);
        let mut line_start = cursor.is_none_or(|c| c.at_logical_line_start);
        let starts_logical_line = line_start;
        let chunk_start = next;
        let mut text = String::with_capacity(budget.bytes);
        let mut spans = Vec::with_capacity((budget.cells + budget.rows).min(budget.bytes));
        let mut complete = false;
        loop {
            if text.len() == budget.bytes {
                break;
            }
            let Some((absolute, _)) = self.buffer_anchor_to_absolute(next) else {
                return Unavailable;
            };
            let Some((row_id, width)) = self.retained_raw_row(absolute) else {
                return Unavailable;
            };
            if width > super::MAX_TERMINAL_COLS {
                return Unsupported("invalid row width");
            }
            if !row_id.is_tracked() {
                return Unsupported("untracked raw row");
            }
            if next == cursor.map_or(start, |c| c.next)
                && cursor.is_some_and(|c| c.row_id != row_id)
            {
                return Stale;
            }
            let encoded = if absolute < self.scrollback.len() {
                self.scrollback[absolute].search_encoded_bytes()
            } else {
                0
            };
            if work.rows_decoded == budget.rows
                || work.cells_charged.saturating_add(width) > budget.cells
                || work.encoded_bytes_charged.saturating_add(encoded) > budget.encoded_bytes
            {
                if work.rows_decoded == 0 {
                    return BudgetTooSmall;
                }
                break;
            }
            // Reserve the whole row before decoding, including cells omitted
            // by trimming/wide continuation and any repeated partial-row read.
            work.rows_decoded += 1;
            work.cells_charged += width;
            work.encoded_bytes_charged += encoded;
            let cells = if absolute < self.scrollback.len() {
                let Some(cells) = self.scrollback[absolute].decode_search_row(width, encoded)
                else {
                    return Unsupported("invalid compressed row");
                };
                cells
            } else {
                self.grid[absolute - self.scrollback.len()].to_vec()
            };
            if cells.len() != width || width > super::MAX_TERMINAL_COLS {
                return Unsupported("invalid row width");
            }
            let mut row_end = if next.line_id == end.line_id {
                end.column.min(width)
            } else {
                width
            };
            let row_start = next.column.min(width);
            while row_end > row_start
                && matches!(cells[row_end - 1].character, ' ' | '\0')
                && !cells[row_end - 1].flags.wide_continuation()
            {
                row_end -= 1;
            }
            let mut stopped = false;
            for (column, cell) in cells.iter().enumerate().take(row_end).skip(row_start) {
                next.column = column;
                if cell.flags.wide_continuation() {
                    next.column = column + 1;
                    continue;
                }
                if text.len() + cell.character.len_utf8() > budget.bytes {
                    stopped = true;
                    break;
                }
                let offset = text.len();
                text.push(cell.character);
                spans.push(RawSearchSpan {
                    bytes: offset..text.len(),
                    row_id,
                    anchor: next,
                });
                line_start = false;
                next.column = column + 1;
            }
            if stopped {
                break;
            }
            next.column = row_end.max(row_start);
            if next.line_id == end.line_id {
                complete = true;
                line_start = true;
                break;
            }
            if !self.absolute_row_is_wrapped(absolute).unwrap_or(false) {
                if text.len() == budget.bytes {
                    break;
                }
                let offset = text.len();
                text.push('\n');
                spans.push(RawSearchSpan {
                    bytes: offset..text.len(),
                    row_id,
                    anchor: next,
                });
                line_start = true;
            }
            next = BufferAnchor {
                line_id: next.line_id.saturating_add(1),
                column: 0,
            };
        }
        work.bytes_emitted = text.len();
        let continuation = if complete {
            None
        } else {
            let Some((absolute, _)) = self.buffer_anchor_to_absolute(next) else {
                return Unavailable;
            };
            let Some((row_id, _)) = self.retained_raw_row(absolute) else {
                return Unavailable;
            };
            Some(RawSearchCursor {
                source,
                record_sequence: sequence,
                record_id: id.to_owned(),
                start,
                end,
                next,
                row_id,
                at_logical_line_start: line_start,
            })
        };
        Chunk(RawSearchChunk {
            record_sequence: sequence,
            record_id: id.to_owned(),
            start: chunk_start,
            text,
            spans,
            source,
            starts_logical_line,
            ends_logical_line: line_start,
            continuation,
            work,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn terminal(output: &str, cols: usize, history: usize) -> TerminalState {
        let mut terminal = TerminalState::new(cols, 4);
        terminal.set_max_scrollback(history);
        terminal.process_input(b"\x1b]133;A\x07\x1b]133;C;id=raw\x07");
        terminal.process_input(output.as_bytes());
        terminal.process_input(b"\x1b]133;D;0;id=raw\x07");
        terminal
    }
    fn chunk(
        t: &TerminalState,
        c: Option<&RawSearchCursor>,
        b: SearchChunkBudget,
    ) -> RawSearchChunk {
        let record = t.command_records().back().unwrap();
        match t.search_output_chunk(record.sequence, &record.id, c, b) {
            SearchChunkOutcome::Chunk(c) => c,
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn search_chunk_budgets_and_continuation_cover_middle_of_100000_lines() {
        let line = format!("{}\r\n", "x".repeat(95));
        let mut output = line.repeat(100_000);
        output.replace_range(
            50_000 * line.len()..50_000 * line.len() + 12,
            "MIDDLE_TOKEN",
        );
        let t = terminal(&output, 96, 120_000);
        let r = t.command_records().back().unwrap();
        assert!(r.captured_output.as_ref().unwrap().truncated);
        assert!(!r
            .captured_output
            .as_ref()
            .unwrap()
            .text
            .contains("MIDDLE_TOKEN"));
        let t = parking_lot::Mutex::new(t);
        let budget = SearchChunkBudget::default();
        let mut cursor = None;
        let mut calls = 0;
        let mut peak_work = SearchChunkWork::default();
        let mut total = 0;
        let mut matches = 0;
        let mut previous = None;
        let mut extract_us = Vec::new();
        let mut normalize_us = Vec::new();
        let mut lock_us = Vec::new();
        let mut matcher = RawLiteralOverlap::new("middle_token", false).unwrap();
        let mut literal_hits = 0;
        let sweep_started = std::time::Instant::now();
        loop {
            let started = std::time::Instant::now();
            let guard = t.lock();
            lock_us.push(started.elapsed().as_micros());
            let started = std::time::Instant::now();
            let c = chunk(&guard, cursor.as_ref(), budget);
            extract_us.push(started.elapsed().as_micros());
            drop(guard);
            let started = std::time::Instant::now();
            literal_hits += matcher.consume(&c).unwrap().hits.len();
            normalize_us.push(started.elapsed().as_micros());
            calls += 1;
            peak_work.records_examined = peak_work.records_examined.max(c.work.records_examined);
            peak_work.rows_decoded = peak_work.rows_decoded.max(c.work.rows_decoded);
            peak_work.cells_charged = peak_work.cells_charged.max(c.work.cells_charged);
            peak_work.encoded_bytes_charged = peak_work
                .encoded_bytes_charged
                .max(c.work.encoded_bytes_charged);
            peak_work.bytes_emitted = peak_work.bytes_emitted.max(c.work.bytes_emitted);
            assert!(c.work.rows_decoded <= budget.rows);
            assert!(c.work.cells_charged <= budget.cells);
            assert!(c.work.encoded_bytes_charged <= budget.encoded_bytes);
            assert!(c.work.bytes_emitted <= budget.bytes);
            assert!(c.work.records_examined <= MAX_COMMAND_MARKS);
            total += c.text.len();
            matches += c.text.matches("MIDDLE_TOKEN").count();
            for span in &c.spans {
                if let Some(anchor) = previous {
                    assert!(span.anchor > anchor);
                }
                previous = Some(span.anchor);
            }
            assert!(c.supports_complete_line_regex());
            cursor = c.continuation;
            if cursor.is_none() {
                break;
            }
            assert!(calls < 10_000);
        }
        println!("actual peak work per call: {peak_work:?}; configured: {budget:?}");
        assert_eq!(literal_hits, 1);
        extract_us.sort_unstable();
        normalize_us.sort_unstable();
        lock_us.sort_unstable();
        println!("uncontended terminal lock acquisition p95={}us max={}us; extraction timer is lock-held", lock_us[lock_us.len()*95/100], lock_us.last().unwrap());
        println!("actual raw extraction p95={}us max={}us; literal normalize/match p95={}us max={}us; sequential sweep={}ms; span size={} bytes", extract_us[extract_us.len()*95/100], extract_us.last().unwrap(), normalize_us[normalize_us.len()*95/100], normalize_us.last().unwrap(), sweep_started.elapsed().as_millis(), std::mem::size_of::<RawSearchSpan>());
        assert_eq!(matches, 1);
        assert_eq!(total, 9_600_000);
        println!("100000 lines, {total} bytes, {calls} bounded raw chunks; omitted captured middle found");
    }
    #[test]
    fn search_chunk_partial_soft_line_is_not_complete_regex_negative() {
        let t = terminal(&"a".repeat(150), 16, 100);
        let budget = SearchChunkBudget {
            rows: 1,
            cells: 16,
            bytes: 8,
            encoded_bytes: 1024,
        };
        let first = chunk(&t, None, budget);
        assert_eq!(first.text, "aaaaaaaa");
        assert!(!first.supports_complete_line_regex());
        assert!(first.continuation.is_some());
        let mut all = first.text.clone();
        let mut cursor = first.continuation;
        while let Some(c) = cursor {
            let next = chunk(&t, Some(&c), budget);
            assert!(!next.supports_complete_line_regex());
            all.push_str(&next.text);
            cursor = next.continuation;
        }
        assert_eq!(all, "a".repeat(150));
    }
    #[test]
    fn search_chunk_rejects_too_small_decode_before_work() {
        let t = terminal(&"hello\r\n".repeat(10), 16, 100);
        let r = t.command_records().back().unwrap();
        assert!(matches!(
            t.search_output_chunk(
                r.sequence,
                &r.id,
                None,
                SearchChunkBudget {
                    cells: 15,
                    ..Default::default()
                }
            ),
            SearchChunkOutcome::BudgetTooSmall
        ));
        assert!(matches!(
            t.search_output_chunk(
                r.sequence,
                &r.id,
                None,
                SearchChunkBudget {
                    encoded_bytes: 1,
                    ..Default::default()
                }
            ),
            SearchChunkOutcome::BudgetTooSmall
        ));
    }
    #[test]
    fn search_chunk_writes_resize_eviction_reset_and_panes_invalidate() {
        for operation in 0..7 {
            let mut t = terminal(&"abc\r\n".repeat(40), 16, 100);
            let c = chunk(
                &t,
                None,
                SearchChunkBudget {
                    rows: 1,
                    ..Default::default()
                },
            )
            .continuation
            .unwrap();
            match operation {
                0 => t.process_input(b"X"),
                1 => t.on_resize(20, 4),
                2 => t.set_max_scrollback(2),
                3 => t.process_input(b"\x1bc"),
                4 => t.process_input(b"\x1b[3J"),
                5 => t.process_input(b"\x1b[?1049h\x1b[?1049l"),
                _ => t.process_input(b"abcdefghijklmnopqrstuvwxyz"),
            };
            assert!(matches!(
                t.search_output_chunk(
                    c.record_sequence,
                    &c.record_id,
                    Some(&c),
                    SearchChunkBudget::default()
                ),
                SearchChunkOutcome::Stale
            ));
        }
        let a = terminal(&"abc\r\n".repeat(40), 16, 100);
        let b = terminal(&"abc\r\n".repeat(40), 16, 100);
        let c = chunk(
            &a,
            None,
            SearchChunkBudget {
                rows: 1,
                ..Default::default()
            },
        )
        .continuation
        .unwrap();
        assert!(matches!(
            b.search_output_chunk(
                c.record_sequence,
                &c.record_id,
                Some(&c),
                SearchChunkBudget::default()
            ),
            SearchChunkOutcome::Stale
        ));
    }
    #[test]
    fn search_chunk_no_extra_decode_when_byte_limit_reached() {
        let t = terminal("abcd\r\nefgh\r\nijkl", 8, 100);
        let c = chunk(
            &t,
            None,
            SearchChunkBudget {
                bytes: 4,
                ..Default::default()
            },
        );
        assert_eq!(c.text, "abcd");
        assert_eq!(c.work.rows_decoded, 1);
        let c2 = chunk(
            &t,
            c.continuation.as_ref(),
            SearchChunkBudget {
                bytes: 4,
                ..Default::default()
            },
        );
        assert_eq!(c2.text, "\nefg");
    }
    #[test]
    fn search_chunk_blank_rows_and_unicode_make_progress() {
        let t = terminal("\r\n\r\n中🙂\r\n", 8, 100);
        let b = SearchChunkBudget {
            bytes: 4,
            rows: 1,
            ..Default::default()
        };
        let mut cursor = None;
        let mut result = String::new();
        let mut calls = 0;
        loop {
            let c = chunk(&t, cursor.as_ref(), b);
            result.push_str(&c.text);
            calls += 1;
            cursor = c.continuation;
            if cursor.is_none() {
                break;
            }
            assert!(calls < 20);
        }
        assert_eq!(result, "\n\n中🙂\n");
    }
}

/// Explicit overlap limit; no unbounded logical line is ever retained.
pub const RAW_LITERAL_QUERY_MAX_BYTES: usize = 4096;
pub const RAW_LITERAL_MAX_HITS: usize = 500;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawLiteralHit {
    pub start: RawSearchSpan,
    pub end: RawSearchSpan,
}
#[derive(Clone, Debug)]
pub struct RawLiteralMatches {
    pub hits: Vec<RawLiteralHit>,
    pub capped: bool,
    pub continuation: bool,
}
/// Streaming substring matcher only. Regex consumers must separately honor
/// supports_complete_line_regex; they cannot treat a partial chunk as a line.
pub struct RawLiteralOverlap {
    needle: String,
    case_sensitive: bool,
    tail: String,
    tail_spans: Vec<RawSearchSpan>,
    expected: Option<RawSearchCursor>,
    started: bool,
    normalized_consumed: usize,
    next_match_offset: usize,
}
impl RawLiteralOverlap {
    pub fn new(query: &str, case_sensitive: bool) -> Result<Self, &'static str> {
        if query.is_empty()
            || query.len() > RAW_LITERAL_QUERY_MAX_BYTES
            || query.chars().any(char::is_control)
        {
            return Err("literal query must be 1..=4096 bytes without controls");
        }
        let query = crate::block_mode::validated_block_search_query(query)
            .map_err(|_| "invalid Block Search query")?;
        if query.is_empty() {
            return Err("empty literal query");
        }
        let needle = if case_sensitive {
            query.to_owned()
        } else {
            block_search_literal_normalize(query)
        };
        if needle.len() > RAW_LITERAL_QUERY_MAX_BYTES * 3 {
            return Err("normalized literal query exceeds 12288 bytes");
        }
        Ok(Self {
            needle,
            case_sensitive,
            tail: String::new(),
            tail_spans: Vec::new(),
            expected: None,
            started: false,
            normalized_consumed: 0,
            next_match_offset: 0,
        })
    }
    pub fn consume(&mut self, chunk: &RawSearchChunk) -> Result<RawLiteralMatches, &'static str> {
        if self.started
            && self.expected.as_ref().is_none_or(|c| {
                c.source != chunk.source
                    || c.record_sequence != chunk.record_sequence
                    || c.record_id != chunk.record_id
                    || c.next != chunk.start
            })
        {
            return Err("nonconsecutive or stale raw chunk");
        }
        // The chunk type is public. Validate its bounded scalar provenance
        // before allocating or consuming it, including caller-built values.
        if chunk.text.len() > 65_536 || chunk.spans.len() > 65_792 {
            return Err("oversized raw chunk");
        }
        let mut previous_end = 0;
        for span in &chunk.spans {
            if span.bytes.start != previous_end || span.bytes.len() > 4 {
                return Err("noncontiguous raw provenance");
            }
            let Some(raw) = chunk.text.get(span.bytes.clone()) else {
                return Err("invalid raw provenance");
            };
            if raw.chars().count() != 1 {
                return Err("invalid scalar provenance");
            }
            previous_end = span.bytes.end;
        }
        if previous_end != chunk.text.len() {
            return Err("incomplete raw provenance");
        }
        let carry = self.tail.len();
        // Count bounded normalization before allocating. Exact reservations
        // avoid geometric capacity growth while old chunk + carry are live.
        let (mut bytes, mut scalars) = (carry, self.tail_spans.len());
        for character in chunk.text.chars() {
            if self.case_sensitive {
                bytes += character.len_utf8();
                scalars += 1;
            } else {
                for lower in block_search_literal_scalar(character) {
                    bytes += lower.len_utf8();
                    scalars += 1;
                }
            }
        }
        if bytes > carry + 12 * 65_536 || scalars > self.tail_spans.len() + 3 * 65_536 {
            return Err("unsupported normalization expansion");
        }
        let mut text = String::with_capacity(bytes);
        text.push_str(&self.tail);
        let mut spans = Vec::with_capacity(scalars);
        spans.extend_from_slice(&self.tail_spans);
        self.tail = String::new();
        self.tail_spans = Vec::new();
        for span in &chunk.spans {
            let Some(raw) = chunk.text.get(span.bytes.clone()) else {
                return Err("invalid raw provenance");
            };
            for character in raw.chars() {
                if self.case_sensitive {
                    let start = text.len();
                    text.push(character);
                    let mut mapped = span.clone();
                    mapped.bytes = start..text.len();
                    spans.push(mapped);
                } else {
                    for lower in block_search_literal_scalar(character) {
                        let start = text.len();
                        text.push(lower);
                        let mut mapped = span.clone();
                        mapped.bytes = start..text.len();
                        spans.push(mapped);
                    }
                }
            }
        }
        let mut hits = Vec::new();
        let mut capped = false;
        let base = self
            .normalized_consumed
            .checked_sub(carry)
            .ok_or("invalid carry offset")?;
        let consumed = base
            .checked_add(text.len())
            .ok_or("literal stream offset exhausted")?;
        // Preserve nonoverlapping substring semantics across chunk boundaries.
        // Restarting match_indices on overlap can invent overlapping matches.
        let mut scan = self.next_match_offset.saturating_sub(base);
        while scan <= text.len() {
            let Some(relative) = text[scan..].find(&self.needle) else {
                break;
            };
            let start = scan + relative;
            let end = start + self.needle.len();
            scan = end;
            self.next_match_offset = base + end;
            if end <= carry {
                continue;
            }
            if hits.len() == RAW_LITERAL_MAX_HITS {
                capped = true;
                break;
            }
            let first = spans.partition_point(|span| span.bytes.end <= start);
            let last = spans.partition_point(|span| span.bytes.start < end);
            if first >= last {
                return Err("missing raw provenance");
            }
            hits.push(RawLiteralHit {
                start: spans[first].clone(),
                end: spans[last - 1].clone(),
            });
        }
        // Retain only enough normalized UTF-8 to complete one bounded query.
        let mut keep_from = text
            .len()
            .saturating_sub(self.needle.len().saturating_sub(1));
        while keep_from < text.len() && !text.is_char_boundary(keep_from) {
            keep_from += 1;
        }
        let tail = text[keep_from..].to_owned();
        // Borrow then clone: into_iter/filter/collect may retain the entire
        // normalized chunk allocation as the tiny carry buffer.
        let tail_start = spans.partition_point(|span| span.bytes.start < keep_from);
        let tail_spans = spans[tail_start..]
            .iter()
            .cloned()
            .map(|mut span| {
                span.bytes.start -= keep_from;
                span.bytes.end -= keep_from;
                span
            })
            .collect();
        self.normalized_consumed = consumed;
        self.tail = tail;
        self.tail_spans = tail_spans;
        self.expected = chunk.continuation.clone();
        self.started = true;
        Ok(RawLiteralMatches {
            hits,
            capped,
            continuation: self.expected.is_some(),
        })
    }
}

#[cfg(test)]
mod literal_tests {
    use super::*;
    #[test]
    fn search_chunk_literal_overlap_is_bounded_and_deduplicated_by_provenance() {
        let mut t = TerminalState::new(32, 4);
        t.process_input(
            b"\x1b]133;A\x07\x1b]133;C;id=raw\x07xxNeedleNeedlezz\x1b]133;D;0;id=raw\x07",
        );
        let r = t.command_records().back().unwrap();
        let mut matcher = RawLiteralOverlap::new("needle", false).unwrap();
        let mut cursor = None;
        let mut starts = Vec::new();
        loop {
            let SearchChunkOutcome::Chunk(c) = t.search_output_chunk(
                r.sequence,
                &r.id,
                cursor.as_ref(),
                SearchChunkBudget {
                    bytes: 4,
                    ..Default::default()
                },
            ) else {
                panic!()
            };
            let result = matcher.consume(&c).unwrap();
            assert!(!result.capped);
            for hit in result.hits {
                starts.push(hit.start.anchor.column);
            }
            assert!(matcher.tail.len() < matcher.needle.len());
            cursor = c.continuation;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(starts, vec![2, 8]);
        assert!(RawLiteralOverlap::new(&"x".repeat(4097), true).is_err());
    }
    #[test]
    fn search_chunk_literal_nonoverlap_semantics_cross_boundaries() {
        let mut t = TerminalState::new(32, 4);
        t.process_input(b"\x1b]133;A\x07\x1b]133;C;id=raw\x07aaaaaaaa\x1b]133;D;0;id=raw\x07");
        let r = t.command_records().back().unwrap();
        let mut matcher = RawLiteralOverlap::new("aa", true).unwrap();
        let mut cursor = None;
        let mut starts = Vec::new();
        loop {
            let SearchChunkOutcome::Chunk(c) = t.search_output_chunk(
                r.sequence,
                &r.id,
                cursor.as_ref(),
                SearchChunkBudget {
                    bytes: 4,
                    ..Default::default()
                },
            ) else {
                panic!()
            };
            starts.extend(
                matcher
                    .consume(&c)
                    .unwrap()
                    .hits
                    .iter()
                    .map(|h| h.start.anchor.column),
            );
            cursor = c.continuation;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(starts, vec![0, 2, 4, 6]);
    }
    #[test]
    fn search_chunk_literal_rejects_replayed_or_other_pane_chunk() {
        let mut t = TerminalState::new(32, 4);
        t.process_input(b"\x1b]133;A\x07\x1b]133;C;id=raw\x07abcdefgh\x1b]133;D;0;id=raw\x07");
        let r = t.command_records().back().unwrap();
        let SearchChunkOutcome::Chunk(c) = t.search_output_chunk(
            r.sequence,
            &r.id,
            None,
            SearchChunkBudget {
                bytes: 4,
                ..Default::default()
            },
        ) else {
            panic!()
        };
        let mut matcher = RawLiteralOverlap::new("abcdef", true).unwrap();
        matcher.consume(&c).unwrap();
        assert!(matcher.consume(&c).is_err());
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;
    #[test]
    fn search_chunk_complete_regex_lines_remain_available_beside_partial_boundary() {
        let c = RawSearchChunk {
            record_sequence: 1,
            record_id: "r".into(),
            start: BufferAnchor {
                line_id: 1,
                column: 0,
            },
            text: "tail\nwhole\npartial".into(),
            spans: Vec::new(),
            source: SearchSourceVersion {
                epoch: 1,
                revision: 1,
            },
            starts_logical_line: false,
            ends_logical_line: false,
            continuation: None,
            work: SearchChunkWork::default(),
        };
        assert!(!c.supports_complete_line_regex());
        assert_eq!(c.complete_logical_lines(), vec![5..10]);
        let regex = regex::Regex::new("^whole$").unwrap();
        assert!(regex.is_match(&c.text[c.complete_logical_lines()[0].clone()]));
    }
    #[test]
    fn search_chunk_literal_unicode_expansion_keeps_original_cell_identity() {
        let mut t = TerminalState::new(32, 4);
        t.process_input(
            "\x1b]133;A\x07\x1b]133;C;id=raw\x07xİZz\x1b]133;D;0;id=raw\x07".as_bytes(),
        );
        let r = t.command_records().back().unwrap();
        let mut matcher = RawLiteralOverlap::new("i\u{307}z", false).unwrap();
        let mut cursor = None;
        let mut hits = Vec::new();
        loop {
            let SearchChunkOutcome::Chunk(c) = t.search_output_chunk(
                r.sequence,
                &r.id,
                cursor.as_ref(),
                SearchChunkBudget {
                    bytes: 4,
                    ..Default::default()
                },
            ) else {
                panic!()
            };
            hits.extend(matcher.consume(&c).unwrap().hits);
            cursor = c.continuation;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].start.anchor.column, 1);
        assert_eq!(hits[0].end.anchor.column, 2);
    }
    #[test]
    fn search_chunk_literal_hit_cap_is_explicit_partial_result() {
        let mut t = TerminalState::new(1024, 4);
        t.process_input(
            format!(
                "\x1b]133;A\x07\x1b]133;C;id=raw\x07{}\x1b]133;D;0;id=raw\x07",
                "x".repeat(600)
            )
            .as_bytes(),
        );
        let r = t.command_records().back().unwrap();
        let SearchChunkOutcome::Chunk(c) =
            t.search_output_chunk(r.sequence, &r.id, None, SearchChunkBudget::default())
        else {
            panic!()
        };
        let result = RawLiteralOverlap::new("x", true)
            .unwrap()
            .consume(&c)
            .unwrap();
        assert_eq!(result.hits.len(), 500);
        assert!(result.capped);
    }
}

#[cfg(test)]
mod shared_literal_tests {
    use super::*;
    use crate::block_mode::{
        search_blocks_with_options, BlockSearchOptions, CachedBlockSearchRecord,
    };

    fn assert_snapshot_raw(text: &str, query: &str, case_sensitive: bool, expected: bool) {
        let records = [CachedBlockSearchRecord::new(
            "raw",
            None,
            Some(text.to_owned()),
        )];
        let snapshot = search_blocks_with_options(
            &records,
            query,
            BlockSearchOptions {
                case_sensitive,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            !snapshot.hits.is_empty(),
            expected,
            "snapshot {text:?} / {query:?}"
        );
        for bytes in [4, 5, 7, 64] {
            let mut t = TerminalState::new(32, 4);
            t.process_input(
                format!("\x1b]133;A\x07\x1b]133;C;id=raw\x07{text}\x1b]133;D;0;id=raw\x07")
                    .as_bytes(),
            );
            let r = t.command_records().back().unwrap();
            let mut matcher = RawLiteralOverlap::new(query, case_sensitive).unwrap();
            let mut cursor = None;
            let mut hits = Vec::new();
            loop {
                let SearchChunkOutcome::Chunk(c) = t.search_output_chunk(
                    r.sequence,
                    &r.id,
                    cursor.as_ref(),
                    SearchChunkBudget {
                        bytes,
                        ..Default::default()
                    },
                ) else {
                    panic!()
                };
                hits.extend(matcher.consume(&c).unwrap().hits);
                cursor = c.continuation;
                if cursor.is_none() {
                    break;
                }
            }
            assert_eq!(
                !hits.is_empty(),
                expected,
                "raw {text:?} / {query:?}, {bytes} byte chunks"
            );
        }
    }

    #[test]
    fn search_chunk_snapshot_raw_query_sigma_equivalence() {
        for text in ["ΟΣ", "οσ", "ος", "ΣΣ", "ςσ"] {
            let query = if text == "ΣΣ" || text == "ςσ" {
                "σσ"
            } else {
                "ΟΣ"
            };
            assert_snapshot_raw(text, query, false, true);
        }
        assert_snapshot_raw("xxΟΣzz", "οσ", false, true);
        assert_snapshot_raw("xxΟΣ", "ος", false, true);
        assert_snapshot_raw("xxΣΣΣ", "σσσ", false, true);
    }

    #[test]
    fn search_chunk_snapshot_raw_query_expansion_and_negative_controls() {
        assert_snapshot_raw("xİZz", "i\u{307}z", false, true);
        assert_snapshot_raw("ΟΣ", " ΟΣ ", false, true);
        assert_snapshot_raw("Straße", "STRASSE", false, false);
        assert_snapshot_raw("ẞ", "ß", false, true);
        assert_snapshot_raw("ΟΣ", "ος", true, false);
        assert_snapshot_raw("σ", "ς", true, false);
        assert_snapshot_raw("ΟΣ", "ΟΣ", true, true);
    }

    #[test]
    fn search_chunk_shared_normalization_is_boundary_independent() {
        // Contextual lowercase differs before/after a cased scalar through
        // case-ignorable marks. Our explicit sigma equivalence does not.
        for text in ["AΣ", "AΣB", "AΣ\u{301}", "AΣ\u{301}B", "ΣΣ", "İΣ"] {
            let whole = block_search_literal_normalize(text);
            for (boundary, _) in text
                .char_indices()
                .chain(std::iter::once((text.len(), '\0')))
            {
                assert_eq!(
                    whole,
                    block_search_literal_normalize(&text[..boundary])
                        + &block_search_literal_normalize(&text[boundary..])
                );
            }
        }
        assert_eq!(block_search_literal_normalize("Σσςİß"), "σσσi\u{307}ß");
    }

    #[test]
    fn search_chunk_regex_engine_semantics_are_unchanged() {
        let records = [CachedBlockSearchRecord::new("r", None, Some("ΟΣ".into()))];
        let insensitive = search_blocks_with_options(
            &records,
            "ος",
            BlockSearchOptions {
                regex: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(insensitive.hits.len(), 1);
        let sensitive = search_blocks_with_options(
            &records,
            "ος",
            BlockSearchOptions {
                regex: true,
                case_sensitive: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(sensitive.hits.is_empty());
    }
}

#[cfg(test)]
mod seal_tests {
    use super::*;

    fn terminal(lines: usize, history: usize) -> (TerminalState, u64) {
        let mut t = TerminalState::new(96, 4);
        t.set_max_scrollback(history);
        t.process_input(b"\x1b]133;A\x07\x1b]133;C;id=sealed\x07");
        let line = format!("{}\r\n", "x".repeat(95));
        for n in 0..lines {
            if n == lines / 2 {
                t.process_input(format!("SEALED_MIDDLE{}\r\n", "x".repeat(82)).as_bytes());
            } else {
                t.process_input(line.as_bytes());
            }
        }
        t.process_input(b"\x1b]133;D;0;id=sealed\x07");
        let sequence = t.command_records().back().unwrap().sequence;
        // Move the completed command's endpoint out of the mutable grid.
        t.process_input(b"\r\n\r\n\r\n\r\n\r\n\r\n");
        (t, sequence)
    }

    #[test]
    fn search_chunk_seal_completes_100k_history_during_continuous_pty_output() {
        let (mut t, sequence) = terminal(100_000, 120_000);
        let seal = t.seal_search_output(sequence, "sealed").unwrap();
        let mut cursor = None;
        let mut matcher = RawLiteralOverlap::new("sealed_middle", false).unwrap();
        let mut hits = 0;
        let mut calls = 0;
        let mut bytes = 0;
        let started = std::time::Instant::now();
        loop {
            t.process_input(b"live PTY append\r\n");
            let SearchChunkOutcome::Chunk(chunk) =
                t.search_sealed_output_chunk(&seal, cursor.as_ref(), SearchChunkBudget::default())
            else {
                panic!("stable historical seal interrupted")
            };
            assert_eq!(chunk.work.records_examined, 2);
            hits += matcher.consume(&chunk).unwrap().hits.len();
            bytes += chunk.text.len();
            calls += 1;
            cursor = chunk.continuation;
            if cursor.is_none() {
                break;
            }
            assert!(calls < 4_000);
        }
        assert_eq!(hits, 1);
        assert_eq!(bytes, 9_600_000);
        assert!(calls > 3_000);
        println!(
            "sealed history amid continuous PTY: {calls} chunks, {bytes} bytes, {} ms",
            started.elapsed().as_millis()
        );
        assert!(t.validate_search_seal(&seal).is_ok());
    }

    #[test]
    fn search_chunk_seal_rejects_eviction_reset_reflow_clear_alt_and_record_reuse() {
        for operation in 0..7 {
            let (mut t, sequence) = terminal(80, 200);
            let seal = t.seal_search_output(sequence, "sealed").unwrap();
            let SearchChunkOutcome::Chunk(chunk) = t.search_sealed_output_chunk(
                &seal,
                None,
                SearchChunkBudget {
                    rows: 1,
                    ..Default::default()
                },
            ) else {
                panic!()
            };
            match operation {
                0 => t.set_max_scrollback(20),
                1 => t.process_input(b"\x1bc"),
                2 => t.on_resize(80, 4),
                3 => t.process_input(b"\x1b[3J"),
                4 => t.process_input(b"\x1b[?1049h\x1b[?1049l"),
                5 => {
                    t.command_records.clear();
                    t.process_input(
                        b"\x1b]133;A\x07\x1b]133;C;id=sealed\x07new\x1b]133;D;0;id=sealed\x07",
                    );
                }
                _ => {
                    t.search_scrollback_epoch = 0;
                }
            }
            assert!(t.validate_search_seal(&seal).is_err());
            assert!(!matches!(
                t.search_sealed_output_chunk(
                    &seal,
                    chunk.continuation.as_ref(),
                    SearchChunkBudget::default()
                ),
                SearchChunkOutcome::Chunk(_)
            ));
        }
        let (a, sequence) = terminal(80, 200);
        let seal = a.seal_search_output(sequence, "sealed").unwrap();
        let (b, _) = terminal(80, 200);
        assert!(matches!(
            b.validate_search_seal(&seal),
            Err(SearchChunkOutcome::Stale)
        ));
    }

    #[test]
    fn search_chunk_seal_survives_eviction_before_range_only() {
        let mut t = TerminalState::new(32, 4);
        t.set_max_scrollback(300);
        t.process_input("prefix\r\n".repeat(100).as_bytes());
        t.process_input(b"\x1b]133;A\x07\x1b]133;C;id=sealed\x07");
        t.process_input("retained\r\n".repeat(40).as_bytes());
        t.process_input(b"\x1b]133;D;0;id=sealed\x07\r\n\r\n\r\n\r\n\r\n");
        let sequence = t.command_records().back().unwrap().sequence;
        let seal = t.seal_search_output(sequence, "sealed").unwrap();
        t.set_max_scrollback(60);
        assert!(t.validate_search_seal(&seal).is_ok());
        let SearchChunkOutcome::Chunk(chunk) =
            t.search_sealed_output_chunk(&seal, None, SearchChunkBudget::default())
        else {
            panic!()
        };
        assert!(chunk.text.starts_with("retained"));
        t.set_max_scrollback(10);
        assert!(matches!(
            t.validate_search_seal(&seal),
            Err(SearchChunkOutcome::Unavailable)
        ));
    }

    #[test]
    fn search_chunk_seal_rejects_live_grid_and_conservative_cursor() {
        let mut t = TerminalState::new(96, 4);
        t.process_input(b"\x1b]133;A\x07\x1b]133;C;id=sealed\x07live\x1b]133;D;0;id=sealed\x07");
        let sequence = t.command_records().back().unwrap().sequence;
        assert!(matches!(
            t.seal_search_output(sequence, "sealed"),
            Err(SearchChunkOutcome::Unsupported(
                "output still touches mutable grid"
            ))
        ));
        let (mut t, sequence) = terminal(80, 200);
        let seal = t.seal_search_output(sequence, "sealed").unwrap();
        let SearchChunkOutcome::Chunk(raw) = t.search_output_chunk(
            sequence,
            "sealed",
            None,
            SearchChunkBudget {
                rows: 1,
                ..Default::default()
            },
        ) else {
            panic!()
        };
        assert!(matches!(
            t.search_sealed_output_chunk(
                &seal,
                raw.continuation.as_ref(),
                SearchChunkBudget::default()
            ),
            SearchChunkOutcome::Stale
        ));
        // Live-grid edits and no-op resizing do not mutate sealed scrollback.
        t.process_input(b"\x1b[H\x1b[2Kedit");
        t.on_resize(96, 4);
        assert!(t.validate_search_seal(&seal).is_ok());
    }
}
