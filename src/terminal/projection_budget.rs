//! Host-owned resource policy; rejection never falls back to guessed geometry.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionUnavailable {
    SourceRows,
    ViewRows,
    RawSlices,
    WideScratch,
    RawCells,
    ArithmeticOverflow,
}
pub(super) type RawBudgetCache = (
    (ProjectionAssessmentKey, ProjectionBudget),
    Result<(), ProjectionUnavailable>,
);
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MaterializationFailureKey {
    pub source: ProjectionAssessmentKey,
    pub policy: ProjectionPolicy,
    pub intent: (usize, bool, bool, bool),
    pub presentation_rows: Option<usize>,
    pub bypass: bool,
}
/// Scalar source identity. Policy/scope/Chat changes invalidate the cache at
/// the formal frame entry point; UI guards never rebuild or scan policy keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProjectionAssessmentKey {
    revisions: [u64; 6],
    dimensions: [usize; 5],
    alternate: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProjectionBudget {
    pub source_rows: usize,
    pub view_rows: usize,
    pub raw_slices: usize,
    pub wide_scratch: usize,
    pub raw_cells: usize,
}
impl Default for ProjectionBudget {
    fn default() -> Self {
        Self {
            source_rows: 101_024,
            view_rows: 131_072,
            raw_slices: 262_144,
            wide_scratch: 1_048_576,
            raw_cells: 1_048_576,
        }
    }
}
pub(super) fn add(a: usize, b: usize) -> Result<usize, ProjectionUnavailable> {
    a.checked_add(b)
        .ok_or(ProjectionUnavailable::ArithmeticOverflow)
}
pub(super) fn limited(
    value: usize,
    cap: usize,
    reason: ProjectionUnavailable,
) -> Result<usize, ProjectionUnavailable> {
    if value > cap {
        Err(reason)
    } else {
        Ok(value)
    }
}
impl TerminalState {
    pub(super) fn projection_assessment_key(&self) -> ProjectionAssessmentKey {
        ProjectionAssessmentKey {
            revisions: [
                self.grid_version,
                self.search_source_revision,
                self.row_identity_revision,
                self.total_lines_scrolled,
                self.finished_output_revision,
                self.full_screen_scroll_revision,
            ],
            dimensions: [
                self.scrollback.len(),
                self.screen.grid.rows(),
                self.screen.grid.row_len(),
                self.scroll_offset,
                self.command_records.len(),
            ],
            alternate: self.screen.use_alt_buffer,
        }
    }
    pub fn projection_unavailable(&self) -> Option<ProjectionUnavailable> {
        if let Some(reason) = self.projection_failure {
            return Some(reason);
        }
        let key = self.projection_assessment_key();
        if self
            .projection_frame_source
            .is_some_and(|certified| certified != key)
        {
            // A retained frame is never authority for a newer source, even if
            // that source still fits the budget. Only formal rendering renews it.
            self.projection_frame_valid
                .store(false, std::sync::atomic::Ordering::Release);
        }
        if let Some((cached, result)) = self.projection_query_cache.get() {
            if cached == key {
                return result;
            }
        }
        #[cfg(test)]
        self.projection_query_evaluations
            .set(self.projection_query_evaluations.get() + 1);
        let result = match &self.projection_last_policy {
            Some(policy) => self
                .plan_budget(self.screen.grid.row_len().max(1), policy)
                .err(),
            None => self.raw_budget().err(),
        };
        self.projection_query_cache.set(Some((key, result)));
        result
    }
    #[cfg(test)]
    pub(crate) fn set_projection_test_row_budget(&mut self, rows: usize) {
        self.projection_test_budget = Some(ProjectionBudget {
            view_rows: rows,
            ..ProjectionBudget::default()
        });
        *self.projection_budget_cache.get_mut() = None;
        self.projection_query_cache.set(None);
        self.projection_materialization_failure = None;
    }
    pub(super) fn begin_projection_frame(&mut self, policy: Option<&ProjectionPolicy>) {
        let source = self.projection_assessment_key();
        if self.projection_frame_source != Some(source)
            || self.projection_last_policy.as_ref() != policy
        {
            self.projection_query_cache.set(None);
            self.projection_frame_valid
                .store(false, std::sync::atomic::Ordering::Release);
            self.projected_viewport_cache = None;
            self.transformed_viewport_cache = None;
        }
        self.projection_frame_source = Some(source);
        self.projection_last_policy = policy.cloned();
    }
    pub(super) fn projection_limits(&self) -> ProjectionBudget {
        #[cfg(test)]
        if let Some(value) = self.projection_test_budget {
            return value;
        }
        ProjectionBudget::default()
    }
    pub(super) fn fail_projection(&mut self, why: ProjectionUnavailable) {
        self.projection_failure = Some(why);
        self.projection_frame_valid
            .store(false, std::sync::atomic::Ordering::Release);
        self.projection_plan_cache = None;
        self.transformed_viewport_cache = None;
        self.projected_viewport_cache = None;
        self.visible_cells_cache = None;
        self.viewport_mapping_exact_cache.set(None);
        self.clear_text_selection();
    }
    pub(super) fn resume_projection(&mut self) {
        self.projection_failure = None;
        if !self
            .projection_frame_valid
            .load(std::sync::atomic::Ordering::Acquire)
        {
            // Previously returned frames must remain revoked after recovery.
            self.projection_frame_valid =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        }
    }
    pub(super) fn unavailable_viewport(&self, projection: HistoryProjection) -> ProjectedViewport {
        let key = ProjectionCacheKey::new(
            self.grid_version,
            projection.revision(),
            self.total_lines_scrolled,
            self.row_identity_revision,
            self.scrollback.len(),
            self.scroll_offset,
            self.screen.grid.rows(),
            self.screen.grid.row_len(),
            self.screen.use_alt_buffer,
            projection::ProjectionMode::Bypass,
        );
        ProjectedViewport::unavailable(key, self.projection_failure.expect("rejection recorded"))
    }
    pub(super) fn source_budget(
        &self,
        policy: Option<&ProjectionPolicy>,
    ) -> Result<(), ProjectionUnavailable> {
        let b = self.projection_limits();
        limited(
            add(self.scrollback.len(), self.screen.grid.rows())?,
            b.source_rows,
            ProjectionUnavailable::SourceRows,
        )?;
        if let Some(p) = policy {
            limited(
                p.collapsed_count(),
                b.source_rows,
                ProjectionUnavailable::SourceRows,
            )?;
        }
        limited(
            self.command_records.len(),
            b.source_rows,
            ProjectionUnavailable::SourceRows,
        )?;
        Ok(())
    }
    /// No layouts, cells, expanded rows or per-column arrays are allocated.
    /// cols-1 is a conservative lower bound on a non-final wide-aware row.
    pub(super) fn plan_budget(
        &self,
        cols: usize,
        policy: &ProjectionPolicy,
    ) -> Result<(), ProjectionUnavailable> {
        self.source_budget(Some(policy))?;
        let key = self.projection_plan_cache_key(cols, policy);
        if let Some((cached, result)) = &*self.projection_budget_cache.borrow() {
            if *cached == key {
                return *result;
            }
        }
        let result = self.plan_budget_uncached(cols, policy);
        *self.projection_budget_cache.borrow_mut() = Some((key, result));
        result
    }
    fn plan_budget_uncached(
        &self,
        cols: usize,
        policy: &ProjectionPolicy,
    ) -> Result<(), ProjectionUnavailable> {
        let b = self.projection_limits();
        let mut rows = self.screen.grid.rows();
        let mut logical = 0usize;
        let mut wide = 0usize;
        let mut open = false;
        for line in &self.scrollback {
            wide = limited(
                add(wide, line.reflow_wide_continuations().len())?,
                b.wide_scratch,
                ProjectionUnavailable::WideScratch,
            )?;
            logical = add(logical, line.reflow_content_len())?;
            open = true;
            if !line.is_wrapped {
                rows = limited(
                    add(rows, logical.div_ceil(cols.saturating_sub(1).max(1)).max(1))?,
                    b.view_rows,
                    ProjectionUnavailable::ViewRows,
                )?;
                logical = 0;
                open = false;
            }
        }
        if open {
            rows = add(rows, logical.div_ceil(cols.saturating_sub(1).max(1)).max(1))?;
        }
        let commands = self.command_records.len();
        // Spacing, header, chat output spacer, summaries and dock decoration.
        let mut extra = policy
            .collapsed_count()
            .checked_mul(2)
            .ok_or(ProjectionUnavailable::ArithmeticOverflow)?;
        for enabled in [
            policy.block_spacing(),
            policy.block_headers(),
            policy.chat_mode(),
        ] {
            if enabled {
                extra = add(extra, commands)?;
            }
        }
        extra = add(extra, policy.companion_status_rows())?;
        if policy.chat_mode() {
            extra = add(
                extra,
                add(policy.chat_footer_rows(), self.screen.grid.rows())?,
            )?;
        }
        limited(
            add(rows, extra)?,
            b.view_rows,
            ProjectionUnavailable::ViewRows,
        )?;
        let slices = add(add(rows, self.scrollback.len())?, self.screen.grid.rows())?;
        limited(
            add(
                slices,
                policy
                    .collapsed_count()
                    .checked_mul(2)
                    .ok_or(ProjectionUnavailable::ArithmeticOverflow)?,
            )?,
            b.raw_slices,
            ProjectionUnavailable::RawSlices,
        )?;
        Ok(())
    }
    pub(super) fn materialization_budget(
        &self,
        plan: &projection::ProjectionPlan,
        start: usize,
        rows: usize,
    ) -> Result<(), ProjectionUnavailable> {
        let dock = plan
            .chat_dock_start
            .map(|at| (at, plan.document_rows().saturating_sub(at)))
            .filter(|(_, count)| *count > 0 && *count < rows);
        let mut decoded = 0usize;
        for (first, count) in std::iter::once((start, rows)).chain(dock) {
            decoded = limited(
                add(
                    decoded,
                    count
                        .checked_mul(plan.cols)
                        .ok_or(ProjectionUnavailable::ArithmeticOverflow)?,
                )?,
                self.projection_limits().raw_cells,
                ProjectionUnavailable::RawCells,
            )?;
            let mut previous = None;
            for row in plan.rows.iter().skip(first).take(count) {
                for slice in &row.raw_slices {
                    let Some(absolute) =
                        self.projection_source_to_absolute(slice.source.absolute_row)
                    else {
                        continue;
                    };
                    if absolute < self.scrollback.len() && previous != Some(absolute) {
                        decoded = limited(
                            add(decoded, self.scrollback[absolute].columns())?,
                            self.projection_limits().raw_cells,
                            ProjectionUnavailable::RawCells,
                        )?;
                        previous = Some(absolute);
                    }
                }
            }
        }
        Ok(())
    }

    /// Only rows actually decoded into a raw historical logical group count.
    /// This path does not clone compressed history. Actual columns, not trimmed
    /// active length, bound decode.
    pub(super) fn raw_budget(&self) -> Result<(), ProjectionUnavailable> {
        let key = (self.projection_assessment_key(), self.projection_limits());
        if let Some((cached, result)) = self.projection_raw_budget_cache.get() {
            if cached == key {
                return result;
            }
        }
        let result = self.raw_budget_uncached();
        self.projection_raw_budget_cache.set(Some((key, result)));
        result
    }
    fn raw_budget_uncached(&self) -> Result<(), ProjectionUnavailable> {
        #[cfg(test)]
        self.projection_raw_evaluations
            .set(self.projection_raw_evaluations.get() + 1);
        let b = self.projection_limits();
        let rows = self.screen.grid.rows();
        let cols = self.screen.grid.row_len().max(1);
        let mut cells = rows
            .checked_mul(cols)
            .ok_or(ProjectionUnavailable::ArithmeticOverflow)?;
        limited(cells, b.raw_cells, ProjectionUnavailable::RawCells)?;
        if self.screen.use_alt_buffer || self.scroll_offset == 0 {
            return Ok(());
        }
        self.source_budget(None)?;
        let mut start = self
            .scrollback
            .len()
            .saturating_sub(add(self.scroll_offset, rows)?);
        while start > 0 && self.scrollback[start - 1].is_wrapped {
            start -= 1;
        }
        let count = |start: usize| -> Result<(usize, usize), ProjectionUnavailable> {
            let mut end = start;
            let mut len = 0;
            loop {
                let row = &self.scrollback[end];
                len = add(len, row.reflow_content_len())?;
                end += 1;
                if !row.is_wrapped || end == self.scrollback.len() {
                    break;
                }
            }
            Ok((end, len.div_ceil(cols).max(1)))
        };
        let mut total = 0;
        let mut at = start;
        while at < self.scrollback.len() {
            let (end, n) = count(at)?;
            total = add(total, n)?;
            at = end;
        }
        let first = total.saturating_sub(self.scroll_offset);
        let last = add(first, rows)?.min(total);
        at = start;
        let mut visual = 0;
        while at < self.scrollback.len() && visual < last {
            let (end, n) = count(at)?;
            let next = add(visual, n)?;
            if next > first {
                for row in self.scrollback.range(at..end) {
                    cells = limited(
                        add(cells, row.columns())?,
                        b.raw_cells,
                        ProjectionUnavailable::RawCells,
                    )?;
                }
            }
            at = end;
            visual = next;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> ProjectionPolicy {
        let mut p = ProjectionPolicy::new();
        p.set_retain_identity_canvas(true);
        p
    }
    fn frame(t: &mut TerminalState, p: &ProjectionPolicy) -> ProjectedViewport {
        t.projected_viewport_with_state(
            HistoryProjection::identity(),
            true,
            p,
            &mut ProjectionViewState::new(),
        )
    }
    fn history() -> TerminalState {
        let mut t = TerminalState::new(8, 2);
        t.process_input(&[b'x'; 64]);
        assert!(!t.scrollback.is_empty());
        t
    }
    #[test]
    fn idle_success_and_rejection_guards_scan_once_and_recheck_changes() {
        let mut t = history();
        let p = policy();
        t.begin_projection_frame(Some(&p));
        for _ in 0..20 {
            assert_eq!(t.projection_unavailable(), None);
        }
        assert_eq!(t.projection_query_evaluations.get(), 1);
        t.process_input(b"Z");
        assert_eq!(t.projection_unavailable(), None);
        assert_eq!(t.projection_query_evaluations.get(), 2);
        t.set_projection_test_row_budget(0);
        for _ in 0..20 {
            assert_eq!(
                t.projection_unavailable(),
                Some(ProjectionUnavailable::ViewRows)
            );
        }
        assert_eq!(t.projection_query_evaluations.get(), 3);
        t.begin_projection_frame(None);
        for _ in 0..20 {
            assert_eq!(t.projection_unavailable(), None);
        }
        assert_eq!(t.projection_query_evaluations.get(), 4);
        t.scroll_offset = 1;
        assert_eq!(t.projection_unavailable(), None);
        assert_eq!(t.projection_query_evaluations.get(), 5);
    }
    #[test]
    fn source_update_revokes_old_geometry_even_when_new_source_fits() {
        let mut t = history();
        let p = policy();
        let old = frame(&mut t, &p);
        assert!(old.is_available());
        t.process_input(b"Z");
        assert_eq!(t.projection_unavailable(), None);
        assert!(!old.is_available());
        assert!(old.application_cell(DisplayPoint::new(0, 0)).is_none());
        assert!(frame(&mut t, &p).is_available());
        assert!(!old.is_available());
    }
    #[test]
    fn transformed_decode_budget_charges_actual_cache_misses_before_materialization() {
        let mut t = history();
        let p = policy();
        assert!(frame(&mut t, &p).is_available());
        let plan = t.projection_plan_cache.as_ref().unwrap().1.clone();
        let row = plan
            .rows
            .iter()
            .position(|row| !row.raw_slices.is_empty())
            .unwrap();
        let source = t
            .projection_source_to_absolute(plan.rows[row].raw_slices[0].source.absolute_row)
            .unwrap();
        assert!(source < t.scrollback.len());
        let exact = plan.cols + t.scrollback[source].columns();
        t.projection_test_budget = Some(ProjectionBudget {
            raw_cells: exact - 1,
            ..ProjectionBudget::default()
        });
        assert_eq!(
            t.materialization_budget(&plan, row, 1),
            Err(ProjectionUnavailable::RawCells)
        );
        t.projection_test_budget.as_mut().unwrap().raw_cells = exact;
        assert_eq!(t.materialization_budget(&plan, row, 1), Ok(()));
    }
    #[test]
    fn old_identity_frame_cannot_authorize_selection_after_recovery() {
        let mut t = history();
        let p = ProjectionPolicy::new();
        let old = frame(&mut t, &p);
        t.projection_test_budget = Some(ProjectionBudget {
            raw_cells: 0,
            ..ProjectionBudget::default()
        });
        assert!(!frame(&mut t, &p).is_available());
        t.projection_test_budget = None;
        let fresh = frame(&mut t, &p);
        assert!(fresh.is_available());
        assert!(!old.is_available());
        t.start_selection_projected(&old, (0, 0));
        assert!(t.selection.is_none());
        t.start_block_selection_projected(&old, (0, 0));
        assert!(t.selection.is_none());
        t.select_word_at_projected(&old, 0, 0);
        assert!(t.selection.is_none());
        t.select_line_at_projected(&old, 0);
        assert!(t.selection.is_none());
        t.start_selection_projected(&fresh, (0, 0));
        let before = t.selection;
        t.update_selection_projected(&old, (1, 1));
        assert_eq!(t.selection, before);
    }
    #[test]
    fn materialization_failure_keeps_requested_offset_and_caches_idle_but_not_bypass() {
        let mut t = history();
        let p = policy();
        t.projection_test_budget = Some(ProjectionBudget {
            raw_cells: 16,
            ..ProjectionBudget::default()
        });
        let mut state = ProjectionViewState::new();
        state.offset_from_bottom = 1;
        state.follow_bottom = false;
        state.canvas_margin = true;
        let rejected =
            t.projected_viewport_with_state(HistoryProjection::identity(), true, &p, &mut state);
        assert_eq!(
            rejected.unavailable_reason(),
            Some(ProjectionUnavailable::RawCells)
        );
        assert_eq!(state.offset_from_bottom, 1);
        assert!(!state.follow_bottom);
        let revision = t.next_projection_plan_revision;
        for _ in 0..10 {
            assert!(!t
                .projected_viewport_with_state(HistoryProjection::identity(), true, &p, &mut state)
                .is_available());
            assert_eq!(state.offset_from_bottom, 1);
        }
        assert_eq!(t.next_projection_plan_revision, revision);
        assert!(t
            .projected_viewport_with_state(HistoryProjection::identity(), false, &p, &mut state)
            .is_available());
        assert_eq!(state.offset_from_bottom, 1);
    }
    #[test]
    fn repeated_formal_raw_frames_reuse_admission_until_source_changes() {
        let mut t = history();
        t.scroll_offset = 1;
        let p = ProjectionPolicy::new();
        for _ in 0..20 {
            assert!(frame(&mut t, &p).is_available());
        }
        assert_eq!(t.projection_raw_evaluations.get(), 1);
        t.process_input(b"Z");
        assert!(frame(&mut t, &p).is_available());
        assert_eq!(t.projection_raw_evaluations.get(), 2);
    }
    #[test]
    fn budget_empty_unavailable_revokes_old_frame_and_recovers_without_raw_fallback() {
        let mut t = history();
        let p = policy();
        let old = frame(&mut t, &p);
        assert!(old.is_available());
        t.set_projection_test_row_budget(0);
        let rejected = frame(&mut t, &p);
        assert_eq!(
            rejected.unavailable_reason(),
            Some(ProjectionUnavailable::ViewRows)
        );
        assert!(rejected.cells().is_empty());
        assert!(!old.is_available());
        assert!(old.application_cell(DisplayPoint::new(0, 0)).is_none());
        assert!(t.copy_selection().is_none());
        assert!(t
            .projected_viewport(HistoryProjection::identity(), false)
            .cells()
            .is_empty());
        t.set_projection_test_row_budget(131_072);
        let recovered = frame(&mut t, &p);
        assert!(recovered.is_available());
        assert!(!old.is_available());
        assert!(!rejected.is_available());
    }
    #[test]
    fn budget_source_and_slice_exact_limits_and_plus_one_fail_before_plan() {
        let mut t = history();
        let p = policy();
        let source = t.scrollback.len() + t.screen.grid.rows();
        t.projection_test_budget = Some(ProjectionBudget {
            source_rows: source,
            ..ProjectionBudget::default()
        });
        assert!(frame(&mut t, &p).is_available());
        t.projection_test_budget.as_mut().unwrap().source_rows = source - 1;
        assert_eq!(
            frame(&mut t, &p).unavailable_reason(),
            Some(ProjectionUnavailable::SourceRows)
        );
        t.projection_test_budget = Some(ProjectionBudget {
            raw_slices: 1,
            ..ProjectionBudget::default()
        });
        *t.projection_budget_cache.get_mut() = None;
        assert_eq!(
            frame(&mut t, &p).unavailable_reason(),
            Some(ProjectionUnavailable::RawSlices)
        );
    }
    #[test]
    fn budget_raw_history_uses_actual_width_not_trimmed_active_length() {
        let mut t = TerminalState::new(16, 2);
        let styled = TerminalCell {
            foreground: Color::Red,
            ..TerminalCell::default()
        };
        for _ in 0..8 {
            t.scrollback
                .push_back(ScrollbackLine::compress(&[styled; 16], true));
        }
        t.scroll_offset = 1;
        t.projection_test_budget = Some(ProjectionBudget {
            raw_cells: 159,
            ..ProjectionBudget::default()
        });
        assert_eq!(t.raw_budget(), Err(ProjectionUnavailable::RawCells));
        let bad = frame(&mut t, &ProjectionPolicy::new());
        assert!(!bad.is_available());
        assert!(t.get_visible_cells().is_empty());
        t.projection_test_budget.as_mut().unwrap().raw_cells = 160;
        assert!(frame(&mut t, &ProjectionPolicy::new()).is_available());
    }
    #[test]
    fn budget_policy_transition_revokes_prior_scope_frame() {
        let mut t = history();
        let p = policy();
        let old = frame(&mut t, &p);
        let mut changed = p.clone();
        changed.set_inline_scope_floor(Some(u64::MAX));
        let _ = frame(&mut t, &changed);
        assert!(!old.is_available());
        assert!(old.application_cell(DisplayPoint::new(0, 0)).is_none());
    }
    #[test]
    fn budget_growth_is_seen_before_next_render_and_incremental_cannot_bypass() {
        let mut t = history();
        let p = policy();
        let _ = frame(&mut t, &p);
        let old_count = t.scrollback.len();
        t.projection_test_budget = Some(ProjectionBudget {
            source_rows: old_count + t.screen.grid.rows(),
            ..ProjectionBudget::default()
        });
        t.process_input(&[b'y'; 32]);
        assert_eq!(
            t.projection_unavailable(),
            Some(ProjectionUnavailable::SourceRows)
        );
        assert!(t.copy_selection().is_none());
        assert!(!frame(&mut t, &p).is_available());
    }
    #[test]
    fn budget_checked_arithmetic_never_wraps() {
        assert_eq!(
            add(usize::MAX, 1),
            Err(ProjectionUnavailable::ArithmeticOverflow)
        );
        assert_eq!(limited(12, 12, ProjectionUnavailable::ViewRows), Ok(12));
        assert_eq!(
            limited(13, 12, ProjectionUnavailable::ViewRows),
            Err(ProjectionUnavailable::ViewRows)
        );
    }
}
