//! `block:search` — cross-block search picker state.
//!
//! A palette-style overlay (same input field + scrollable result list +
//! Enter/Escape/arrow routing as [`crate::command_palette`]) over every
//! [`CommandRecord`](crate::terminal::CommandRecord) of the ACTIVE session.
//! The pure matching lives in [`crate::block_mode::search_blocks`]; this
//! module owns only the UI state. Record text is extracted ONCE per
//! picker-open into `cache` (rebuilt on a session switch while open); each
//! keystroke then only rescans those cached strings, bounded by
//! `MAX_COMMAND_MARKS` records, the 500-hit cap, and the per-record
//! captured-output caps.

use crate::block_mode::{BlockSearchHit, CachedBlockSearchRecord};

#[derive(Default)]
pub struct BlockSearchState {
    pub is_open: bool,
    pub query: String,
    /// Index into `hits` of the highlighted result row.
    pub selected_index: usize,
    /// One-shot: focus the query field on the frame after opening.
    pub needs_focus: bool,
    pub hits: Vec<BlockSearchHit>,
    /// True when the last run stopped at the hit cap (older blocks were left
    /// unscanned).
    pub capped: bool,
    /// Per-open extraction cache the hits are computed from, newest record
    /// first. Built when `session_id` goes stale (open/tab switch), NOT per
    /// keystroke — accepted staleness: blocks that finish while the picker
    /// is open are not seen until it is reopened or the session switches.
    pub cache: Vec<CachedBlockSearchRecord>,
    /// Session the current `hits` AND `cache` were computed against. A tab
    /// switch while the picker is open invalidates both.
    pub session_id: Option<String>,
    /// Query the current `hits` were computed for; `None` forces a recompute
    /// on the next rendered frame.
    pub computed_query: Option<String>,
}

impl BlockSearchState {
    /// Open with a fresh query (palette precedent). Hits are recomputed by
    /// the renderer on the next frame; clearing `session_id` also forces a
    /// fresh extraction cache for that recompute.
    pub fn open(&mut self) {
        self.is_open = true;
        self.needs_focus = true;
        self.query.clear();
        self.selected_index = 0;
        self.hits.clear();
        self.capped = false;
        self.session_id = None;
        self.computed_query = None;
    }

    pub fn close(&mut self) {
        self.is_open = false;
        // The cache can hold a large slice of scrollback; release it while
        // the picker is closed (reopening rebuilds it anyway).
        self.cache = Vec::new();
    }

    /// Move the highlight down one row, wrapping (palette semantics).
    pub fn select_next(&mut self) {
        if !self.hits.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.hits.len();
        }
    }

    /// Move the highlight up one row, wrapping (palette semantics).
    pub fn select_prev(&mut self) {
        if !self.hits.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.hits.len() - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    /// The highlighted hit, if any.
    pub fn selected_hit(&self) -> Option<&BlockSearchHit> {
        self.hits.get(self.selected_index)
    }

    /// Whether `hits` are stale for (`active_session_id`, current query).
    pub fn needs_refresh(&self, active_session_id: &str) -> bool {
        self.computed_query.as_deref() != Some(self.query.as_str())
            || self.session_id.as_deref() != Some(active_session_id)
    }

    /// Result-count footer: `"N matches"` (`"1 match"`), plus
    /// `" · older blocks not searched"` when the run stopped at the hit cap
    /// — which is what `capped` actually means, not that more matches
    /// necessarily exist.
    pub fn count_label(&self) -> String {
        let count = self.hits.len();
        let noun = if count == 1 { "match" } else { "matches" };
        let mut label = format!("{count} {noun}");
        if self.capped {
            label.push_str(" · older blocks not searched");
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
            line_text: String::new(),
            command_preview: String::new(),
        }
    }

    #[test]
    fn open_resets_query_selection_and_stale_results() {
        let mut state = BlockSearchState {
            query: "old".to_string(),
            selected_index: 3,
            hits: vec![hit("a")],
            capped: true,
            cache: vec![CachedBlockSearchRecord::new("a", Some("ls"), None)],
            session_id: Some("s".to_string()),
            computed_query: Some("old".to_string()),
            ..Default::default()
        };
        state.open();
        assert!(state.is_open && state.needs_focus);
        assert!(state.query.is_empty() && state.hits.is_empty() && !state.capped);
        assert_eq!(state.selected_index, 0);
        // session_id is cleared, which is also the cache-rebuild trigger.
        assert_eq!(state.session_id, None);
        assert!(state.needs_refresh("s"));
        // Closing releases the (potentially large) extraction cache.
        state.close();
        assert!(!state.is_open && state.cache.is_empty());
    }

    #[test]
    fn selection_wraps_in_both_directions_and_survives_empty_hits() {
        let mut state = BlockSearchState::default();
        state.select_next();
        state.select_prev();
        assert_eq!(state.selected_index, 0);
        state.hits = vec![hit("a"), hit("b"), hit("c")];
        state.select_prev();
        assert_eq!(state.selected_index, 2);
        state.select_next();
        assert_eq!(state.selected_index, 0);
        assert_eq!(state.selected_hit().unwrap().record_id, "a");
    }

    #[test]
    fn refresh_tracks_both_query_and_session_and_count_label_reports_cap() {
        let mut state = BlockSearchState {
            query: "q".to_string(),
            ..Default::default()
        };
        assert!(state.needs_refresh("s1"));
        state.computed_query = Some("q".to_string());
        state.session_id = Some("s1".to_string());
        assert!(!state.needs_refresh("s1"));
        assert!(state.needs_refresh("s2"));
        state.query.push('x');
        assert!(state.needs_refresh("s1"));

        assert_eq!(state.count_label(), "0 matches");
        state.hits = vec![hit("a")];
        assert_eq!(state.count_label(), "1 match");
        state.hits = vec![hit("a"), hit("b")];
        state.capped = true;
        // The cap label says what `capped` means — the scan stopped early —
        // not that more matches exist.
        assert_eq!(state.count_label(), "2 matches · older blocks not searched");
    }
}
