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
use std::collections::{HashMap, HashSet};

const BLOCK_SEARCH_PAGE_STEP: usize = 10;

/// Cheap identity of the terminal's retained semantic-record deque. This is
/// deliberately independent of output snapshots and scrollback rows: only a
/// real `command_records` insertion/retirement changes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetainedRecordVersion {
    pub len: usize,
    pub oldest_sequence: Option<u64>,
    pub newest_sequence: Option<u64>,
}

/// Pane-local, process-lifetime bookmark truth. PTY-controlled record ids are
/// unsuitable here because their bounded tombstones eventually permit reuse;
/// the terminal-owned sequence is monotonic for the lifetime of one pane.
///
/// Mutation is centralized so revisions advance only when the actual set
/// changes. Empty bookmark sets are removed, while their last revision remains
/// until the session closes so an open picker can observe the transition.
#[derive(Default)]
pub struct BlockBookmarkState {
    by_session: HashMap<String, HashSet<u64>>,
    revisions: HashMap<String, u64>,
    observed_records: HashMap<String, RetainedRecordVersion>,
}

impl BlockBookmarkState {
    pub fn get(&self, session_id: &str) -> Option<&HashSet<u64>> {
        self.by_session.get(session_id)
    }

    pub fn contains(&self, session_id: &str, sequence: u64) -> bool {
        self.by_session
            .get(session_id)
            .is_some_and(|bookmarks| bookmarks.contains(&sequence))
    }

    pub fn revision(&self, session_id: &str) -> u64 {
        self.revisions.get(session_id).copied().unwrap_or(0)
    }

    pub fn session_ids(&self) -> Vec<String> {
        self.by_session.keys().cloned().collect()
    }

    pub fn needs_prune(&self, session_id: &str, version: RetainedRecordVersion) -> bool {
        self.by_session.contains_key(session_id)
            && self.observed_records.get(session_id).copied() != Some(version)
    }

    /// Toggle one live sequence and return its new state. The caller supplies
    /// the deque version observed during live-record validation, preventing a
    /// redundant full retained-record scan on the next static frame.
    pub fn toggle(
        &mut self,
        session_id: &str,
        sequence: u64,
        version: RetainedRecordVersion,
    ) -> bool {
        let bookmarks = self.by_session.entry(session_id.to_string()).or_default();
        let active = if bookmarks.remove(&sequence) {
            false
        } else {
            bookmarks.insert(sequence);
            true
        };
        if bookmarks.is_empty() {
            self.by_session.remove(session_id);
        }
        self.observed_records
            .insert(session_id.to_string(), version);
        self.bump_revision(session_id);
        active
    }

    /// Reconcile bookmarks only after the retained-record deque identity
    /// changes. Snapshot/output eviction never calls this path and therefore
    /// cannot erase a still-live bookmark.
    pub fn retain_live(
        &mut self,
        session_id: &str,
        version: RetainedRecordVersion,
        live_complete_sequences: &HashSet<u64>,
    ) -> bool {
        self.observed_records
            .insert(session_id.to_string(), version);
        let Some(bookmarks) = self.by_session.get_mut(session_id) else {
            return false;
        };
        let previous_len = bookmarks.len();
        bookmarks.retain(|sequence| live_complete_sequences.contains(sequence));
        let changed = bookmarks.len() != previous_len;
        if bookmarks.is_empty() {
            self.by_session.remove(session_id);
        }
        if changed {
            self.bump_revision(session_id);
        }
        changed
    }

    pub fn remove_session(&mut self, session_id: &str) -> bool {
        self.observed_records.remove(session_id);
        self.revisions.remove(session_id);
        self.by_session.remove(session_id).is_some()
    }

    fn bump_revision(&mut self, session_id: &str) {
        let revision = self.revisions.entry(session_id.to_string()).or_default();
        *revision = revision.saturating_add(1);
    }
}

/// Honest empty-state detail for the Bookmarked filter. The picker has a
/// single metadata-filter axis, so there is no additional AND-filter case on
/// this frontend.
pub fn bookmarked_empty_message(
    has_live_bookmarks: bool,
    has_bookmarked_indexed_text: bool,
    scope: BlockSearchScope,
) -> String {
    if !has_live_bookmarks {
        "No bookmarked command blocks in retained history".to_string()
    } else if !has_bookmarked_indexed_text {
        let surface = match scope {
            BlockSearchScope::All => "command or output",
            BlockSearchScope::Command => "command",
            BlockSearchScope::Output => "output",
        };
        format!("Bookmarked blocks have no indexed {surface} text")
    } else {
        "No matches in bookmarked blocks".to_string()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockSearchFilter {
    #[default]
    All,
    Failed,
    Slow,
    Bookmarked,
    Background,
}

/// Result of routing one logical `B` press while the picker is open. The first
/// edge owns that logical-key lifetime: an exact bookmark chord keeps
/// suppressing repeats even if Ctrl/Shift is released before `B`, while an
/// ordinary text press keeps propagating even if modifiers later change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockSearchBookmarkKeyRoute {
    Toggle,
    Suppress,
    Propagate,
}

/// Exact finalized-record set represented by one cache. Length alone is not
/// enough because the bounded deque can evict one old record while adding one
/// new record in the same update.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
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
    /// Previous visual rank, used only when retention removed the exact hit.
    pub index: usize,
}

/// Stable picker-row claim queued by pointer or keyboard input. The visual
/// index is deliberately absent: a refresh may reorder rows before an action
/// is applied, so callers revalidate this identity against the current hit
/// set and finalized-record generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSearchBookmarkTarget {
    pub session_id: String,
    pub record_version: BlockSearchRecordVersion,
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
    /// Set by the previous render pass when one of the matching, scope, filter,
    /// Refresh, Reset, or bookmark controls owns keyboard focus. The input pass
    /// runs before widgets render, so this lets Enter reach the focused button
    /// instead of being preempted by picker-wide result confirmation.
    pub intent_control_focused: bool,
    /// One-shot: center the highlighted result in the virtual result list.
    /// Keyboard/query-driven moves set this; pointer hover deliberately does
    /// not, so wheel and scrollbar movement remain under the user's control.
    pub scroll_to_selected: bool,
    /// One-shot: restore keyboard/AccessKit focus to the selected row's star
    /// after that activation re-filters the virtual result list. Pointer
    /// activation deliberately keeps the established query-refocus behavior.
    pub needs_bookmark_focus: bool,
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
    /// Execution id → terminal-owned sequence, for every completed record of
    /// the cached session. Bookmarks are keyed by sequence while hits and the
    /// cache are keyed by id, so this is the join between them.
    ///
    /// Built with the cache, from the same locked pass over the record deque,
    /// and invalidated with it. The overlay used to rebuild this map from
    /// scratch on every rendered frame — up to `MAX_COMMAND_MARKS` `String`
    /// clones under the terminal mutex, immediately beside the cache that
    /// exists so exactly that work happens once per record change.
    ///
    /// Shared rather than owned so the render pass can hold a snapshot of it
    /// across the picker window's closure, which needs `&mut self`, without
    /// copying the ids back out again.
    pub record_sequences: std::sync::Arc<std::collections::HashMap<String, u64>>,
    /// Session the current `hits` AND `cache` were computed against. A tab
    /// switch while the picker is open invalidates both.
    pub session_id: Option<String>,
    pub record_version: Option<BlockSearchRecordVersion>,
    /// Bookmark-set revision used for the current hits. Unlike record version,
    /// a change here only re-filters the existing bounded cache.
    pub bookmark_revision: Option<u64>,
    /// Query the current `hits` were computed for; `None` forces a recompute
    /// on the next rendered frame.
    pub computed_query: Option<String>,
    /// Logical-key latch for picker-local Ctrl+Shift+B. App code transitions it
    /// only through the tested routing helpers; crate visibility keeps
    /// struct-update syntax available to sibling-module tests.
    pub(crate) bookmark_logical_b_held: bool,
    pub(crate) bookmark_logical_b_claimed: bool,
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
        self.needs_bookmark_focus = false;
        self.intent_control_focused = false;
    }

    /// Invalidate only the source/index version for an explicit F5 refresh.
    /// Query intent and the current hit anchor remain intact. An invalid query
    /// refuses the request so rebuilding cannot discard the last valid result
    /// set merely to rediscover the same expression error.
    pub fn request_manual_refresh(&mut self) -> bool {
        if self.query_error.is_some() {
            return false;
        }
        self.record_version = None;
        true
    }

    /// Open with the last process-lifetime matching intent. Hits, cache,
    /// selection and pane identity are always rebuilt fresh; query/options,
    /// scope and metadata filter are intentionally not serialized anywhere.
    pub fn open(&mut self) {
        self.is_open = true;
        self.needs_focus = true;
        self.needs_bookmark_focus = false;
        self.intent_control_focused = false;
        self.scroll_to_selected = true;
        self.selected_index = 0;
        self.hits.clear();
        self.capped = false;
        self.older_not_indexed = false;
        self.query_error = None;
        self.cache = Vec::new();
        self.record_sequences = std::sync::Arc::default();
        self.session_id = None;
        self.record_version = None;
        self.bookmark_revision = None;
        self.computed_query = None;
        self.reset_bookmark_key_latch();
    }

    pub fn close(&mut self) {
        self.is_open = false;
        self.intent_control_focused = false;
        if self.query.len() > crate::block_mode::BLOCK_SEARCH_QUERY_MAX_BYTES {
            // Do not reopen directly into the one-scalar TooLong sentinel.
            self.query.clear();
        }
        // Retain only matching intent while closed. Results/cache can hold a
        // large slice of scrollback and pane identities are stale on reopen.
        self.cache = Vec::new();
        self.record_sequences = std::sync::Arc::default();
        self.hits = Vec::new();
        self.capped = false;
        self.older_not_indexed = false;
        self.query_error = None;
        self.selected_index = 0;
        self.session_id = None;
        self.record_version = None;
        self.bookmark_revision = None;
        self.computed_query = None;
        self.needs_bookmark_focus = false;
        self.reset_bookmark_key_latch();
    }

    /// Route a logical `B` press. `repeat` is still checked on an unlatched
    /// edge so a stale repeat delivered after focus loss can never toggle.
    pub fn bookmark_logical_b_press(
        &mut self,
        exact_ctrl_shift: bool,
        repeat: bool,
    ) -> BlockSearchBookmarkKeyRoute {
        if self.bookmark_logical_b_held {
            return if self.bookmark_logical_b_claimed {
                BlockSearchBookmarkKeyRoute::Suppress
            } else {
                BlockSearchBookmarkKeyRoute::Propagate
            };
        }
        if repeat {
            return BlockSearchBookmarkKeyRoute::Suppress;
        }
        self.bookmark_logical_b_held = true;
        self.bookmark_logical_b_claimed = exact_ctrl_shift;
        if exact_ctrl_shift {
            BlockSearchBookmarkKeyRoute::Toggle
        } else {
            BlockSearchBookmarkKeyRoute::Propagate
        }
    }

    pub fn release_bookmark_key(&mut self) {
        self.reset_bookmark_key_latch();
    }

    pub fn reset_bookmark_key_latch(&mut self) {
        self.bookmark_logical_b_held = false;
        self.bookmark_logical_b_claimed = false;
    }

    /// Preserve keyboard/AccessKit continuity after a star activation. If a
    /// Bookmarked re-filter removed the final row, fall back to the query
    /// editor instead of leaving focus on a virtual widget that no longer
    /// exists.
    pub fn restore_focus_after_bookmark_activation(&mut self) {
        self.needs_bookmark_focus = !self.hits.is_empty();
        self.needs_focus = self.hits.is_empty();
        if !self.hits.is_empty() {
            // An exact retained anchor deliberately preserves scroll during a
            // normal refresh. Focus restoration is different: the stable star
            // must be rendered by the virtual list before it can take focus,
            // even when newly inserted hits pushed it outside the viewport.
            self.scroll_to_selected = true;
        }
    }

    /// Release every index/result allocation before a version rebuild starts.
    /// Query/filter controls survive so the new index can immediately
    /// reevaluate the user's current intent.
    pub fn release_index_for_rebuild(&mut self) {
        self.cache = Vec::new();
        self.record_sequences = std::sync::Arc::default();
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

    pub fn bookmark_target(&self, index: usize) -> Option<BlockSearchBookmarkTarget> {
        if !self.is_open || self.query_error.is_some() || self.computed_query.is_none() {
            return None;
        }
        let hit = self.hits.get(index)?;
        Some(BlockSearchBookmarkTarget {
            session_id: self.session_id.clone()?,
            record_version: self.record_version?,
            record_id: hit.record_id.clone(),
            line_no: hit.line_no,
            is_output_line: hit.is_output_line,
        })
    }

    pub fn contains_bookmark_target(&self, target: &BlockSearchBookmarkTarget) -> bool {
        self.is_open
            && self.query_error.is_none()
            && self.session_id.as_deref() == Some(target.session_id.as_str())
            && self.record_version == Some(target.record_version)
            && self.hits.iter().any(|hit| {
                hit.record_id == target.record_id
                    && hit.line_no == target.line_no
                    && hit.is_output_line == target.is_output_line
            })
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
            index: self.selected_index,
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
        let same_session_anchor = anchor.filter(|anchor| anchor.session_id == session_id);
        let anchored_index = same_session_anchor.and_then(|anchor| {
            hits.iter().position(|hit| {
                hit.record_id == anchor.record_id
                    && hit.line_no == anchor.line_no
                    && hit.is_output_line == anchor.is_output_line
            })
        });
        self.selected_index = anchored_index
            .or_else(|| {
                same_session_anchor
                    .filter(|_| !hits.is_empty())
                    .map(|anchor| anchor.index.min(hits.len() - 1))
            })
            .unwrap_or(0);
        self.hits = hits;
        self.capped = capped;
        self.query_error = None;
        // A background record-version refresh that retained the exact row
        // must not yank a pointer user away from the scroll position they are
        // inspecting. Preserve any already-pending keyboard request, while a
        // new intent or vanished anchor reveals the rank-preserving fallback.
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
        bookmark_revision: u64,
    ) -> bool {
        self.computed_query.as_deref() != Some(self.query.as_str())
            || self.session_id.as_deref() != Some(active_session_id)
            || self.record_version != Some(record_version)
            || self.bookmark_revision != Some(bookmark_revision)
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
    fn the_record_sequence_join_is_released_with_the_index_it_joins_to() {
        // Bookmarks are keyed by the terminal's monotonic sequence while hits
        // and the cache are keyed by execution id, so the overlay needs this
        // join on every frame it paints. It used to be rebuilt from the live
        // record deque each frame — up to `MAX_COMMAND_MARKS` id clones under
        // the terminal mutex — right beside the cache that exists so exactly
        // that work happens once per record change.
        //
        // Moving it into the cache is only correct while the two are
        // invalidated together, and there are three ways the cache goes away,
        // not one. A join that outlived its cache would resolve a hit's id
        // against a different generation of the record deque and the bookmark
        // star would follow the wrong block; a join that outlived a *closed*
        // picker is the same stale generation held resident for the rest of
        // the process — up to `MAX_COMMAND_MARKS` ids of `MAX_OSC_133_ID_BYTES`
        // each, which is exactly the retention `close` drops the cache to
        // avoid.
        let indexed = || BlockSearchState {
            cache: vec![CachedBlockSearchRecord::new("kept", Some("build"), None)],
            record_sequences: std::sync::Arc::new(std::collections::HashMap::from([(
                "kept".to_string(),
                7u64,
            )])),
            ..Default::default()
        };
        assert_eq!(indexed().record_sequences.get("kept"), Some(&7));

        for (name, release) in [
            (
                "release_index_for_rebuild",
                BlockSearchState::release_index_for_rebuild as fn(&mut BlockSearchState),
            ),
            ("open", BlockSearchState::open as fn(&mut BlockSearchState)),
            (
                "close",
                BlockSearchState::close as fn(&mut BlockSearchState),
            ),
        ] {
            let mut state = indexed();
            release(&mut state);
            assert!(state.cache.is_empty(), "{name} must drop the cache");
            assert!(
                state.record_sequences.is_empty(),
                "{name}: the id-to-sequence join must not survive the cache it belongs to"
            );
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
            intent_control_focused: true,
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
        assert!(!state.intent_control_focused);
        assert_eq!(state.query, "old");
        assert!(state.hits.is_empty() && !state.capped);
        assert!(state.case_sensitive && state.regex && state.whole_word);
        assert_eq!(state.scope, BlockSearchScope::Output);
        assert_eq!(state.filter, BlockSearchFilter::Bookmarked);
        assert_eq!(state.selected_index, 0);
        // session_id is cleared, which is also the cache-rebuild trigger.
        assert_eq!(state.session_id, None);
        assert!(state.needs_refresh("s", BlockSearchRecordVersion::default(), 0));
        // Closing releases the (potentially large) extraction cache.
        state.close();
        assert!(!state.is_open && state.cache.is_empty() && state.hits.is_empty());
        assert!(!state.intent_control_focused);
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
            intent_control_focused: true,
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
        assert!(!state.intent_control_focused);
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

        // The anchored row disappearing preserves the closest old rank.
        state.adopt_hits(
            vec![hit("newest"), hit("oldest")],
            false,
            "pane-a",
            Some(&anchor),
        );
        assert_eq!(state.selected_index, 1);
        assert_eq!(state.selected_hit().unwrap().record_id, "oldest");
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
        assert!(state.needs_refresh("s1", version, 0));
        state.computed_query = Some("q".to_string());
        state.session_id = Some("s1".to_string());
        state.record_version = Some(version);
        state.bookmark_revision = Some(0);
        assert!(!state.needs_refresh("s1", version, 0));
        assert!(state.needs_refresh("s2", version, 0));
        assert!(state.needs_refresh("s1", version, 1));
        assert!(state.needs_refresh(
            "s1",
            BlockSearchRecordVersion {
                // Same-length retention rotation must invalidate even when a
                // count-only probe would claim nothing changed.
                oldest_sequence: Some(2),
                newest_sequence: Some(2),
                ..version
            },
            0,
        ));
        state.query.push('x');
        assert!(state.needs_refresh("s1", version, 0));

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
    fn manual_refresh_invalidates_only_the_cache_version() {
        let version = BlockSearchRecordVersion {
            len: 2,
            oldest_sequence: Some(1),
            newest_sequence: Some(2),
        };
        let mut state = BlockSearchState {
            query: "q".to_string(),
            computed_query: Some("q".to_string()),
            session_id: Some("s1".to_string()),
            record_version: Some(version),
            hits: vec![hit("a"), hit("b")],
            selected_index: 1,
            ..Default::default()
        };
        assert!(state.request_manual_refresh());
        assert_eq!(state.record_version, None);
        assert_eq!(state.computed_query.as_deref(), Some("q"));
        assert_eq!(state.selected_hit().unwrap().record_id, "b");

        state.record_version = Some(version);
        state.query_error = Some("bad regex".to_string());
        assert!(!state.request_manual_refresh());
        assert_eq!(state.record_version, Some(version));
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

    #[test]
    fn bookmark_key_latch_claims_until_logical_b_release() {
        let mut state = BlockSearchState::default();
        assert_eq!(
            state.bookmark_logical_b_press(true, false),
            BlockSearchBookmarkKeyRoute::Toggle
        );
        assert_eq!(
            state.bookmark_logical_b_press(true, true),
            BlockSearchBookmarkKeyRoute::Suppress
        );
        assert_eq!(
            state.bookmark_logical_b_press(false, true),
            BlockSearchBookmarkKeyRoute::Suppress,
            "releasing modifiers before B must not leak a repeat into query text"
        );
        state.release_bookmark_key();
        assert_eq!(
            state.bookmark_logical_b_press(true, false),
            BlockSearchBookmarkKeyRoute::Toggle
        );
        state.reset_bookmark_key_latch();
        assert_eq!(
            state.bookmark_logical_b_press(true, true),
            BlockSearchBookmarkKeyRoute::Suppress,
            "a repeat received after focus loss is never a fresh toggle"
        );
    }

    #[test]
    fn bookmark_key_latch_preserves_shift_b_text_repeats() {
        let mut state = BlockSearchState::default();
        assert_eq!(
            state.bookmark_logical_b_press(false, false),
            BlockSearchBookmarkKeyRoute::Propagate
        );
        assert_eq!(
            state.bookmark_logical_b_press(false, true),
            BlockSearchBookmarkKeyRoute::Propagate
        );
        assert_eq!(
            state.bookmark_logical_b_press(true, true),
            BlockSearchBookmarkKeyRoute::Propagate,
            "modifiers acquired midway through one logical B do not steal text input"
        );
    }

    #[test]
    fn stale_shortcut_bookmark_focus_uses_nearest_live_star_or_query_fallback() {
        let version = BlockSearchRecordVersion {
            len: 3,
            oldest_sequence: Some(1),
            newest_sequence: Some(3),
        };
        let mut state = BlockSearchState {
            is_open: true,
            session_id: Some("pane".to_string()),
            hits: vec![hit("a"), hit("removed"), hit("c")],
            selected_index: 1,
            needs_focus: true,
            record_version: Some(version),
            computed_query: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(
            state.bookmark_logical_b_press(true, false),
            BlockSearchBookmarkKeyRoute::Toggle
        );
        let stale_target = state
            .bookmark_target(state.selected_index)
            .expect("shortcut captures the selected stable target");
        state.release_bookmark_key();

        // Model the shared action's anchor-preserving refresh after the
        // shortcut target retires. The old target cannot be reused, while
        // focus follows the nearest surviving stable row.
        let removed_anchor = state.selected_hit_anchor().expect("focused star anchor");
        state.record_version = None;
        state.adopt_hits(
            vec![hit("a"), hit("c")],
            false,
            "pane",
            Some(&removed_anchor),
        );
        state.record_version = Some(BlockSearchRecordVersion {
            len: 2,
            oldest_sequence: Some(1),
            newest_sequence: Some(3),
        });
        assert!(!state.contains_bookmark_target(&stale_target));
        assert_eq!(state.selected_index, 1);
        assert_eq!(state.selected_hit().unwrap().record_id, "c");
        assert!(
            state.scroll_to_selected,
            "the nearest surviving star must be scrolled into the virtual viewport"
        );
        state.restore_focus_after_bookmark_activation();
        assert!(state.needs_bookmark_focus);
        assert!(!state.needs_focus);

        state.hits.clear();
        state.restore_focus_after_bookmark_activation();
        assert!(!state.needs_bookmark_focus);
        assert!(state.needs_focus);

        state.close();
        assert!(!state.needs_bookmark_focus);
    }

    #[test]
    fn bookmark_focus_restoration_scrolls_a_retained_anchor_after_many_insertions() {
        let mut state = BlockSearchState {
            is_open: true,
            session_id: Some("pane".to_string()),
            hits: vec![hit("retained")],
            ..Default::default()
        };
        let retained_anchor = state.selected_hit_anchor().expect("retained star anchor");
        let mut refreshed_hits = (0..32)
            .map(|index| hit(&format!("new-{index}")))
            .collect::<Vec<_>>();
        refreshed_hits.push(hit("retained"));
        state.adopt_hits(refreshed_hits, false, "pane", Some(&retained_anchor));

        assert_eq!(state.selected_index, 32);
        assert_eq!(state.selected_hit().unwrap().record_id, "retained");
        assert!(
            !state.scroll_to_selected,
            "an ordinary exact-anchor refresh preserves the inspected scroll position"
        );

        state.restore_focus_after_bookmark_activation();
        assert!(state.needs_bookmark_focus);
        assert!(
            state.scroll_to_selected,
            "focus restoration must render the selected virtual star before requesting focus"
        );
    }

    #[test]
    fn bookmark_state_uses_sequences_and_bumps_only_for_real_set_changes() {
        let mut bookmarks = BlockBookmarkState::default();
        let version = RetainedRecordVersion {
            len: 2,
            oldest_sequence: Some(10),
            newest_sequence: Some(11),
        };
        assert!(bookmarks.toggle("pane", 10, version));
        assert!(bookmarks.contains("pane", 10));
        assert!(!bookmarks.contains("pane", 11));
        assert_eq!(bookmarks.revision("pane"), 1);
        assert!(!bookmarks.needs_prune("pane", version));

        let live = HashSet::from([10, 11]);
        assert!(!bookmarks.retain_live("pane", version, &live));
        assert_eq!(bookmarks.revision("pane"), 1);

        // The PTY-visible record id may later be reused, but a new terminal-
        // owned sequence cannot inherit the old bookmark.
        let rotated = RetainedRecordVersion {
            len: 2,
            oldest_sequence: Some(11),
            newest_sequence: Some(12),
        };
        assert!(bookmarks.needs_prune("pane", rotated));
        assert!(bookmarks.retain_live("pane", rotated, &HashSet::from([11, 12])));
        assert!(!bookmarks.contains("pane", 12));
        assert_eq!(bookmarks.revision("pane"), 2);
        assert!(bookmarks.get("pane").is_none(), "empty sets are removed");
    }

    #[test]
    fn bookmark_session_close_clears_truth_revision_and_prune_gate() {
        let mut bookmarks = BlockBookmarkState::default();
        let version = RetainedRecordVersion {
            len: 1,
            oldest_sequence: Some(3),
            newest_sequence: Some(3),
        };
        assert!(bookmarks.toggle("pane", 3, version));
        assert!(bookmarks.remove_session("pane"));
        assert!(bookmarks.get("pane").is_none());
        assert_eq!(bookmarks.revision("pane"), 0);
        assert!(!bookmarks.needs_prune("pane", version));
    }

    #[test]
    fn bookmark_target_is_stable_identity_not_a_queued_visual_index() {
        let version = BlockSearchRecordVersion {
            len: 2,
            oldest_sequence: Some(1),
            newest_sequence: Some(2),
        };
        let mut state = BlockSearchState {
            is_open: true,
            session_id: Some("pane".to_string()),
            record_version: Some(version),
            bookmark_revision: Some(0),
            computed_query: Some(String::new()),
            hits: vec![hit("first"), hit("second")],
            ..Default::default()
        };
        let target = state.bookmark_target(1).expect("current row identity");
        state.hits.swap(0, 1);
        assert!(state.contains_bookmark_target(&target));
        state
            .hits
            .retain(|candidate| candidate.record_id != "second");
        assert!(!state.contains_bookmark_target(&target));
        state.hits.push(hit("second"));
        state.record_version = Some(BlockSearchRecordVersion {
            newest_sequence: Some(3),
            ..version
        });
        assert!(!state.contains_bookmark_target(&target));
    }

    #[test]
    fn bookmarked_empty_state_distinguishes_truth_and_indexed_scope_text() {
        assert_eq!(
            bookmarked_empty_message(false, false, BlockSearchScope::All),
            "No bookmarked command blocks in retained history"
        );
        assert_eq!(
            bookmarked_empty_message(true, false, BlockSearchScope::Output),
            "Bookmarked blocks have no indexed output text"
        );
        assert_eq!(
            bookmarked_empty_message(true, true, BlockSearchScope::Command),
            "No matches in bookmarked blocks"
        );
    }
}
