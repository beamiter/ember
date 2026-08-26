//! `block:search` — cross-block search picker state.
//!
//! A palette-style overlay (same input field + scrollable result list +
//! Enter/Escape/arrow routing as [`crate::command_palette`]) over every
//! [`CommandRecord`](crate::terminal::CommandRecord) of the ACTIVE session.
//! The pure matching lives in [`crate::block_mode::search_blocks`]; this
//! module owns only the UI state. Record text enters a newest-first 8 MiB
//! source snapshot and a 16 MiB original/lowercase cache. Finalized-record
//! version changes rebuild that cache, while query/filter edits only rescan
//! it. Stable session + record versions prevent old hits from jumping into a
//! replacement pane.

use crate::block_mode::{BlockSearchHit, BlockSearchScope, CachedBlockSearchRecord};

const BLOCK_SEARCH_PAGE_STEP: usize = 10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockSearchFilter {
    #[default]
    All,
    Failed,
    Slow,
    Bookmarked,
    Background,
}

/// Exact finalized-record set represented by one cache. Length alone is not
/// enough because the bounded deque can evict one old record while adding one
/// new record in the same update.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockSearchRecordVersion {
    pub len: usize,
    pub oldest_sequence: Option<u64>,
    pub newest_sequence: Option<u64>,
}

/// Stable identity of the highlighted result row, used to keep the highlight
/// on the SAME hit across a refresh that only gained or lost whole records.
/// `(record_id, line_no, is_output_line)` is unique inside one record: a
/// record contributes at most one command row, and output rows carry their
/// 1-based logical line number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSearchHitAnchor {
    /// The pane the anchored row belongs to. Record ids are only unique inside
    /// one session (`local:{sequence}` restarts at 1 per terminal), so without
    /// this a tab switch could re-bind the highlight to an unrelated block
    /// that merely happens to share an id.
    pub session_id: String,
    pub record_id: String,
    pub line_no: Option<usize>,
    pub is_output_line: bool,
}

#[derive(Default)]
pub struct BlockSearchState {
    pub is_open: bool,
    pub query: String,
    /// Index into `hits` of the highlighted result row.
    pub selected_index: usize,
    /// One-shot: focus the query field on the frame after opening.
    pub needs_focus: bool,
    /// One-shot: center the highlighted result in the virtual result list.
    /// Keyboard/query-driven moves set this; pointer hover deliberately does
    /// not, so wheel and scrollbar movement remain under the user's control.
    pub scroll_to_selected: bool,
    pub hits: Vec<BlockSearchHit>,
    /// True when the last run stopped at the hit cap (older blocks were left
    /// unscanned).
    pub capped: bool,
    pub older_not_indexed: bool,
    pub case_sensitive: bool,
    pub regex: bool,
    pub whole_word: bool,
    /// Command/output surface restriction, applied before the hit cap.
    pub scope: BlockSearchScope,
    pub filter: BlockSearchFilter,
    /// Invalid/oversized expressions preserve the last valid hits and index,
    /// but activation is gated until the query compiles again.
    pub query_error: Option<String>,
    /// Bounded cache, oldest record first.
    pub cache: Vec<CachedBlockSearchRecord>,
    /// Session the current `hits` AND `cache` were computed against. A tab
    /// switch while the picker is open invalidates both.
    pub session_id: Option<String>,
    pub record_version: Option<BlockSearchRecordVersion>,
    /// Query the current `hits` were computed for; `None` forces a recompute
    /// on the next rendered frame.
    pub computed_query: Option<String>,
}

impl BlockSearchState {
    pub fn reset_intent(&mut self) {
        self.query.clear();
        self.case_sensitive = false;
        self.regex = false;
        self.whole_word = false;
        self.scope = BlockSearchScope::default();
        self.filter = BlockSearchFilter::default();
        self.query_error = None;
        self.computed_query = None;
        self.selected_index = 0;
        self.needs_focus = true;
    }

    /// Open with the last process-lifetime matching intent. Hits, cache,
    /// selection and pane identity are always rebuilt fresh; query/options,
    /// scope and metadata filter are intentionally not serialized anywhere.
    pub fn open(&mut self) {
        self.is_open = true;
        self.needs_focus = true;
        self.scroll_to_selected = true;
        self.selected_index = 0;
        self.hits.clear();
        self.capped = false;
        self.older_not_indexed = false;
        self.query_error = None;
        self.cache = Vec::new();
        self.session_id = None;
        self.record_version = None;
        self.computed_query = None;
    }

    pub fn close(&mut self) {
        self.is_open = false;
        if self.query.len() > crate::block_mode::BLOCK_SEARCH_QUERY_MAX_BYTES {
            // Do not reopen directly into the one-scalar TooLong sentinel.
            self.query.clear();
        }
        // Retain only matching intent while closed. Results/cache can hold a
        // large slice of scrollback and pane identities are stale on reopen.
        self.cache = Vec::new();
        self.hits = Vec::new();
        self.capped = false;
        self.older_not_indexed = false;
        self.query_error = None;
        self.selected_index = 0;
        self.session_id = None;
        self.record_version = None;
        self.computed_query = None;
    }

    /// Release every index/result allocation before a version rebuild starts.
    /// Query/filter controls survive so the new index can immediately
    /// reevaluate the user's current intent.
    pub fn release_index_for_rebuild(&mut self) {
        self.cache = Vec::new();
        self.hits = Vec::new();
        self.capped = false;
        self.older_not_indexed = false;
        self.query_error = None;
        self.selected_index = 0;
    }

    /// Move the highlight down one row, wrapping (palette semantics).
    pub fn select_next(&mut self) {
        if self.hits.is_empty() {
            self.selected_index = 0;
            return;
        }
        let current = self.selected_index.min(self.hits.len() - 1);
        self.selected_index = (current + 1) % self.hits.len();
        self.scroll_to_selected = true;
    }

    /// Move the highlight up one row, wrapping (palette semantics).
    pub fn select_prev(&mut self) {
        if self.hits.is_empty() {
            self.selected_index = 0;
            return;
        }
        let current = self.selected_index.min(self.hits.len() - 1);
        self.selected_index = if current == 0 {
            self.hits.len() - 1
        } else {
            current - 1
        };
        self.scroll_to_selected = true;
    }

    pub fn select_first(&mut self) {
        self.selected_index = 0;
        if !self.hits.is_empty() {
            self.scroll_to_selected = true;
        }
    }

    pub fn select_last(&mut self) {
        self.selected_index = self.hits.len().saturating_sub(1);
        if !self.hits.is_empty() {
            self.scroll_to_selected = true;
        }
    }

    pub fn select_page(&mut self, forward: bool) {
        if self.hits.is_empty() {
            self.selected_index = 0;
            return;
        }
        let current = self.selected_index.min(self.hits.len() - 1);
        self.selected_index = if forward {
            current
                .saturating_add(BLOCK_SEARCH_PAGE_STEP)
                .min(self.hits.len() - 1)
        } else {
            current.saturating_sub(BLOCK_SEARCH_PAGE_STEP)
        };
        self.scroll_to_selected = true;
    }

    /// Let pointer hover take ownership only after real pointer movement.
    /// A stationary cursor must not overwrite a keyboard selection on every
    /// rendered frame merely because the virtual list moved underneath it.
    pub fn select_hovered(&mut self, index: usize, pointer_moved: bool) {
        if pointer_moved && index < self.hits.len() {
            self.selected_index = index;
        }
    }

    /// The highlighted hit, if any.
    pub fn selected_hit(&self) -> Option<&BlockSearchHit> {
        self.query_error
            .is_none()
            .then(|| self.hits.get(self.selected_index))
            .flatten()
    }

    /// The highlighted row's stable identity. Captured BEFORE a refresh so it
    /// survives `release_index_for_rebuild`, which empties `hits`. `None` when
    /// no row is highlighted, or when the current hits have no owning session
    /// to anchor against.
    pub fn selected_hit_anchor(&self) -> Option<BlockSearchHitAnchor> {
        let hit = self.hits.get(self.selected_index)?;
        Some(BlockSearchHitAnchor {
            session_id: self.session_id.clone()?,
            record_id: hit.record_id.clone(),
            line_no: hit.line_no,
            is_output_line: hit.is_output_line,
        })
    }

    /// Install a fresh result set for `session_id`. When `anchor` came from
    /// that same pane and its row is still present, the highlight stays on
    /// that row — so a background command finishing while the picker is open
    /// no longer yanks the list back to the top and re-points Enter at a block
    /// the user never chose. Callers pass `None` whenever the query, case,
    /// regex or filter changed: that is a new intent and must start at the
    /// first row. An anchor from a different pane is likewise ignored.
    pub fn adopt_hits(
        &mut self,
        hits: Vec<BlockSearchHit>,
        capped: bool,
        session_id: &str,
        anchor: Option<&BlockSearchHitAnchor>,
    ) {
        let anchored_index = anchor
            .filter(|anchor| anchor.session_id == session_id)
            .and_then(|anchor| {
                hits.iter().position(|hit| {
                    hit.record_id == anchor.record_id
                        && hit.line_no == anchor.line_no
                        && hit.is_output_line == anchor.is_output_line
                })
            });
        self.selected_index = anchored_index.unwrap_or(0);
        self.hits = hits;
        self.capped = capped;
        self.query_error = None;
        // A background record-version refresh that retained the exact row
        // must not yank a pointer user away from the scroll position they are
        // inspecting. Preserve any already-pending keyboard request, while a
        // new intent or vanished anchor deliberately reveals the fallback row.
        if anchored_index.is_none() {
            self.scroll_to_selected = true;
        }
    }

    /// Whether `hits` are stale for the active pane, finalized record set, or
    /// current query.
    pub fn needs_refresh(
        &self,
        active_session_id: &str,
        record_version: BlockSearchRecordVersion,
    ) -> bool {
        self.computed_query.as_deref() != Some(self.query.as_str())
            || self.session_id.as_deref() != Some(active_session_id)
            || self.record_version != Some(record_version)
    }

    /// Result-count footer: `"N matches"` (`"1 match"`), plus
    /// `" · older blocks not searched"` when the run stopped at the hit cap
    /// — which is what `capped` actually means, not that more matches
    /// necessarily exist.
    pub fn count_label(&self) -> String {
        let count = self.hits.len();
        let noun = if count == 1 { "match" } else { "matches" };
        let mut label = if count == 0 {
            format!("{count} {noun}")
        } else {
            format!(
                "{} of {count} {noun}",
                self.selected_index.saturating_add(1).min(count)
            )
        };
        if self.capped {
            label.push_str(" · older blocks not searched");
        }
        if self.older_not_indexed {
            label.push_str(" · older blocks not indexed");
        }
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(record_id: &str) -> BlockSearchHit {
        BlockSearchHit {
            record_id: record_id.to_string(),
            is_output_line: false,
            line_no: None,
            match_span: None,
            line_text: String::new(),
            command_preview: String::new(),
        }
    }

    #[test]
    fn open_restores_matching_intent_but_resets_stale_results() {
        let mut state = BlockSearchState {
            query: "old".to_string(),
            case_sensitive: true,
            regex: true,
            whole_word: true,
            scope: BlockSearchScope::Output,
            filter: BlockSearchFilter::Bookmarked,
            selected_index: 3,
            hits: vec![hit("a")],
            capped: true,
            cache: vec![CachedBlockSearchRecord::new("a", Some("ls"), None)],
            session_id: Some("s".to_string()),
            record_version: Some(BlockSearchRecordVersion {
                len: 1,
                oldest_sequence: Some(1),
                newest_sequence: Some(1),
            }),
            computed_query: Some("old".to_string()),
            ..Default::default()
        };
        state.open();
        assert!(state.is_open && state.needs_focus);
        assert_eq!(state.query, "old");
        assert!(state.hits.is_empty() && !state.capped);
        assert!(state.case_sensitive && state.regex && state.whole_word);
        assert_eq!(state.scope, BlockSearchScope::Output);
        assert_eq!(state.filter, BlockSearchFilter::Bookmarked);
        assert_eq!(state.selected_index, 0);
        // session_id is cleared, which is also the cache-rebuild trigger.
        assert_eq!(state.session_id, None);
        assert!(state.needs_refresh("s", BlockSearchRecordVersion::default()));
        // Closing releases the (potentially large) extraction cache.
        state.close();
        assert!(!state.is_open && state.cache.is_empty() && state.hits.is_empty());
        assert_eq!(state.session_id, None);

        state.query = "x".repeat(crate::block_mode::BLOCK_SEARCH_QUERY_MAX_BYTES + 1);
        state.case_sensitive = true;
        state.close();
        assert!(state.query.is_empty());
        assert!(state.case_sensitive, "only invalid query text is discarded");
    }

    #[test]
    fn reset_returns_every_intent_control_to_default() {
        let mut state = BlockSearchState {
            query: "needle".to_string(),
            case_sensitive: true,
            regex: true,
            whole_word: true,
            scope: BlockSearchScope::Output,
            filter: BlockSearchFilter::Bookmarked,
            query_error: Some("old".to_string()),
            computed_query: Some("needle".to_string()),
            selected_index: 4,
            hits: vec![hit("a")],
            ..Default::default()
        };
        state.reset_intent();
        assert!(state.query.is_empty());
        assert!(!state.case_sensitive && !state.regex && !state.whole_word);
        assert_eq!(state.scope, BlockSearchScope::All);
        assert_eq!(state.filter, BlockSearchFilter::All);
        assert!(state.query_error.is_none() && state.computed_query.is_none());
        assert_eq!(state.selected_index, 0);
        assert!(state.needs_focus);
        assert_eq!(state.hits.len(), 1, "refresh owns result replacement");
    }

    #[test]
    fn selection_wraps_in_both_directions_and_survives_empty_hits() {
        let mut state = BlockSearchState {
            selected_index: usize::MAX,
            ..Default::default()
        };
        state.select_next();
        state.select_prev();
        assert_eq!(state.selected_index, 0);
        state.hits = vec![hit("a"), hit("b"), hit("c")];
        state.select_prev();
        assert_eq!(state.selected_index, 2);
        state.select_next();
        assert_eq!(state.selected_index, 0);
        assert_eq!(state.selected_hit().unwrap().record_id, "a");

        state.hits = (0..25).map(|index| hit(&index.to_string())).collect();
        state.select_page(true);
        assert_eq!(state.selected_index, 10);
        state.select_page(true);
        assert_eq!(state.selected_index, 20);
        state.select_page(true);
        assert_eq!(state.selected_index, 24);
        state.select_page(false);
        assert_eq!(state.selected_index, 14);
        state.select_first();
        assert_eq!(state.selected_index, 0);
        state.select_last();
        assert_eq!(state.selected_index, 24);
        state.selected_index = usize::MAX;
        state.select_prev();
        assert_eq!(state.selected_index, 23);
    }

    #[test]
    fn stationary_hover_never_steals_keyboard_selection_or_requests_scroll() {
        let mut state = BlockSearchState {
            hits: vec![hit("a"), hit("b"), hit("c")],
            ..Default::default()
        };
        state.select_next();
        assert_eq!(state.selected_index, 1);
        assert!(state.scroll_to_selected);

        state.select_hovered(2, false);
        assert_eq!(state.selected_index, 1);

        state.scroll_to_selected = false;
        state.select_hovered(2, true);
        assert_eq!(state.selected_index, 2);
        assert!(!state.scroll_to_selected);
    }

    #[test]
    fn adopt_hits_keeps_the_highlight_on_the_same_row_when_a_record_is_added() {
        let mut state = BlockSearchState {
            hits: vec![hit("newest"), hit("middle"), hit("oldest")],
            selected_index: 1,
            session_id: Some("pane-a".to_string()),
            ..Default::default()
        };
        let anchor = state.selected_hit_anchor().expect("a highlighted row");
        assert_eq!(anchor.record_id, "middle");
        assert_eq!(anchor.session_id, "pane-a");

        // A background command finishes: the newest-first list gains a row.
        state.adopt_hits(
            vec![
                hit("brand-new"),
                hit("newest"),
                hit("middle"),
                hit("oldest"),
            ],
            false,
            "pane-a",
            Some(&anchor),
        );
        assert_eq!(state.selected_index, 2);
        assert_eq!(state.selected_hit().unwrap().record_id, "middle");
        assert!(
            !state.scroll_to_selected,
            "a retained background-refresh anchor preserves pointer scroll"
        );

        // The anchored row disappearing falls back to the first row.
        state.adopt_hits(
            vec![hit("newest"), hit("oldest")],
            false,
            "pane-a",
            Some(&anchor),
        );
        assert_eq!(state.selected_index, 0);
        assert!(state.scroll_to_selected);

        // Record ids repeat across panes (`local:{sequence}` restarts at 1),
        // so an anchor from another pane must never re-bind the highlight.
        state.selected_index = 1;
        state.adopt_hits(
            vec![hit("newest"), hit("middle"), hit("oldest")],
            false,
            "pane-b",
            Some(&anchor),
        );
        assert_eq!(state.selected_index, 0);

        // A new intent (query/case/regex/filter change) passes None and
        // deliberately restarts at the top.
        state.selected_index = 1;
        state.adopt_hits(vec![hit("newest"), hit("oldest")], true, "pane-a", None);
        assert_eq!(state.selected_index, 0);
        assert!(state.capped);

        // Output rows in one record are distinguished by their line number.
        let output_hit = |record: &str, line: usize| {
            let mut value = hit(record);
            value.is_output_line = true;
            value.line_no = Some(line);
            value
        };
        state.hits = vec![output_hit("r", 1), output_hit("r", 9)];
        state.selected_index = 1;
        let line_anchor = state.selected_hit_anchor().expect("a highlighted row");
        state.adopt_hits(
            vec![output_hit("r", 1), output_hit("r", 4), output_hit("r", 9)],
            false,
            "pane-a",
            Some(&line_anchor),
        );
        assert_eq!(state.selected_index, 2);

        // With no owning session there is nothing to anchor against.
        state.session_id = None;
        assert!(state.selected_hit_anchor().is_none());
    }

    #[test]
    fn refresh_tracks_both_query_and_session_and_count_label_reports_cap() {
        let mut state = BlockSearchState {
            query: "q".to_string(),
            ..Default::default()
        };
        let version = BlockSearchRecordVersion {
            len: 1,
            oldest_sequence: Some(1),
            newest_sequence: Some(1),
        };
        assert!(state.needs_refresh("s1", version));
        state.computed_query = Some("q".to_string());
        state.session_id = Some("s1".to_string());
        state.record_version = Some(version);
        assert!(!state.needs_refresh("s1", version));
        assert!(state.needs_refresh("s2", version));
        assert!(state.needs_refresh(
            "s1",
            BlockSearchRecordVersion {
                // Same-length retention rotation must invalidate even when a
                // count-only probe would claim nothing changed.
                oldest_sequence: Some(2),
                newest_sequence: Some(2),
                ..version
            }
        ));
        state.query.push('x');
        assert!(state.needs_refresh("s1", version));

        assert_eq!(state.count_label(), "0 matches");
        state.hits = vec![hit("a")];
        assert_eq!(state.count_label(), "1 of 1 match");
        state.hits = vec![hit("a"), hit("b")];
        assert_eq!(state.count_label(), "1 of 2 matches");
        state.selected_index = 1;
        state.capped = true;
        state.older_not_indexed = true;
        // The cap label says what `capped` means — the scan stopped early —
        // not that more matches exist.
        assert_eq!(
            state.count_label(),
            "2 of 2 matches · older blocks not searched · older blocks not indexed"
        );
    }

    #[test]
    fn invalid_query_gates_activation_without_dropping_valid_hits() {
        let mut state = BlockSearchState::default();
        state.hits.push(hit("a"));
        assert!(state.selected_hit().is_some());
        state.query_error = Some("bad regex".to_string());
        assert!(state.selected_hit().is_none());
        assert_eq!(state.hits.len(), 1);
    }

    #[test]
    fn rebuild_release_drops_vec_capacity_but_preserves_query_controls() {
        let mut state = BlockSearchState {
            query: "needle".to_string(),
            case_sensitive: true,
            regex: true,
            whole_word: true,
            scope: BlockSearchScope::Output,
            filter: BlockSearchFilter::Failed,
            query_error: Some("old".to_string()),
            ..Default::default()
        };
        state.cache.reserve(2048);
        state.hits.reserve(2048);
        assert!(state.cache.capacity() > 0 && state.hits.capacity() > 0);

        state.release_index_for_rebuild();

        assert_eq!(state.cache.capacity(), 0);
        assert_eq!(state.hits.capacity(), 0);
        assert_eq!(state.query, "needle");
        assert!(state.case_sensitive && state.regex && state.whole_word);
        assert_eq!(state.scope, BlockSearchScope::Output);
        assert_eq!(state.filter, BlockSearchFilter::Failed);
        assert!(state.query_error.is_none());
    }
}
