use super::{RawRowId, TerminalCell};
use smallvec::SmallVec;
use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

/// A cell coordinate in the materialized terminal viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayPoint {
    pub row: usize,
    pub column: usize,
}

/// Stable coordinate in one retained physical raw row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawCellAnchor {
    pub row_id: RawRowId,
    pub column: usize,
}

/// Stable half-open boundary in one retained physical terminal row.
/// `col` may equal the physical row width for an end boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawCellBoundary {
    pub row: RawRowId,
    pub col: usize,
}

/// Exact retained output owned by one completed OSC 133 command lifecycle.
/// Synthetic projection rows never use these raw origins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FinishedOutputRange {
    pub zone_id: u64,
    pub start: RawCellBoundary,
    pub end: RawCellBoundary,
}

/// Identity of a synthetic collapse row within one policy revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyntheticRowKey {
    pub zone_id: u64,
    pub policy_revision: u64,
}

/// Provenance class for one projected document row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProjectedRowKind {
    Raw,
    Padding,
    CollapsedSummary {
        key: SyntheticRowKey,
        hidden_range: FinishedOutputRange,
        hidden_display_rows: usize,
    },
}

/// Result of resolving a stable terminal-buffer anchor against the current
/// projected document. Hidden owners are reported without mutating policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectedBufferAnchorLocation {
    /// No effective transform is active; callers may use legacy raw scrolling.
    Identity,
    /// The anchor survives in the projected document and was moved to its top.
    Visible { document_row: usize },
    /// The exact raw cell is owned by an effective collapsed command output.
    Hidden { zone_id: u64 },
    /// The anchor is stale, trimmed, structurally omitted, or otherwise unsafe.
    Unmapped,
}

/// User-owned semantic transforms for primary-screen Block Mode. An empty
/// set is the identity and must retain the P0 viewport fast path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionPolicy {
    revision: u64,
    collapsed: BTreeSet<u64>,
}

impl Default for ProjectionPolicy {
    fn default() -> Self {
        Self {
            revision: 1,
            collapsed: BTreeSet::new(),
        }
    }
}

impl ProjectionPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_identity(&self) -> bool {
        self.collapsed.is_empty()
    }

    pub fn is_collapsed(&self, zone_id: u64) -> bool {
        self.collapsed.contains(&zone_id)
    }

    pub fn collapsed_zone_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.collapsed.iter().copied()
    }

    pub(super) fn ids(&self) -> SmallVec<[u64; 4]> {
        self.collapsed.iter().copied().collect()
    }

    pub fn collapse(&mut self, zone_id: u64) -> bool {
        if self.collapsed.contains(&zone_id) {
            return false;
        }
        let Some(revision) = self.revision.checked_add(1) else {
            return false;
        };
        self.collapsed.insert(zone_id);
        self.revision = revision;
        true
    }

    pub fn expand(&mut self, zone_id: u64) -> bool {
        if !self.collapsed.contains(&zone_id) {
            return false;
        }
        let Some(revision) = self.revision.checked_add(1) else {
            return false;
        };
        self.collapsed.remove(&zone_id);
        self.revision = revision;
        true
    }
}

/// Allocation-free geometry for one retained physical terminal row.
///
/// `active_len` is computed by the row owner using the exact historical
/// reflow trimming rule. The projection planner consumes that cached value and
/// the cached wide-continuation columns without inspecting or decoding cells.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Dormant P1 plan core; materialization lands in a later slice.
pub(super) struct RawRowLayout {
    pub(super) absolute_row: usize,
    pub(super) raw_row: RawRowId,
    pub(super) active_len: usize,
    pub(super) wide_continuations: SmallVec<[usize; 2]>,
    pub(super) wrapped: bool,
}

#[allow(dead_code)] // Dormant P1 plan core; materialization lands in a later slice.
impl RawRowLayout {
    pub(super) fn new(
        absolute_row: usize,
        raw_row: RawRowId,
        active_len: usize,
        wide_continuations: impl IntoIterator<Item = usize>,
        wrapped: bool,
    ) -> Self {
        Self {
            absolute_row,
            raw_row,
            active_len,
            wide_continuations: wide_continuations.into_iter().collect(),
            wrapped,
        }
    }
}

/// Where materialization can read a planned raw slice. This source remains
/// available even when raw-row identity allocation has been exhausted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Dormant P1 plan core; materialization lands in a later slice.
pub(super) struct RawSliceSource {
    pub(super) absolute_row: usize,
    pub(super) col_start: usize,
}

/// Stable provenance for a planned raw slice. Untracked rows deliberately
/// have no origin, while retaining their independent materialization source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Dormant P1 plan core; origin consumers land later.
pub(super) struct RawSliceOrigin {
    pub(super) row: RawRowId,
    pub(super) col_start: usize,
}

/// One contiguous piece of a physical row referenced by a planned display
/// row. The plan carries geometry only and never owns terminal cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Dormant P1 plan core; materialization lands in a later slice.
pub(super) struct RawSlice {
    pub(super) view_col_start: usize,
    pub(super) source: RawSliceSource,
    pub(super) origin: Option<RawSliceOrigin>,
    pub(super) len: usize,
    /// A two-column glyph body retained in a one-column projection. Its
    /// continuation is omitted, so the materializer must render a narrow body.
    pub(super) narrow_wide_body: bool,
}

/// Stable row provenance used when a raw row contributes no cell span, most
/// notably for an entirely blank historical logical line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Dormant P1 plan core; row consumers land in a later slice.
pub(super) struct RawRowSource {
    pub(super) raw_row: RawRowId,
    pub(super) raw_absolute_row: usize,
}

/// Geometry for one row in the complete projected document. Columns not
/// covered by `raw_slices` are structural projection padding and stay
/// deliberately unmapped.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Dormant P1 plan core; viewport slicing lands later.
pub(super) struct ProjectionPlanRow {
    pub(super) raw_slices: SmallVec<[RawSlice; 2]>,
    pub(super) row_source: Option<RawRowSource>,
    pub(super) wrapped: bool,
    pub(super) kind: ProjectedRowKind,
}

/// Snapshot-local placement of one physical raw row in the full plan. A row
/// elided by reflow remains represented with no display bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Dormant P1 plan core; collapse resolution lands later.
pub(super) struct RawRowPlacement {
    pub(super) absolute_row: usize,
    pub(super) raw_row: RawRowId,
    pub(super) first_view_row: Option<usize>,
    pub(super) last_view_row: Option<usize>,
}

/// Cell-free geometry for the complete projected terminal document.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Dormant P1 plan core; cache and viewport wiring land later.
pub(super) struct ProjectionPlan {
    pub(super) cols: usize,
    pub(super) rows: VecDeque<ProjectionPlanRow>,
    pub(super) raw_rows: VecDeque<RawRowPlacement>,
    /// Monotonic source coordinate of `raw_rows.front()`. Current retained
    /// absolute rows are source - base.
    pub(super) raw_absolute_base: usize,
    /// Monotonic display coordinate represented by `rows.front()`.
    pub(super) display_row_base: usize,
    pub(super) history_rows: usize,
    pub(super) raw_slice_count: usize,
    pub(super) policy_revision: u64,
    pub(super) effective_collapsed: BTreeSet<u64>,
    pub(super) resolved_collapses: Vec<ResolvedCollapse>,
    /// Exact prior key and number of document rows appended by the most
    /// recent validated streaming advance. A scrolled-up view can preserve
    /// its top row arithmetically instead of searching the full document.
    pub(super) incremental_from: Option<ProjectionPlanCacheKey>,
    pub(super) incremental_appended_rows: usize,
    /// Non-zero identity of this exact cached plan instance.
    pub(super) plan_revision: u64,
}

/// Exact structural identity of a full-document transformed plan. Cell bytes
/// and the ordinary grid paint revision are deliberately absent: neither can
/// change where raw rows, wrap boundaries, or collapse summaries are placed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProjectionPlanCacheKey {
    pub(super) total_lines_scrolled: u64,
    pub(super) row_identity_revision: u64,
    pub(super) finished_output_revision: u64,
    pub(super) scrollback_len: usize,
    pub(super) rows: usize,
    pub(super) cols: usize,
    pub(super) row_wrapped: SmallVec<[bool; 64]>,
    pub(super) policy_revision: u64,
    pub(super) policy_ids: SmallVec<[u64; 4]>,
    /// Canonical count of full-screen primary-buffer row transfers. Combined
    /// with the row-identity delta, this rejects any batch containing a
    /// noncanonical move. Zero is the uncacheable overflow sentinel.
    pub(super) full_screen_scroll_revision: u64,
}

pub(super) type ProjectionPlanCache = (ProjectionPlanCacheKey, Arc<ProjectionPlan>);

/// Exact identity of one late-materialized transformed viewport. Unlike the
/// plan key, this includes cell paint state and the document slice start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TransformedViewportCacheKey {
    pub(super) plan: ProjectionPlanCacheKey,
    pub(super) projection_revision: u64,
    pub(super) grid_version: u64,
    pub(super) document_start: usize,
    pub(super) viewport_rows: usize,
    pub(super) cursor_row: RawRowId,
    pub(super) cursor_col: usize,
}

pub(super) type TransformedViewportCache = (TransformedViewportCacheKey, ProjectedViewport);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ResolvedCollapse {
    pub(super) range: FinishedOutputRange,
    pub(super) start_absolute: usize,
    pub(super) end_absolute: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HideSegment {
    collapse: usize,
    view_start: usize,
    view_end: usize,
}

/// Per-logical-group geometry reused for a complete cold plan build.
///
/// An ordinary unwrapped scrollback row is a group by itself. Allocating both
/// scratch vectors afresh in every call therefore meant two heap allocations
/// per retained row whenever a transformed projection needed a full rebuild.
#[derive(Default)]
struct GroupScratch {
    logical_sources: Vec<(usize, RawSlice)>,
    logical_wide_continuations: Vec<usize>,
}

#[allow(dead_code)] // Dormant P1 plan core; state wiring lands in a later slice.
impl ProjectionPlan {
    /// Plan the current identity document without materializing any cells.
    ///
    /// Historical rows form soft-wrapped logical groups and are reflowed at
    /// `cols`. Live-grid rows are a separate, hard-boundary suffix: each one
    /// contributes exactly one full-width row even if its wrap bit is set.
    pub(super) fn identity(
        history_layouts: impl IntoIterator<Item = RawRowLayout>,
        grid_layouts: impl IntoIterator<Item = RawRowLayout>,
        cols: usize,
    ) -> Self {
        debug_assert!(cols > 0);
        // Consume layouts once and construct raw placements in that same
        // pass. The old path collected both streams, cloned them back out,
        // then walked both collections a third time to build `raw_rows`.
        let history_layouts = history_layouts.into_iter();
        let grid_layouts = grid_layouts.into_iter();
        let capacity = history_layouts
            .size_hint()
            .0
            .saturating_add(grid_layouts.size_hint().0);
        let mut rows = VecDeque::with_capacity(capacity);
        let mut raw_rows = VecDeque::with_capacity(capacity);
        let mut group = Vec::new();
        let mut scratch = GroupScratch::default();
        let mut history_rows = 0usize;
        let mut raw_absolute_base = None;
        let placement = |layout: &RawRowLayout| RawRowPlacement {
            absolute_row: layout.absolute_row,
            raw_row: layout.raw_row,
            first_view_row: None,
            last_view_row: None,
        };

        for layout in history_layouts {
            history_rows = history_rows.saturating_add(1);
            raw_absolute_base.get_or_insert(layout.absolute_row);
            raw_rows.push_back(placement(&layout));
            let wrapped = layout.wrapped;
            group.push(layout);
            if !wrapped {
                Self::append_identity_group(&mut rows, &group, cols, &mut scratch);
                group.clear();
            }
        }
        if !group.is_empty() {
            Self::append_identity_group(&mut rows, &group, cols, &mut scratch);
        }

        // Never join the live grid onto a trailing wrapped history group, or
        // one grid row onto the next. The grid already has display geometry.
        for layout in grid_layouts {
            raw_absolute_base.get_or_insert(layout.absolute_row);
            raw_rows.push_back(placement(&layout));
            Self::append_grid_row(&mut rows, layout, cols);
        }

        let raw_slice_count = rows.iter().map(|row| row.raw_slices.len()).sum();
        let mut plan = Self {
            cols,
            rows,
            raw_rows,
            raw_absolute_base: raw_absolute_base.unwrap_or(0),
            display_row_base: 0,
            history_rows,
            raw_slice_count,
            policy_revision: 0,
            effective_collapsed: BTreeSet::new(),
            resolved_collapses: Vec::new(),
            incremental_from: None,
            incremental_appended_rows: 0,
            plan_revision: 0,
        };
        plan.rebuild_raw_row_placements();
        plan
    }

    /// Advance a transformed plan across one or more ordinary full-screen
    /// scrolls. The retained history prefix is unchanged except for capped
    /// front eviction; the old live grid becomes the appended history tail,
    /// and the current live grid replaces only the hard-boundary suffix.
    /// Callers validate the exact raw-id sequence before entering this path.
    pub(super) fn advance_full_screen_scroll(
        &mut self,
        evicted_raw_rows: usize,
        appended_history: impl IntoIterator<Item = RawRowLayout>,
        grid_layouts: impl IntoIterator<Item = RawRowLayout>,
    ) -> bool {
        let old_grid_rows = self.raw_rows.len().saturating_sub(self.history_rows);
        let appended_history: Vec<_> = appended_history.into_iter().collect();
        let grid_layouts: Vec<_> = grid_layouts.into_iter().collect();
        if old_grid_rows == 0
            || self.rows.len() < old_grid_rows
            || appended_history.iter().any(|layout| layout.wrapped)
        {
            return false;
        }
        for _ in 0..old_grid_rows {
            let Some(row) = self.rows.pop_back() else {
                return false;
            };
            self.raw_slice_count = self.raw_slice_count.saturating_sub(row.raw_slices.len());
            self.raw_rows.pop_back();
        }
        let mut scratch = GroupScratch::default();
        for layout in appended_history.iter().cloned() {
            let first_view_row = self.display_row_base.saturating_add(self.rows.len());
            Self::append_identity_group(
                &mut self.rows,
                std::slice::from_ref(&layout),
                self.cols,
                &mut scratch,
            );
            let last_view_row = self
                .display_row_base
                .saturating_add(self.rows.len().saturating_sub(1));
            self.raw_rows.push_back(RawRowPlacement {
                absolute_row: layout.absolute_row,
                raw_row: layout.raw_row,
                first_view_row: Some(first_view_row),
                last_view_row: Some(last_view_row),
            });
        }
        for layout in grid_layouts.iter().cloned() {
            let view_row = self.display_row_base.saturating_add(self.rows.len());
            Self::append_grid_row(&mut self.rows, layout.clone(), self.cols);
            self.raw_rows.push_back(RawRowPlacement {
                absolute_row: layout.absolute_row,
                raw_row: layout.raw_row,
                first_view_row: Some(view_row),
                last_view_row: Some(view_row),
            });
        }

        for _ in 0..evicted_raw_rows {
            let Some(placement) = self.raw_rows.pop_front() else {
                return false;
            };
            let display_end = placement.last_view_row.or(placement.first_view_row);
            if let Some(display_end) = display_end {
                while self.display_row_base <= display_end {
                    let Some(row) = self.rows.pop_front() else {
                        return false;
                    };
                    self.raw_slice_count =
                        self.raw_slice_count.saturating_sub(row.raw_slices.len());
                    self.display_row_base = self.display_row_base.saturating_add(1);
                }
            }
            self.raw_absolute_base = self.raw_absolute_base.saturating_add(1);
        }
        self.history_rows = self
            .history_rows
            .saturating_add(appended_history.len())
            .saturating_sub(evicted_raw_rows);
        self.raw_slice_count = self.raw_slice_count.saturating_add(
            self.rows
                .iter()
                .rev()
                .take(appended_history.len().saturating_add(grid_layouts.len()))
                .map(|row| row.raw_slices.len())
                .sum::<usize>(),
        );
        true
    }

    pub(super) fn front_rows_are_independently_evictable(&self, count: usize) -> bool {
        self.raw_rows.iter().take(count).all(|placement| {
            let Some(first) = placement.first_view_row else {
                return false;
            };
            if placement.last_view_row != Some(first) {
                return false;
            }
            let Some(row) = first
                .checked_sub(self.display_row_base)
                .and_then(|row| self.rows.get(row))
            else {
                return false;
            };
            matches!(row.kind, ProjectedRowKind::Raw)
                && row
                    .raw_slices
                    .iter()
                    .all(|slice| slice.source.absolute_row == placement.absolute_row)
                && row
                    .row_source
                    .is_none_or(|source| source.raw_absolute_row == placement.absolute_row)
        })
    }

    fn append_grid_row(
        output: &mut VecDeque<ProjectionPlanRow>,
        layout: RawRowLayout,
        cols: usize,
    ) {
        let row_source = layout.raw_row.is_tracked().then_some(RawRowSource {
            raw_row: layout.raw_row,
            raw_absolute_row: layout.absolute_row,
        });
        let raw_slices = std::iter::once(RawSlice {
            view_col_start: 0,
            source: RawSliceSource {
                absolute_row: layout.absolute_row,
                col_start: 0,
            },
            origin: layout.raw_row.is_tracked().then_some(RawSliceOrigin {
                row: layout.raw_row,
                col_start: 0,
            }),
            // Grid rows contain real cells across their physical width. This
            // differs from padding introduced after historical reflow.
            len: cols,
            narrow_wide_body: false,
        })
        .collect();
        output.push_back(ProjectionPlanRow {
            raw_slices,
            row_source,
            wrapped: layout.wrapped,
            kind: ProjectedRowKind::Raw,
        });
    }

    fn append_identity_group(
        output: &mut VecDeque<ProjectionPlanRow>,
        group: &[RawRowLayout],
        cols: usize,
        scratch: &mut GroupScratch,
    ) {
        let group_first_source = group.first().and_then(|layout| {
            layout.raw_row.is_tracked().then_some(RawRowSource {
                raw_row: layout.raw_row,
                raw_absolute_row: layout.absolute_row,
            })
        });
        let logical_len = group
            .iter()
            .fold(0usize, |len, layout| len.saturating_add(layout.active_len));
        if logical_len == 0 {
            output.push_back(ProjectionPlanRow {
                raw_slices: SmallVec::new(),
                row_source: group_first_source,
                wrapped: false,
                kind: ProjectedRowKind::Raw,
            });
            return;
        }

        // A two-column cell cannot fit at width one. Match the terminal's
        // narrow-body behavior by keeping the lead, omitting its continuation,
        // and recording the loss of the second display column explicitly.
        if cols == 1 {
            let group_start = output.len();
            for layout in group {
                let mut continuations = layout.wide_continuations.iter().copied().peekable();
                for raw_col in 0..layout.active_len {
                    if continuations.peek().copied() == Some(raw_col) {
                        continuations.next();
                        continue;
                    }
                    let raw_slice = RawSlice {
                        view_col_start: 0,
                        source: RawSliceSource {
                            absolute_row: layout.absolute_row,
                            col_start: raw_col,
                        },
                        origin: layout.raw_row.is_tracked().then_some(RawSliceOrigin {
                            row: layout.raw_row,
                            col_start: raw_col,
                        }),
                        len: 1,
                        narrow_wide_body: continuations.peek().copied()
                            == Some(raw_col.saturating_add(1)),
                    };
                    output.push_back(ProjectionPlanRow {
                        raw_slices: std::iter::once(raw_slice).collect(),
                        row_source: raw_slice.origin.map(|origin| RawRowSource {
                            raw_row: origin.row,
                            raw_absolute_row: raw_slice.source.absolute_row,
                        }),
                        wrapped: true,
                        kind: ProjectedRowKind::Raw,
                    });
                }
            }
            if output.len() > group_start {
                let last = output.back_mut().expect("non-empty projection group");
                last.wrapped = false;
            }
            return;
        }

        // These temporary indices are linear in physical rows and wide cells.
        // Advancing monotonic cursors below keeps planning O(H + P + S), where
        // H is raw history, P planned rows, and S emitted raw slices.
        let GroupScratch {
            logical_sources,
            logical_wide_continuations,
        } = scratch;
        logical_sources.clear();
        logical_wide_continuations.clear();
        let mut source_start = 0usize;
        for layout in group {
            logical_wide_continuations.extend(
                layout
                    .wide_continuations
                    .iter()
                    .map(|raw_col| source_start.saturating_add(*raw_col)),
            );
            if layout.active_len > 0 {
                logical_sources.push((
                    source_start,
                    RawSlice {
                        view_col_start: 0,
                        source: RawSliceSource {
                            absolute_row: layout.absolute_row,
                            col_start: 0,
                        },
                        origin: layout.raw_row.is_tracked().then_some(RawSliceOrigin {
                            row: layout.raw_row,
                            col_start: 0,
                        }),
                        len: layout.active_len,
                        narrow_wide_body: false,
                    },
                ));
            }
            source_start = source_start.saturating_add(layout.active_len);
        }

        let mut logical_offset = 0usize;
        let mut source_cursor = 0usize;
        let mut wide_cursor = 0usize;
        while logical_offset < logical_len {
            let mut end = logical_offset.saturating_add(cols).min(logical_len);
            while logical_wide_continuations
                .get(wide_cursor)
                .is_some_and(|position| *position < end)
            {
                wide_cursor += 1;
            }
            if end < logical_len
                && logical_wide_continuations.get(wide_cursor).copied() == Some(end)
            {
                end -= 1;
            }

            while let Some((start, slice)) = logical_sources.get(source_cursor) {
                if start.saturating_add(slice.len) > logical_offset {
                    break;
                }
                source_cursor += 1;
            }
            let mut raw_slices: SmallVec<[RawSlice; 2]> = SmallVec::new();
            for (start, slice) in logical_sources[source_cursor..].iter().copied() {
                if start >= end {
                    break;
                }
                let source_end = start.saturating_add(slice.len);
                let overlap_start = start.max(logical_offset);
                let overlap_end = source_end.min(end);
                if overlap_start < overlap_end {
                    raw_slices.push(RawSlice {
                        view_col_start: overlap_start - logical_offset,
                        source: RawSliceSource {
                            absolute_row: slice.source.absolute_row,
                            col_start: slice.source.col_start + overlap_start - start,
                        },
                        origin: slice.origin.map(|origin| RawSliceOrigin {
                            row: origin.row,
                            col_start: origin.col_start + overlap_start - start,
                        }),
                        len: overlap_end - overlap_start,
                        narrow_wide_body: false,
                    });
                }
            }
            let row_source = raw_slices.iter().find_map(|slice| {
                slice.origin.map(|origin| RawRowSource {
                    raw_row: origin.row,
                    raw_absolute_row: slice.source.absolute_row,
                })
            });
            output.push_back(ProjectionPlanRow {
                raw_slices,
                row_source,
                wrapped: end < logical_len,
                kind: ProjectedRowKind::Raw,
            });
            logical_offset = end;
        }
    }

    fn rebuild_raw_row_placements(&mut self) {
        for placement in &mut self.raw_rows {
            placement.first_view_row = None;
            placement.last_view_row = None;
        }
        for (view_row, row) in self.rows.iter().enumerate() {
            let view_row = self.display_row_base.saturating_add(view_row);
            for absolute_row in row
                .raw_slices
                .iter()
                .map(|slice| slice.source.absolute_row)
                .chain(
                    row.row_source
                        .into_iter()
                        .map(|source| source.raw_absolute_row),
                )
            {
                // Full-document absolute rows are contiguous: history starts
                // at zero and the live grid begins exactly at history.len().
                let Some(raw_index) = absolute_row.checked_sub(self.raw_absolute_base) else {
                    continue;
                };
                let Some(placement) = self.raw_rows.get_mut(raw_index) else {
                    continue;
                };
                if placement.absolute_row != absolute_row {
                    continue;
                }
                placement.first_view_row = Some(
                    placement
                        .first_view_row
                        .map_or(view_row, |first| first.min(view_row)),
                );
                placement.last_view_row = Some(
                    placement
                        .last_view_row
                        .map_or(view_row, |last| last.max(view_row)),
                );
            }
        }
    }

    fn clipped_slices_linear(
        row: &ProjectionPlanRow,
        view_start: usize,
        view_end: usize,
        slice_cursor: &mut usize,
    ) -> SmallVec<[RawSlice; 2]> {
        while row
            .raw_slices
            .get(*slice_cursor)
            .is_some_and(|slice| slice.view_col_start.saturating_add(slice.len) <= view_start)
        {
            *slice_cursor += 1;
        }
        let mut clipped = SmallVec::new();
        let mut index = *slice_cursor;
        while let Some(slice) = row.raw_slices.get(index).copied() {
            let slice_start = slice.view_col_start;
            if slice_start >= view_end {
                break;
            }
            let slice_end = slice_start.saturating_add(slice.len);
            let overlap_start = slice_start.max(view_start);
            let overlap_end = slice_end.min(view_end);
            if overlap_start < overlap_end {
                let delta = overlap_start - slice_start;
                clipped.push(RawSlice {
                    view_col_start: overlap_start,
                    source: RawSliceSource {
                        absolute_row: slice.source.absolute_row,
                        col_start: slice.source.col_start + delta,
                    },
                    origin: slice.origin.map(|origin| RawSliceOrigin {
                        row: origin.row,
                        col_start: origin.col_start + delta,
                    }),
                    len: overlap_end - overlap_start,
                    narrow_wide_body: slice.narrow_wide_body,
                });
            }
            if slice_end <= view_end {
                index += 1;
                *slice_cursor = index;
            } else {
                // The same source slice continues after a hidden interval.
                // Leave the cursor here; the next interval clips its suffix in
                // O(1) rather than rescanning every earlier slice.
                *slice_cursor = index;
                break;
            }
        }
        clipped
    }

    fn push_raw_fragment(
        output: &mut VecDeque<ProjectionPlanRow>,
        slices: SmallVec<[RawSlice; 2]>,
        wrapped: bool,
    ) {
        if slices.is_empty() {
            return;
        }
        let row_source = slices.iter().find_map(|slice| {
            slice.origin.map(|origin| RawRowSource {
                raw_row: origin.row,
                raw_absolute_row: slice.source.absolute_row,
            })
        });
        output.push_back(ProjectionPlanRow {
            raw_slices: slices,
            row_source,
            wrapped,
            kind: ProjectedRowKind::Raw,
        });
    }

    /// Splice already validated, non-overlapping raw ranges into the complete
    /// identity document without decoding cells. Input order is irrelevant:
    /// ranges are sorted by raw document position before the linear sweep.
    /// Surviving slices retain their columns; summaries are hard, origin-free.
    pub(super) fn splice_collapses(
        mut self,
        collapses: &[ResolvedCollapse],
        policy_revision: u64,
    ) -> Self {
        if collapses.is_empty() {
            self.policy_revision = policy_revision;
            return self;
        }
        let mut collapses = collapses.to_vec();
        collapses.sort_unstable_by_key(|collapse| {
            (
                collapse.start_absolute,
                collapse.range.start.col,
                collapse.end_absolute,
                collapse.range.end.col,
                collapse.range.zone_id,
            )
        });
        let collapses = collapses.as_slice();

        let mut owners_by_raw: Vec<SmallVec<[usize; 2]>> =
            (0..self.raw_rows.len()).map(|_| SmallVec::new()).collect();
        for (index, collapse) in collapses.iter().enumerate() {
            for absolute_row in collapse.start_absolute..=collapse.end_absolute {
                if let Some(owners) = absolute_row
                    .checked_sub(self.raw_absolute_base)
                    .and_then(|raw_index| owners_by_raw.get_mut(raw_index))
                {
                    owners.push(index);
                }
            }
        }
        let mut owner_cursors = vec![0usize; self.raw_rows.len()];
        let mut row_segments: Vec<SmallVec<[HideSegment; 2]>> = Vec::with_capacity(self.rows.len());
        let mut hidden_display_rows = vec![0usize; collapses.len()];

        for row in &self.rows {
            let mut segments: SmallVec<[HideSegment; 2]> = SmallVec::new();
            for slice in &row.raw_slices {
                let Some(raw_index) = slice
                    .source
                    .absolute_row
                    .checked_sub(self.raw_absolute_base)
                else {
                    continue;
                };
                let Some(owners) = owners_by_raw.get(raw_index) else {
                    continue;
                };
                let slice_start = slice.source.col_start;
                let slice_end = slice_start.saturating_add(slice.len);
                let cursor = &mut owner_cursors[raw_index];
                while let Some(collapse_index) = owners.get(*cursor).copied() {
                    let collapse = collapses[collapse_index];
                    let raw_end = if slice.source.absolute_row == collapse.end_absolute {
                        collapse.range.end.col
                    } else {
                        usize::MAX
                    };
                    if raw_end > slice_start {
                        break;
                    }
                    *cursor += 1;
                }
                let mut owner_index = *cursor;
                while let Some(collapse_index) = owners.get(owner_index).copied() {
                    let collapse = collapses[collapse_index];
                    let raw_start = if slice.source.absolute_row == collapse.start_absolute {
                        collapse.range.start.col
                    } else {
                        0
                    };
                    if raw_start >= slice_end {
                        break;
                    }
                    let raw_end = if slice.source.absolute_row == collapse.end_absolute {
                        collapse.range.end.col
                    } else {
                        usize::MAX
                    };
                    let overlap_start = slice_start.max(raw_start);
                    let overlap_end = slice_end.min(raw_end);
                    if overlap_start < overlap_end {
                        segments.push(HideSegment {
                            collapse: collapse_index,
                            view_start: slice.view_col_start + overlap_start - slice_start,
                            view_end: slice.view_col_start + overlap_end - slice_start,
                        });
                    }
                    if raw_end <= slice_end {
                        owner_index += 1;
                        *cursor = owner_index;
                    } else {
                        break;
                    }
                }
            }

            // A real empty history row has row provenance even though it has
            // no cell slice. It still contributes one hidden display row.
            if segments.is_empty() && row.raw_slices.is_empty() {
                if let Some(source) = row.row_source {
                    if let Some(owners) = source
                        .raw_absolute_row
                        .checked_sub(self.raw_absolute_base)
                        .and_then(|raw_index| owners_by_raw.get(raw_index))
                    {
                        for collapse in owners.iter().copied() {
                            segments.push(HideSegment {
                                collapse,
                                view_start: 0,
                                view_end: self.cols,
                            });
                        }
                    }
                }
            }

            let mut merged: SmallVec<[HideSegment; 2]> = SmallVec::new();
            for segment in segments {
                debug_assert!(merged.last().is_none_or(|previous| {
                    (previous.view_start, previous.collapse)
                        <= (segment.view_start, segment.collapse)
                }));
                if let Some(previous) = merged.last_mut() {
                    if previous.collapse == segment.collapse
                        && segment.view_start <= previous.view_end
                    {
                        previous.view_end = previous.view_end.max(segment.view_end);
                        continue;
                    }
                }
                merged.push(segment);
            }
            let mut last_counted = None;
            for collapse in merged.iter().map(|segment| segment.collapse) {
                if last_counted != Some(collapse) {
                    hidden_display_rows[collapse] = hidden_display_rows[collapse].saturating_add(1);
                    last_counted = Some(collapse);
                }
            }
            row_segments.push(merged);
        }

        let effective: Vec<bool> = hidden_display_rows
            .iter()
            .map(|hidden| *hidden > 0)
            .collect();
        let mut summary_emitted = vec![false; collapses.len()];
        let mut output = VecDeque::with_capacity(
            self.rows
                .len()
                .saturating_add(effective.iter().filter(|value| **value).count()),
        );
        for (row, segments) in self.rows.iter().zip(row_segments) {
            let segments: SmallVec<[HideSegment; 2]> = segments
                .into_iter()
                .filter(|segment| effective[segment.collapse])
                .collect();
            if segments.is_empty() {
                output.push_back(row.clone());
                continue;
            }
            let mut cursor = 0usize;
            let mut slice_cursor = 0usize;
            for segment in segments {
                Self::push_raw_fragment(
                    &mut output,
                    Self::clipped_slices_linear(row, cursor, segment.view_start, &mut slice_cursor),
                    false,
                );
                if !summary_emitted[segment.collapse] {
                    let collapse = collapses[segment.collapse];
                    output.push_back(ProjectionPlanRow {
                        raw_slices: SmallVec::new(),
                        row_source: None,
                        wrapped: false,
                        kind: ProjectedRowKind::CollapsedSummary {
                            key: SyntheticRowKey {
                                zone_id: collapse.range.zone_id,
                                policy_revision,
                            },
                            hidden_range: collapse.range,
                            hidden_display_rows: hidden_display_rows[segment.collapse],
                        },
                    });
                    summary_emitted[segment.collapse] = true;
                }
                cursor = cursor.max(segment.view_end);
            }
            Self::push_raw_fragment(
                &mut output,
                Self::clipped_slices_linear(row, cursor, self.cols, &mut slice_cursor),
                row.wrapped,
            );
        }

        self.rows = output;
        self.raw_slice_count = self.rows.iter().map(|row| row.raw_slices.len()).sum();
        self.policy_revision = policy_revision;
        self.effective_collapsed = collapses
            .iter()
            .enumerate()
            .filter_map(|(index, collapse)| effective[index].then_some(collapse.range.zone_id))
            .collect();
        self.resolved_collapses = collapses
            .iter()
            .enumerate()
            .filter_map(|(index, collapse)| effective[index].then_some(*collapse))
            .collect();
        self.rebuild_raw_row_placements();
        self
    }

    fn raw_absolute_row(&self, row: RawRowId) -> Option<usize> {
        row.is_tracked().then_some(())?;
        self.raw_rows
            .iter()
            .find(|placement| placement.raw_row == row)
            .map(|placement| placement.absolute_row)
    }

    fn summary_row(&self, zone_id: u64) -> Option<usize> {
        self.rows.iter().position(|row| {
            matches!(
                row.kind,
                ProjectedRowKind::CollapsedSummary { key, .. } if key.zone_id == zone_id
            )
        })
    }

    pub(super) fn raw_cell_document_row(&self, anchor: RawCellAnchor) -> Option<usize> {
        self.rows.iter().position(|row| {
            row.raw_slices.iter().any(|slice| {
                slice.origin.is_some_and(|origin| {
                    origin.row == anchor.row_id
                        && anchor.column >= origin.col_start
                        && anchor.column < origin.col_start.saturating_add(slice.len)
                })
            })
        })
    }

    /// Resolve one stable selection identity into this exact plan.
    /// Synthetic summaries deliberately have no fallback: placing a selection
    /// endpoint on one would either copy nothing or silently change its range.
    pub(super) fn selection_point_for_anchor(
        &self,
        anchor: super::ProjectedSelectionAnchor,
    ) -> Option<(usize, usize)> {
        match anchor {
            super::ProjectedSelectionAnchor::Cell(anchor) => {
                for (row_index, row) in self.rows.iter().enumerate() {
                    for slice in &row.raw_slices {
                        let Some(origin) = slice.origin else {
                            continue;
                        };
                        if origin.row == anchor.row_id
                            && anchor.column >= origin.col_start
                            && anchor.column < origin.col_start.saturating_add(slice.len)
                        {
                            return Some((
                                row_index,
                                slice
                                    .view_col_start
                                    .saturating_add(anchor.column.saturating_sub(origin.col_start)),
                            ));
                        }
                    }
                }

                // A live-grid row carries origins through its trailing blank
                // cells. History compression retains only the row's active
                // text, so one of those exact cell identities can disappear
                // when the row scrolls out of the grid. Falling back to the
                // row would keep a visible selection whose copied byte changed
                // from a space to nothing; exact cell loss must fail closed.
                None
            }
            super::ProjectedSelectionAnchor::Row { row, column } => {
                let document_row = self.raw_row_document_row(row)?;
                self.row_is_raw(document_row)
                    .then(|| (document_row, column.min(self.cols.saturating_sub(1))))
            }
        }
    }

    fn row_is_raw(&self, document_row: usize) -> bool {
        self.rows
            .get(document_row)
            .is_some_and(|row| matches!(row.kind, ProjectedRowKind::Raw))
    }

    pub(super) fn raw_row_document_row(&self, row: RawRowId) -> Option<usize> {
        self.raw_rows
            .iter()
            .find(|placement| placement.raw_row == row)
            .and_then(|placement| placement.first_view_row)
            .and_then(|row| row.checked_sub(self.display_row_base))
    }

    pub(super) fn summary_owning_raw_cell(&self, anchor: RawCellAnchor) -> Option<usize> {
        let absolute = self.raw_absolute_row(anchor.row_id)?;
        let collapse = self.resolved_collapses.iter().find(|collapse| {
            if absolute < collapse.start_absolute || absolute > collapse.end_absolute {
                return false;
            }
            let after_start =
                absolute > collapse.start_absolute || anchor.column >= collapse.range.start.col;
            let before_end =
                absolute < collapse.end_absolute || anchor.column < collapse.range.end.col;
            after_start && before_end
        })?;
        self.summary_row(collapse.range.zone_id)
    }

    /// Map the previous viewport's stable top row into this plan. A summary
    /// that was expanded falls back to its first retained raw cell/row.
    pub(super) fn document_row_for_anchor(&self, anchor: ProjectedTopAnchor) -> Option<usize> {
        match anchor {
            ProjectedTopAnchor::RawCell(anchor) => self
                .raw_cell_document_row(anchor)
                .or_else(|| self.summary_owning_raw_cell(anchor)),
            ProjectedTopAnchor::RawRow(row) => self.raw_row_document_row(row).or_else(|| {
                let absolute = self.raw_absolute_row(row)?;
                let collapse = self.resolved_collapses.iter().find(|collapse| {
                    absolute >= collapse.start_absolute && absolute <= collapse.end_absolute
                })?;
                self.summary_row(collapse.range.zone_id)
            }),
            ProjectedTopAnchor::Summary {
                zone_id,
                hidden_range,
            } => self.summary_row(zone_id).or_else(|| {
                self.raw_cell_document_row(RawCellAnchor {
                    row_id: hidden_range.start.row,
                    column: hidden_range.start.col,
                })
                .or_else(|| self.raw_row_document_row(hidden_range.start.row))
                .or_else(|| {
                    let start = self.raw_absolute_row(hidden_range.start.row)?;
                    let end = self.raw_absolute_row(hidden_range.end.row)?;
                    self.raw_rows
                        .iter()
                        .filter(|placement| {
                            placement.absolute_row >= start && placement.absolute_row <= end
                        })
                        .find_map(|placement| {
                            placement
                                .first_view_row
                                .and_then(|row| row.checked_sub(self.display_row_base))
                        })
                })
            }),
        }
    }

    #[cfg(test)]
    pub(super) fn metadata_units(&self) -> usize {
        self.rows.len() + self.raw_rows.len() + self.raw_slice_count
    }

    pub(super) fn document_rows(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn row(&self, document_row: usize) -> Option<&ProjectionPlanRow> {
        self.rows.get(document_row)
    }
}

impl DisplayPoint {
    pub const fn new(row: usize, column: usize) -> Self {
        Self { row, column }
    }
}

/// Versioned policy for projecting retained terminal history.
///
/// P0 intentionally has only the identity policy. Keeping its revision
/// separate from `TerminalState::grid_version` lets a later projection policy
/// invalidate its own cache without pretending that PTY-owned cells changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HistoryProjection {
    revision: u64,
}

impl HistoryProjection {
    pub const fn identity() -> Self {
        Self { revision: 0 }
    }

    /// Construct the identity policy at an independently managed revision.
    /// This is useful when projection-only state changes in a future policy.
    pub const fn identity_at_revision(revision: u64) -> Self {
        Self { revision }
    }

    pub const fn revision(self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProjectionMode {
    /// Block mode is disabled, or the alternate screen owns the viewport.
    Bypass,
    /// Primary-screen block mode. P0 still materializes the identity view.
    Identity,
    /// Primary-screen Block Mode with at least one effective document splice.
    Transformed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectedTopAnchor {
    RawCell(RawCellAnchor),
    RawRow(RawRowId),
    Summary {
        zone_id: u64,
        hidden_range: FinishedOutputRange,
    },
}

/// Session-owned scroll state for a transformed block document. Block Mode
/// off and the alternate screen bypass projection without discarding it.
#[derive(Clone, Debug)]
pub struct ProjectionViewState {
    pub(super) offset_from_bottom: usize,
    pub(super) follow_bottom: bool,
    pub(super) top_anchor: Option<ProjectedTopAnchor>,
    pub(super) last_plan_key: Option<ProjectionPlanCacheKey>,
}

impl Default for ProjectionViewState {
    fn default() -> Self {
        Self {
            offset_from_bottom: 0,
            follow_bottom: true,
            top_anchor: None,
            last_plan_key: None,
        }
    }
}

impl ProjectionViewState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn offset_from_bottom(&self) -> usize {
        self.offset_from_bottom
    }

    pub fn set_offset(&mut self, offset: usize, viewport: &ProjectedViewport) {
        self.offset_from_bottom = offset.min(viewport.max_scroll_offset());
        self.follow_bottom = self.offset_from_bottom == 0;
    }

    pub fn scroll(&mut self, lines: isize, viewport: &ProjectedViewport) {
        let offset = if lines > 0 {
            self.offset_from_bottom.saturating_add(lines as usize)
        } else {
            self.offset_from_bottom.saturating_sub(lines.unsigned_abs())
        };
        self.set_offset(offset, viewport);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.offset_from_bottom = 0;
        self.follow_bottom = true;
        self.top_anchor = None;
    }
}

/// Complete key for a projected viewport snapshot.
///
/// `total_lines_scrolled` is deliberately present even though most PTY writes
/// also advance `grid_version`: pushing a compressed scrollback row has a
/// dedicated cache invalidation path and does not itself bump the grid version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProjectionCacheKey {
    pub grid_version: u64,
    pub projection_revision: u64,
    pub total_lines_scrolled: u64,
    pub row_identity_revision: u64,
    pub scrollback_len: usize,
    pub scroll_offset: usize,
    pub rows: usize,
    pub columns: usize,
    /// Explicit screen identity keeps primary and alternate bypass snapshots
    /// distinct even when Block Mode is disabled and both use `Bypass` mode.
    pub alt_screen: bool,
    mode: ProjectionMode,
}

/// Projection state that can change display topology or viewport geometry.
///
/// Unlike [`ProjectionCacheKey`], this deliberately excludes `grid_version`:
/// ordinary cell writes are handled by `TerminalState::row_versions` and must
/// not force a full GPU instance rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProjectionLayoutKey {
    pub projection_revision: u64,
    pub total_lines_scrolled: u64,
    pub row_identity_revision: u64,
    pub scrollback_len: usize,
    pub scroll_offset: usize,
    pub rows: usize,
    pub columns: usize,
    pub alt_screen: bool,
    mode: ProjectionMode,
}

impl ProjectionCacheKey {
    pub fn is_bypass(self) -> bool {
        self.mode == ProjectionMode::Bypass
    }

    pub fn layout_key(self) -> ProjectionLayoutKey {
        ProjectionLayoutKey {
            projection_revision: self.projection_revision,
            total_lines_scrolled: self.total_lines_scrolled,
            row_identity_revision: self.row_identity_revision,
            scrollback_len: self.scrollback_len,
            scroll_offset: self.scroll_offset,
            rows: self.rows,
            columns: self.columns,
            alt_screen: self.alt_screen,
            mode: self.mode,
        }
    }

    #[allow(clippy::too_many_arguments)] // This is the complete cache identity tuple.
    pub(super) const fn new(
        grid_version: u64,
        projection_revision: u64,
        total_lines_scrolled: u64,
        row_identity_revision: u64,
        scrollback_len: usize,
        scroll_offset: usize,
        rows: usize,
        columns: usize,
        alt_screen: bool,
        mode: ProjectionMode,
    ) -> Self {
        Self {
            grid_version,
            projection_revision,
            total_lines_scrolled,
            row_identity_revision,
            scrollback_len,
            scroll_offset,
            rows,
            columns,
            alt_screen,
            mode,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ProjectedSelectionAnchor, RawCellAnchor};
    use super::{
        FinishedOutputRange, ProjectedRowKind, ProjectionCacheKey, ProjectionMode, ProjectionPlan,
        ProjectionPolicy, RawCellBoundary, RawRowId, RawRowLayout, RawRowSource, RawSliceOrigin,
        RawSliceSource, ResolvedCollapse,
    };
    use std::collections::BTreeSet;

    fn key(grid_version: u64) -> ProjectionCacheKey {
        ProjectionCacheKey::new(
            grid_version,
            2,
            3,
            4,
            5,
            6,
            7,
            8,
            false,
            ProjectionMode::Identity,
        )
    }

    #[test]
    fn layout_key_ignores_cell_revision_but_tracks_every_topology_input() {
        let baseline = key(10);
        assert_eq!(baseline.layout_key(), key(11).layout_key());

        let changed = [
            ProjectionCacheKey::new(10, 9, 3, 4, 5, 6, 7, 8, false, ProjectionMode::Identity),
            ProjectionCacheKey::new(10, 2, 9, 4, 5, 6, 7, 8, false, ProjectionMode::Identity),
            ProjectionCacheKey::new(10, 2, 3, 9, 5, 6, 7, 8, false, ProjectionMode::Identity),
            ProjectionCacheKey::new(10, 2, 3, 4, 9, 6, 7, 8, false, ProjectionMode::Identity),
            ProjectionCacheKey::new(10, 2, 3, 4, 5, 9, 7, 8, false, ProjectionMode::Identity),
            ProjectionCacheKey::new(10, 2, 3, 4, 5, 6, 9, 8, false, ProjectionMode::Identity),
            ProjectionCacheKey::new(10, 2, 3, 4, 5, 6, 7, 9, false, ProjectionMode::Identity),
            ProjectionCacheKey::new(10, 2, 3, 4, 5, 6, 7, 8, true, ProjectionMode::Identity),
            ProjectionCacheKey::new(10, 2, 3, 4, 5, 6, 7, 8, false, ProjectionMode::Bypass),
        ];
        for changed in changed {
            assert_ne!(baseline.layout_key(), changed.layout_key());
        }
    }

    #[test]
    fn identity_plan_reflows_history_but_keeps_grid_as_a_hard_boundary() {
        let history_first = RawRowId::new(1);
        let history_second = RawRowId::new(2);
        let blank_history = RawRowId::new(3);
        let grid_row = RawRowId::new(4);
        let plan = ProjectionPlan::identity(
            [
                RawRowLayout::new(0, history_first, 3, [], true),
                RawRowLayout::new(1, history_second, 3, [], false),
                RawRowLayout::new(2, blank_history, 0, [], false),
            ],
            [RawRowLayout::new(3, grid_row, 0, [], true)],
            4,
        );

        assert_eq!(plan.cols, 4);
        assert_eq!(plan.rows.len(), 4);
        assert_eq!(plan.raw_slice_count, 4);

        let first = &plan.rows[0];
        assert!(first.wrapped);
        assert_eq!(first.raw_slices.len(), 2);
        assert_eq!(
            first.raw_slices[0].source,
            RawSliceSource {
                absolute_row: 0,
                col_start: 0,
            }
        );
        assert_eq!(first.raw_slices[0].len, 3);
        assert_eq!(first.raw_slices[1].view_col_start, 3);
        assert_eq!(
            first.raw_slices[1].origin,
            Some(RawSliceOrigin {
                row: history_second,
                col_start: 0,
            })
        );
        assert_eq!(first.raw_slices[1].len, 1);

        let second = &plan.rows[1];
        assert!(!second.wrapped);
        assert_eq!(second.raw_slices.len(), 1);
        assert_eq!(second.raw_slices[0].view_col_start, 0);
        assert_eq!(second.raw_slices[0].source.col_start, 1);
        assert_eq!(second.raw_slices[0].len, 2);
        assert_eq!(
            second.row_source,
            Some(RawRowSource {
                raw_row: history_second,
                raw_absolute_row: 1,
            })
        );

        let blank = &plan.rows[2];
        assert!(blank.raw_slices.is_empty());
        assert_eq!(
            blank.row_source,
            Some(RawRowSource {
                raw_row: blank_history,
                raw_absolute_row: 2,
            })
        );
        assert!(!blank.wrapped);

        let grid = &plan.rows[3];
        assert_eq!(grid.raw_slices.len(), 1);
        assert_eq!(grid.raw_slices[0].source.absolute_row, 3);
        assert_eq!(grid.raw_slices[0].len, 4);
        assert!(grid.wrapped, "grid wrap metadata must not join rows");

        assert_eq!(plan.raw_rows[0].first_view_row, Some(0));
        assert_eq!(plan.raw_rows[0].last_view_row, Some(0));
        assert_eq!(plan.raw_rows[1].first_view_row, Some(0));
        assert_eq!(plan.raw_rows[1].last_view_row, Some(1));
        assert_eq!(plan.raw_rows[2].first_view_row, Some(2));
        assert_eq!(plan.raw_rows[2].last_view_row, Some(2));
        assert_eq!(plan.raw_rows[3].first_view_row, Some(3));
        assert_eq!(plan.raw_rows[3].last_view_row, Some(3));
    }

    #[test]
    fn selection_cell_anchor_fails_closed_when_live_trailing_blank_is_trimmed() {
        let row_id = RawRowId::new(44);
        let trailing_blank = ProjectedSelectionAnchor::Cell(RawCellAnchor { row_id, column: 10 });

        // Live grid rows expose stable origins through the full terminal
        // width, including trailing blank cells.
        let live = ProjectionPlan::identity(
            std::iter::empty(),
            [RawRowLayout::new(0, row_id, 4, [], false)],
            12,
        );
        assert_eq!(
            live.selection_point_for_anchor(trailing_blank),
            Some((0, 10))
        );

        // Once the same physical row enters compressed history, only its four
        // active cells remain. Reusing row identity here would silently change
        // a selected space into an empty string.
        let history = ProjectionPlan::identity(
            [RawRowLayout::new(0, row_id, 4, [], false)],
            std::iter::empty(),
            12,
        );
        assert_eq!(history.selection_point_for_anchor(trailing_blank), None);
    }

    #[test]
    fn identity_plan_keeps_untracked_sources_without_claiming_origins() {
        let plan = ProjectionPlan::identity(
            [RawRowLayout::new(0, RawRowId::UNTRACKED, 2, [], false)],
            [RawRowLayout::new(1, RawRowId::UNTRACKED, 0, [], false)],
            5,
        );

        assert_eq!(plan.rows.len(), 2);
        let history = &plan.rows[0];
        assert_eq!(
            history.raw_slices[0].source,
            RawSliceSource {
                absolute_row: 0,
                col_start: 0,
            }
        );
        assert_eq!(history.raw_slices[0].len, 2);
        assert_eq!(history.raw_slices[0].origin, None);
        assert_eq!(history.row_source, None);

        let grid = &plan.rows[1];
        assert_eq!(grid.raw_slices[0].source.absolute_row, 1);
        assert_eq!(grid.raw_slices[0].len, 5);
        assert_eq!(grid.raw_slices[0].origin, None);
        assert_eq!(grid.row_source, None);
    }

    #[test]
    fn identity_plan_width_one_skips_wide_continuation_and_marks_body() {
        let row = RawRowId::new(7);
        let plan = ProjectionPlan::identity(
            [RawRowLayout::new(0, row, 3, [1], false)],
            std::iter::empty(),
            1,
        );

        assert_eq!(plan.rows.len(), 2);
        assert_eq!(plan.raw_slice_count, 2);
        assert_eq!(plan.rows[0].raw_slices[0].source.col_start, 0);
        assert!(plan.rows[0].raw_slices[0].narrow_wide_body);
        assert!(plan.rows[0].wrapped);
        assert_eq!(plan.rows[1].raw_slices[0].source.col_start, 2);
        assert!(!plan.rows[1].raw_slices[0].narrow_wide_body);
        assert!(!plan.rows[1].wrapped);
        assert_eq!(plan.raw_rows[0].first_view_row, Some(0));
        assert_eq!(plan.raw_rows[0].last_view_row, Some(1));
    }

    #[test]
    fn identity_plan_never_splits_a_wide_pair_at_a_reflow_boundary() {
        let row = RawRowId::new(9);
        let plan = ProjectionPlan::identity(
            [RawRowLayout::new(0, row, 7, [4], false)],
            std::iter::empty(),
            4,
        );

        assert_eq!(plan.rows.len(), 2);
        assert_eq!(plan.rows[0].raw_slices[0].len, 3);
        assert!(plan.rows[0].wrapped);
        assert_eq!(plan.rows[1].raw_slices[0].source.col_start, 3);
        assert_eq!(plan.rows[1].raw_slices[0].len, 4);
        assert!(!plan.rows[1].wrapped);
    }

    #[test]
    fn collapse_splice_keeps_two_disjoint_ranges_on_one_raw_row() {
        let row = RawRowId::new(21);
        let base = ProjectionPlan::identity(
            std::iter::empty(),
            [RawRowLayout::new(0, row, 12, [], false)],
            12,
        );
        // Deliberately reverse raw order: caller iteration order is policy
        // order, not a geometry contract.
        let collapses = [
            ResolvedCollapse {
                range: FinishedOutputRange {
                    zone_id: 42,
                    start: RawCellBoundary { row, col: 6 },
                    end: RawCellBoundary { row, col: 8 },
                },
                start_absolute: 0,
                end_absolute: 0,
            },
            ResolvedCollapse {
                range: FinishedOutputRange {
                    zone_id: 41,
                    start: RawCellBoundary { row, col: 2 },
                    end: RawCellBoundary { row, col: 4 },
                },
                start_absolute: 0,
                end_absolute: 0,
            },
        ];

        let collapsed = base.splice_collapses(&collapses, 9);

        assert_eq!(collapsed.effective_collapsed, BTreeSet::from([41, 42]));
        assert_eq!(
            collapsed
                .rows
                .iter()
                .filter(|row| matches!(row.kind, ProjectedRowKind::CollapsedSummary { .. }))
                .count(),
            2
        );
        let raw: Vec<_> = collapsed
            .rows
            .iter()
            .flat_map(|row| row.raw_slices.iter())
            .map(|slice| (slice.source.col_start, slice.len, slice.view_col_start))
            .collect();
        assert_eq!(raw, vec![(0, 2, 0), (4, 2, 4), (8, 4, 8)]);
    }

    #[test]
    fn collapse_splice_counts_a_real_empty_history_row_without_cell_origins() {
        let row = RawRowId::new(22);
        let base = ProjectionPlan::identity(
            [RawRowLayout::new(0, row, 0, [], false)],
            std::iter::empty(),
            8,
        );
        assert!(base.rows[0].raw_slices.is_empty());
        let range = FinishedOutputRange {
            zone_id: 51,
            start: RawCellBoundary { row, col: 0 },
            end: RawCellBoundary { row, col: 8 },
        };

        let collapsed = base.splice_collapses(
            &[ResolvedCollapse {
                range,
                start_absolute: 0,
                end_absolute: 0,
            }],
            3,
        );

        assert_eq!(collapsed.rows.len(), 1);
        assert!(matches!(
            collapsed.rows[0].kind,
            ProjectedRowKind::CollapsedSummary {
                hidden_range,
                hidden_display_rows: 1,
                ..
            } if hidden_range == range
        ));
        assert!(collapsed.rows[0].raw_slices.is_empty());
        assert!(collapsed.rows[0].row_source.is_none());
    }

    #[test]
    fn collapse_policy_is_idempotent_and_never_wraps_its_revision() {
        let mut policy = ProjectionPolicy::new();
        assert!(policy.collapse(7));
        assert!(!policy.collapse(7));
        assert!(policy.is_collapsed(7));
        assert!(policy.expand(7));
        assert!(!policy.expand(7));
        assert!(policy.is_identity());

        policy.revision = u64::MAX;
        assert!(!policy.collapse(8));
        assert!(!policy.is_collapsed(8));
    }

    #[test]
    fn collapse_of_already_trimmed_trailing_blanks_stays_identity() {
        let row = RawRowId::new(23);
        // History reflow already omits raw columns 2..8. Hiding a subset of
        // those structural/non-document cells must not manufacture a summary
        // row or leave the identity fast path.
        let base = ProjectionPlan::identity(
            [RawRowLayout::new(0, row, 2, [], false)],
            std::iter::empty(),
            8,
        );
        let collapsed = base.clone().splice_collapses(
            &[ResolvedCollapse {
                range: FinishedOutputRange {
                    zone_id: 61,
                    start: RawCellBoundary { row, col: 4 },
                    end: RawCellBoundary { row, col: 6 },
                },
                start_absolute: 0,
                end_absolute: 0,
            }],
            5,
        );

        assert!(collapsed.effective_collapsed.is_empty());
        assert_eq!(collapsed.rows, base.rows);
    }
}

/// One affine run in the raw/display origin map.
///
/// Runs never cross a raw row or a display row. Structural cells introduced
/// solely to pad a reflowed row have no run and therefore map to `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OriginSpan {
    pub display_start: DisplayPoint,
    pub raw_start: RawCellAnchor,
    pub len: usize,
}

pub(super) struct MaterializedProjection {
    pub(super) cells: Vec<Vec<TerminalCell>>,
    pub(super) row_wrapped: Vec<bool>,
    pub(super) row_kinds: Vec<ProjectedRowKind>,
    pub(super) row_sources: Vec<Option<RawRowSource>>,
    pub(super) origins: Vec<OriginSpan>,
    pub(super) document_start: usize,
    pub(super) top_padding: usize,
}

impl OriginSpan {
    fn display_end(self) -> usize {
        self.display_start.column.saturating_add(self.len)
    }

    fn raw_end(self) -> usize {
        self.raw_start.column.saturating_add(self.len)
    }
}

/// Immutable materialized viewport plus its stable raw-cell provenance.
///
/// The cell `Arc` is the exact allocation returned by the pre-projection
/// visible-cell path. Thus the identity fast path adds no cell clone and P0
/// preserves every byte and geometry decision made by the existing reflow.
#[derive(Clone, Debug)]
pub struct ProjectedViewport {
    key: ProjectionCacheKey,
    cells: Arc<Vec<Vec<TerminalCell>>>,
    row_wrapped: Arc<Vec<bool>>,
    origins_by_display: Arc<Vec<OriginSpan>>,
    origins_by_raw: Arc<Vec<OriginSpan>>,
    row_kinds: Arc<Vec<ProjectedRowKind>>,
    row_sources: Arc<Vec<Option<RawRowSource>>>,
    cursor: DisplayPoint,
    document_rows: usize,
    document_start: usize,
    top_padding: usize,
    effective_collapsed: Arc<BTreeSet<u64>>,
}

impl ProjectedViewport {
    /// `row_sources` carries per-display-row provenance for the identity
    /// viewport. It must be reflow-aware and `key.rows` long; a shorter or
    /// absent vector is padded with `None`, which fails closed to "no
    /// provenance" for those rows.
    pub(super) fn new(
        key: ProjectionCacheKey,
        cells: Arc<Vec<Vec<TerminalCell>>>,
        row_wrapped: Vec<bool>,
        origins: Vec<OriginSpan>,
        mut row_sources: Vec<Option<RawRowSource>>,
        cursor: DisplayPoint,
    ) -> Self {
        row_sources.resize(key.rows, None);
        let mut origins_by_raw = origins.clone();
        origins_by_raw.sort_unstable_by_key(|span| {
            (
                span.raw_start.row_id,
                span.raw_start.column,
                span.display_start.row,
                span.display_start.column,
            )
        });
        Self {
            key,
            cells,
            row_wrapped: Arc::new(row_wrapped),
            origins_by_display: Arc::new(origins),
            origins_by_raw: Arc::new(origins_by_raw),
            row_kinds: Arc::new(vec![ProjectedRowKind::Raw; key.rows]),
            row_sources: Arc::new(row_sources),
            cursor,
            document_rows: key.scrollback_len.saturating_add(key.rows),
            document_start: key.scrollback_len.saturating_sub(key.scroll_offset),
            top_padding: 0,
            effective_collapsed: Arc::new(BTreeSet::new()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_transformed(
        key: ProjectionCacheKey,
        cells: Vec<Vec<TerminalCell>>,
        row_wrapped: Vec<bool>,
        row_kinds: Vec<ProjectedRowKind>,
        row_sources: Vec<Option<RawRowSource>>,
        origins: Vec<OriginSpan>,
        cursor: DisplayPoint,
        document_rows: usize,
        document_start: usize,
        top_padding: usize,
        effective_collapsed: BTreeSet<u64>,
    ) -> Self {
        let mut origins_by_raw = origins.clone();
        origins_by_raw.sort_unstable_by_key(|span| {
            (
                span.raw_start.row_id,
                span.raw_start.column,
                span.display_start.row,
                span.display_start.column,
            )
        });
        Self {
            key,
            cells: Arc::new(cells),
            row_wrapped: Arc::new(row_wrapped),
            origins_by_display: Arc::new(origins),
            origins_by_raw: Arc::new(origins_by_raw),
            row_kinds: Arc::new(row_kinds),
            row_sources: Arc::new(row_sources),
            cursor,
            document_rows,
            document_start,
            top_padding,
            effective_collapsed: Arc::new(effective_collapsed),
        }
    }

    pub fn key(&self) -> ProjectionCacheKey {
        self.key
    }

    pub fn cells(&self) -> &[Vec<TerminalCell>] {
        self.cells.as_slice()
    }

    pub fn cells_arc(&self) -> Arc<Vec<Vec<TerminalCell>>> {
        Arc::clone(&self.cells)
    }

    pub fn row_wrapped(&self) -> &[bool] {
        self.row_wrapped.as_slice()
    }

    pub fn row_kinds(&self) -> &[ProjectedRowKind] {
        self.row_kinds.as_slice()
    }

    pub fn row_kind(&self, row: usize) -> Option<ProjectedRowKind> {
        self.row_kinds.get(row).copied()
    }

    pub fn cursor(&self) -> DisplayPoint {
        self.cursor
    }

    pub fn rows(&self) -> usize {
        self.cells.len()
    }

    pub fn columns(&self) -> usize {
        self.cells.first().map_or(self.key.columns, Vec::len)
    }

    pub fn scroll_offset(&self) -> usize {
        self.key.scroll_offset
    }

    pub fn is_transformed(&self) -> bool {
        self.key.mode == ProjectionMode::Transformed
    }

    /// Exact non-zero full-document plan instance for transformed snapshots.
    pub fn plan_revision(&self) -> Option<u64> {
        self.is_transformed()
            .then_some(self.key.projection_revision)
            .filter(|revision| *revision != 0)
    }

    pub fn document_rows(&self) -> usize {
        self.document_rows
    }

    pub fn document_start(&self) -> usize {
        self.document_start
    }

    pub fn top_padding(&self) -> usize {
        self.top_padding
    }

    pub fn max_scroll_offset(&self) -> usize {
        self.document_rows.saturating_sub(self.rows())
    }

    pub fn effective_collapsed(&self) -> &BTreeSet<u64> {
        self.effective_collapsed.as_ref()
    }

    pub fn view_document_row(&self, display_row: usize) -> Option<usize> {
        (display_row >= self.top_padding && display_row < self.rows()).then(|| {
            self.document_start
                .saturating_add(display_row - self.top_padding)
        })
    }

    pub fn view_row_absolute(&self, display_row: usize) -> Option<usize> {
        self.row_sources
            .get(display_row)
            .copied()
            .flatten()
            .map(|source| source.raw_absolute_row)
    }

    pub(super) fn row_has_origin(&self, display_row: usize) -> bool {
        let index = self
            .origins_by_display
            .partition_point(|span| span.display_start.row < display_row);
        self.origins_by_display
            .get(index)
            .is_some_and(|span| span.display_start.row == display_row)
    }

    pub(super) fn row_source_at(&self, display_row: usize) -> Option<RawRowSource> {
        self.row_sources.get(display_row).copied().flatten()
    }

    pub(super) fn real_column_bounds(&self, display_row: usize) -> Option<(usize, usize)> {
        let start = self
            .origins_by_display
            .partition_point(|span| span.display_start.row < display_row);
        let mut spans = self.origins_by_display[start..]
            .iter()
            .take_while(|span| span.display_start.row == display_row);
        let Some(first) = spans.next() else {
            let source = self.row_source_at(display_row)?;
            return (source.raw_row.is_tracked()
                && matches!(self.row_kind(display_row), Some(ProjectedRowKind::Raw)))
            .then(|| (0, self.columns().saturating_sub(1)));
        };
        Some(spans.fold(
            (
                first.display_start.column,
                first.display_end().saturating_sub(1),
            ),
            |(left, right), span| {
                (
                    left.min(span.display_start.column),
                    right.max(span.display_end().saturating_sub(1)),
                )
            },
        ))
    }

    pub fn raw_row_view_bounds(&self, row_id: RawRowId) -> Option<(usize, usize)> {
        if !row_id.is_tracked() {
            return None;
        }
        let mut matches = self
            .row_sources
            .iter()
            .enumerate()
            .filter_map(|(row, source)| {
                source
                    .filter(|source| source.raw_row == row_id)
                    .map(|_| row)
            })
            .chain(self.origins_by_display.iter().filter_map(|span| {
                (span.raw_start.row_id == row_id).then_some(span.display_start.row)
            }));
        let first = matches.next()?;
        Some(matches.fold((first, first), |(min, max), row| {
            (min.min(row), max.max(row))
        }))
    }

    /// First exact raw cell represented by one display row. This is O(log S)
    /// over visible origin spans and lets chrome classify same-physical-row
    /// command fragments without scanning every terminal cell.
    pub fn first_raw_anchor_in_row(&self, display_row: usize) -> Option<RawCellAnchor> {
        let index = self
            .origins_by_display
            .partition_point(|span| span.display_start.row < display_row);
        self.origins_by_display
            .get(index)
            .filter(|span| span.display_start.row == display_row)
            .map(|span| span.raw_start)
    }

    pub(super) fn stable_top_anchor(&self) -> Option<ProjectedTopAnchor> {
        let view_row = self.top_padding;
        match self.row_kinds.get(view_row).copied()? {
            ProjectedRowKind::Padding => None,
            ProjectedRowKind::CollapsedSummary {
                key, hidden_range, ..
            } => Some(ProjectedTopAnchor::Summary {
                zone_id: key.zone_id,
                hidden_range,
            }),
            ProjectedRowKind::Raw => self
                .origins_by_display
                .iter()
                .filter(|span| span.display_start.row == view_row)
                .min_by_key(|span| span.display_start.column)
                .map(|span| ProjectedTopAnchor::RawCell(span.raw_start))
                .or_else(|| {
                    self.row_sources
                        .get(view_row)
                        .copied()
                        .flatten()
                        .map(|source| ProjectedTopAnchor::RawRow(source.raw_row))
                }),
        }
    }

    pub fn history_len(&self) -> usize {
        self.key.scrollback_len
    }

    pub fn total_lines(&self) -> usize {
        self.history_len().saturating_add(self.rows())
    }

    /// Preserve the pre-projection scrollbar/selection coordinate arithmetic.
    /// Stable origin APIs below are intentionally separate in P0 so adopting
    /// the foundation cannot silently alter existing selection geometry.
    pub(crate) fn legacy_absolute_row(&self, display_row: usize) -> usize {
        self.history_len()
            .saturating_sub(self.scroll_offset())
            .saturating_add(display_row)
    }

    /// P0 mouse reporting remains in the application's displayed grid space.
    pub fn application_cell(&self, point: DisplayPoint) -> Option<(usize, usize)> {
        if !self.is_transformed() {
            return (point.row < self.rows() && point.column < self.columns())
                .then_some((point.row, point.column));
        }
        let anchor = self.raw_anchor_at(point)?;
        let source = self.row_sources.get(point.row)?.as_ref()?;
        // `then`, not `then_some`: the latter takes its value BY VALUE, so the
        // subtraction ran before the guard that protects it and underflowed on
        // any row sourced from scrollback rather than the live grid — an
        // ordinary hover while a block is collapsed and the view is scrolled.
        // Release builds discarded the wrapped value, but debug builds
        // panicked.
        (source.raw_row == anchor.row_id && source.raw_absolute_row >= self.key.scrollback_len)
            .then(|| {
                (
                    source.raw_absolute_row - self.key.scrollback_len,
                    anchor.column,
                )
            })
    }

    /// Preserve Kitty's existing live-viewport-relative placement contract.
    pub fn kitty_viewport_row(&self, placement_y: i64) -> i64 {
        placement_y.saturating_add(self.scroll_offset() as i64)
    }

    /// Return the stable raw origin of a real projected cell. Reflow padding
    /// has no origin. Wide continuations retain their own raw column so every
    /// surviving physical cell round-trips independently.
    pub fn raw_anchor_at(&self, point: DisplayPoint) -> Option<RawCellAnchor> {
        self.cells.get(point.row)?.get(point.column)?;
        let index = self.origins_by_display.partition_point(|span| {
            span.display_start.row < point.row
                || (span.display_start.row == point.row
                    && span.display_start.column <= point.column)
        });
        let span = self.origins_by_display.get(index.checked_sub(1)?)?;
        if span.display_start.row != point.row
            || point.column < span.display_start.column
            || point.column >= span.display_end()
        {
            return None;
        }
        Some(RawCellAnchor {
            row_id: span.raw_start.row_id,
            column: span
                .raw_start
                .column
                .saturating_add(point.column - span.display_start.column),
        })
    }

    /// Locate a retained raw cell in this projected viewport. Anchors that
    /// were evicted, truncated, or represented only by structural padding
    /// fail closed. Wide continuations retain their own raw/display column.
    pub fn display_point_for(&self, anchor: RawCellAnchor) -> Option<DisplayPoint> {
        if !anchor.row_id.is_tracked() {
            return None;
        }
        let index = self.origins_by_raw.partition_point(|span| {
            span.raw_start.row_id < anchor.row_id
                || (span.raw_start.row_id == anchor.row_id
                    && span.raw_start.column <= anchor.column)
        });
        let span = self.origins_by_raw.get(index.checked_sub(1)?)?;
        if span.raw_start.row_id != anchor.row_id
            || anchor.column < span.raw_start.column
            || anchor.column >= span.raw_end()
        {
            return None;
        }
        Some(DisplayPoint {
            row: span.display_start.row,
            column: span
                .display_start
                .column
                .saturating_add(anchor.column - span.raw_start.column),
        })
    }
}
