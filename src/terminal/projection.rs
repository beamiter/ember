use super::{RawRowId, TerminalCell};
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
pub(super) enum ProjectionMode {
    /// Block mode is disabled, or the alternate screen owns the viewport.
    Bypass,
    /// Primary-screen block mode. P0 still materializes the identity view.
    Identity,
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
    use super::{ProjectionCacheKey, ProjectionMode};

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
    cursor: DisplayPoint,
}

impl ProjectedViewport {
    pub(super) fn new(
        key: ProjectionCacheKey,
        cells: Arc<Vec<Vec<TerminalCell>>>,
        row_wrapped: Vec<bool>,
        origins: Vec<OriginSpan>,
        cursor: DisplayPoint,
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
            cells,
            row_wrapped: Arc::new(row_wrapped),
            origins_by_display: Arc::new(origins),
            origins_by_raw: Arc::new(origins_by_raw),
            cursor,
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
        (point.row < self.rows() && point.column < self.columns())
            .then_some((point.row, point.column))
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
