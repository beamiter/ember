// Chat is a presentation of the same PTY-owned cells, never a second editor.
// Projection construction shares state.rs raw-source and selection-plan machinery.
use super::*;
impl super::TerminalState {
    pub fn chat_surface(&self) -> jterm_core::chat_mode::ChatSurface {
        use jterm_core::chat_mode::{resolve_surface, ChatSessionState};
        let current = self.command_records.back();
        resolve_surface(ChatSessionState {
            shell_integration: current.is_some(),
            prompt_ready: self.shell_is_prompt_ready(),
            command_running: current
                .is_some_and(|record| !record.complete && record.state == CommandState::Running),
            alternate_screen: self.use_alt_buffer,
        })
    }

    /// Programs claiming main-screen mouse/graphics or moving above their
    /// output boundary need the whole native surface, just like an alternate
    /// screen. Keep the selected mode so a later ordinary prompt restores Chat.
    pub fn chat_native_takeover(&self) -> bool {
        use jterm_core::chat_mode::ChatSurface;
        if self.chat_native_takeover_sequence.is_some() {
            return true;
        }
        if !matches!(
            self.chat_surface(),
            ChatSurface::Prompt | ChatSurface::Running
        ) || self.is_mouse_enabled()
            || !self.kitty_graphics.get_placements().is_empty()
        {
            return true;
        }
        if self.chat_surface() == ChatSurface::Running {
            if let Some(output) = self
                .command_records
                .back()
                .and_then(|record| record.output_start)
            {
                let cursor = self.current_buffer_anchor();
                return (cursor.line_id, cursor.column) < (output.line_id, output.column);
            }
        }
        false
    }

    /// Observe parsed operations even for hidden panes, before a later reset
    /// in the same input batch can erase the evidence of native takeover.
    pub(in crate::terminal) fn observe_chat_native_takeover(&mut self) {
        if self.chat_native_takeover_sequence.is_none()
            && self
                .command_records
                .back()
                .is_some_and(|record| !record.complete && record.state == CommandState::Running)
            && self.chat_native_takeover()
        {
            self.chat_native_takeover_sequence =
                self.command_records.back().map(|record| record.sequence);
        }
    }

    /// Only whole, unambiguous command rows may be indented. Same-row output,
    /// reused boundaries and missing C marks remain full-width native content.
    fn chat_command_sources(&self) -> Vec<(u64, std::ops::Range<usize>)> {
        let Some(base) = self.projection_source_base() else {
            return Vec::new();
        };
        let mut candidates = Vec::new();
        for (index, record) in self.command_records.iter().enumerate() {
            let Some(output) = record.output_start else {
                continue;
            };
            if record.prompt_start.column != 0
                || output.column != 0
                || !record.start_mark_seen
                || record
                    .command
                    .as_deref()
                    .is_none_or(|command| command.trim().is_empty())
                || !matches!(record.state, CommandState::Running | CommandState::Complete)
            {
                continue;
            }
            let (Some((start, _)), Some((end, _))) = (
                self.buffer_anchor_to_absolute(record.prompt_start),
                self.buffer_anchor_to_absolute(output),
            ) else {
                continue;
            };
            if start >= end
                || self.command_records.get(index + 1).is_some_and(|next| {
                    self.buffer_anchor_to_absolute(next.prompt_start)
                        .is_some_and(|(next, _)| end > next)
                })
            {
                continue;
            }
            candidates.push((record.sequence, base + start..base + end));
        }
        // All members of an overlapping component fail closed, rather than
        // arbitrarily choosing a command owner for the shared physical rows.
        let mut result = Vec::new();
        let mut i = 0;
        while i < candidates.len() {
            let start = i;
            let mut end = candidates[i].1.end;
            i += 1;
            while i < candidates.len() && candidates[i].1.start < end {
                end = end.max(candidates[i].1.end);
                i += 1;
            }
            if i == start + 1 {
                result.push(candidates[start].clone());
            }
        }
        result
    }

    /// Only mutable command rows affect horizontal geometry. Output arriving
    /// below them does not invalidate the entire retained transcript plan.
    pub(super) fn chat_content_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        for (sequence, source) in self.chat_command_sources() {
            sequence.hash(&mut hash);
            source.hash(&mut hash);
            let grid_base = self.total_lines_scrolled as usize;
            let start = source.start.saturating_sub(grid_base).min(self.grid.rows());
            let end = source.end.saturating_sub(grid_base).min(self.grid.rows());
            self.row_versions[start..end].hash(&mut hash);
        }
        hash.finish()
    }

    fn chat_source_content_len(&self, source: usize) -> usize {
        let Some(absolute) = self.projection_source_to_absolute(source) else {
            return 0;
        };
        if absolute < self.scrollback.len() {
            return self.scrollback[absolute].reflow_content_len();
        }
        let row = absolute - self.scrollback.len();
        if self.grid.row_wrapped[row] {
            return self.grid.row_len();
        }
        let blank = TerminalCell::default();
        self.grid[row]
            .iter()
            .rposition(|cell| {
                cell.character != blank.character
                    || cell.foreground != blank.foreground
                    || cell.background != blank.background
                    || cell.flags != blank.flags
                    || cell.hyperlink_id != blank.hyperlink_id
            })
            .map_or(0, |column| column + 1)
    }

    pub(super) fn apply_chat_plan(&self, plan: &mut ProjectionPlan, tail_keep: usize) {
        plan.chat_mode = true;
        let sources = self.chat_command_sources();
        let source_base = self.projection_source_base().unwrap_or(0);
        let prompt_sources: HashSet<_> = self
            .command_records
            .iter()
            .filter_map(|record| self.buffer_anchor_to_absolute(record.prompt_start))
            .map(|(absolute, _)| source_base + absolute)
            .collect();
        let output_starts: HashSet<_> = sources
            .iter()
            .map(|(_, source)| source.end)
            .filter(|source| !prompt_sources.contains(source))
            .collect();
        let mut rows = VecDeque::with_capacity(plan.rows.len() + output_starts.len());
        for row in plan.rows.drain(..) {
            if row.kind == ProjectedRowKind::Raw {
                if let Some(slice) = row.raw_slices.first().filter(|slice| {
                    slice.view_col_start == 0
                        && slice.source.col_start == 0
                        && output_starts.contains(&slice.source.absolute_row)
                }) {
                    if let Some(origin) = slice.origin {
                        rows.push_back(ProjectionPlanRow {
                            raw_slices: SmallVec::new(),
                            row_source: None,
                            wrapped: false,
                            kind: ProjectedRowKind::BlockSpacer {
                                before: RawCellAnchor {
                                    row_id: origin.row,
                                    column: 0,
                                },
                            },
                        });
                        plan.spacer_count += 1;
                    }
                }
            }
            rows.push_back(row);
        }
        plan.rows = rows;
        let mut command_rows = std::collections::BTreeMap::<usize, Vec<(usize, usize)>>::new();
        for (row_index, row) in plan.rows.iter().enumerate() {
            if row.kind != ProjectedRowKind::Raw {
                continue;
            }
            let Some(first_source) = row
                .raw_slices
                .first()
                .map(|slice| slice.source.absolute_row)
                .or(row.row_source.map(|source| source.raw_absolute_row))
            else {
                continue;
            };
            let Some(owner) = sources
                .partition_point(|(_, range)| range.start <= first_source)
                .checked_sub(1)
                .filter(|index| sources[*index].1.contains(&first_source))
            else {
                continue;
            };
            if row
                .raw_slices
                .iter()
                .any(|slice| !sources[owner].1.contains(&slice.source.absolute_row))
            {
                continue;
            }
            let content_end = row
                .raw_slices
                .iter()
                .map(|slice| {
                    let active = self.chat_source_content_len(slice.source.absolute_row);
                    slice.view_col_start
                        + slice.len.min(active.saturating_sub(slice.source.col_start))
                })
                .max()
                .unwrap_or(0);
            command_rows
                .entry(owner)
                .or_default()
                .push((row_index, content_end));
        }
        for (owner, rows) in command_rows {
            let first = rows[0].0;
            let last = rows.last().expect("command rows are nonempty").0;
            if rows.len() != last - first + 1 {
                continue;
            }
            let width = rows.iter().map(|(_, width)| *width).max().unwrap_or(0);
            // Two display columns are visual padding, not terminal cells. Long
            // commands deliberately retain full width instead of being cut.
            let offset = plan.cols.saturating_sub(width.saturating_add(2));
            for (index, content_end) in rows {
                let row = &mut plan.rows[index];
                row.raw_slices.retain_mut(|slice| {
                    slice.len = slice
                        .len
                        .min(content_end.saturating_sub(slice.view_col_start));
                    slice.view_col_start += offset;
                    slice.len > 0
                });
            }
            plan.chat_commands.push(ChatCommandSpan {
                sequence: sources[owner].0,
                rows: first..last + 1,
                column: offset,
            });
        }
        // Unlike Block, Chat intentionally removes all proven empty terminal
        // tail below the live composer floor. The underlying grid and PTY size
        // are untouched. Styled blanks, images, cursor and completion menus
        // were included by block_chrome_tail_keep.
        let first_trimmed_source = self.total_lines_scrolled as usize + tail_keep;
        while plan.rows.back().is_some_and(|row| {
            row.kind == ProjectedRowKind::Raw
                && row
                    .row_source
                    .is_some_and(|source| source.raw_absolute_row >= first_trimmed_source)
        }) {
            plan.rows.pop_back();
        }
        plan.raw_slice_count = plan.rows.iter().map(|row| row.raw_slices.len()).sum();
        plan.rebuild_raw_row_placements();
        if self.chat_surface().composer_docked() {
            plan.chat_dock_start = self
                .command_records
                .back()
                .filter(|record| record.prompt_start.column == 0)
                .and_then(|record| self.buffer_anchor_to_absolute(record.prompt_start))
                .and_then(|(absolute, _)| self.raw_row_id_at_absolute(absolute))
                .and_then(|row_id| plan.raw_cell_document_row(RawCellAnchor { row_id, column: 0 }));
            if let Some(start) = plan.chat_dock_start {
                if plan.document_rows().saturating_sub(start).saturating_add(1) < self.grid.rows() {
                    if let Some(before) = plan.rows[start].raw_slices.first().and_then(|slice| {
                        slice.origin.map(|origin| RawCellAnchor {
                            row_id: origin.row,
                            column: origin.col_start,
                        })
                    }) {
                        plan.rows.push_back(ProjectionPlanRow {
                            raw_slices: SmallVec::new(),
                            row_source: None,
                            wrapped: false,
                            kind: ProjectedRowKind::CompanionStatus {
                                before,
                                index: 0,
                                total: 1,
                            },
                        });
                        plan.chrome_row_count += 1;
                    }
                }
            }
        }
    }

    pub(super) fn materialize_chat_or_block_viewport(
        &self,
        plan: &ProjectionPlan,
        document_start: usize,
        viewport_rows: usize,
        cursor_row_id: RawRowId,
        scroll_offset: usize,
    ) -> ProjectedViewport {
        let mut materialized =
            self.materialize_projection_plan(plan, document_start, viewport_rows);
        let mut documents: Vec<_> = (0..viewport_rows)
            .map(|row| {
                row.checked_sub(materialized.top_padding)
                    .map(|row| document_start + row)
                    .filter(|document| *document < plan.document_rows())
            })
            .collect();
        let mut composer = None;
        if let Some(dock_start) = plan.chat_dock_start {
            let dock_rows = plan.document_rows().saturating_sub(dock_start);
            // Oversized multiline prompts/menus retain the ordinary scrolling
            // PTY view rather than cropping away editable input.
            if dock_rows > 0 && dock_rows < viewport_rows {
                let target_start = viewport_rows - dock_rows;
                let dock = self.materialize_projection_plan(plan, dock_start, dock_rows);
                materialized
                    .origins
                    .retain(|span| span.display_start.row < target_start);
                for mut origin in dock.origins {
                    origin.display_start.row += target_start;
                    materialized.origins.push(origin);
                }
                for row in 0..dock_rows {
                    let target = target_start + row;
                    materialized.cells[target].clone_from(&dock.cells[row]);
                    materialized.row_wrapped[target] = dock.row_wrapped[row];
                    materialized.row_kinds[target] = dock.row_kinds[row];
                    materialized.row_sources[target] = dock.row_sources[row];
                    documents[target] = Some(dock_start + row);
                }
                // The dock is a separate display region, not the continuation
                // of the last visible history row. Word/link expansion must
                // not join them and thereby select hidden document rows.
                if target_start > 0 {
                    materialized.row_wrapped[target_start - 1] = false;
                }
                composer = Some(target_start..viewport_rows);
            }
        }
        let cursor = materialized
            .origins
            .iter()
            .find_map(|span| {
                let end = span.raw_start.column.saturating_add(span.len);
                (span.raw_start.row_id == cursor_row_id
                    && (span.raw_start.column..end).contains(&self.cursor_col))
                .then(|| {
                    DisplayPoint::new(
                        span.display_start.row,
                        span.display_start.column + self.cursor_col - span.raw_start.column,
                    )
                })
            })
            .unwrap_or_else(|| DisplayPoint::new(usize::MAX, usize::MAX));
        let key = ProjectionCacheKey::new(
            self.grid_version,
            plan.plan_revision,
            self.total_lines_scrolled,
            self.row_identity_revision,
            self.scrollback.len(),
            scroll_offset,
            viewport_rows,
            plan.cols,
            false,
            ProjectionMode::Transformed,
        );
        let viewport = ProjectedViewport::new_transformed(
            key,
            materialized.cells,
            materialized.row_wrapped,
            materialized.row_kinds,
            materialized.row_sources,
            materialized.origins,
            cursor,
            plan.document_rows(),
            materialized.document_start,
            materialized.top_padding,
            plan.effective_collapsed.clone(),
        );
        if !plan.chat_mode {
            return viewport;
        }
        let mut commands = Vec::new();
        for command in &plan.chat_commands {
            let mut visible = documents.iter().enumerate().filter_map(|(row, document)| {
                document
                    .filter(|document| command.rows.contains(document))
                    .map(|_| row)
            });
            if let Some(first) = visible.next() {
                let last = visible.next_back().unwrap_or(first);
                commands.push(ChatCommandSpan {
                    sequence: command.sequence,
                    rows: first..last + 1,
                    column: command.column,
                });
            }
        }
        viewport.with_chat_layout(documents, commands, composer)
    }
}

impl ChatInputAction {
    pub fn is_current(self, terminal: &super::TerminalState) -> bool {
        let Some(record) = terminal.command_records.back() else {
            return false;
        };
        if terminal.chat_native_takeover() {
            return false;
        }
        match self {
            Self::Submit { sequence } => {
                sequence == record.sequence
                    && terminal.chat_surface().allows_submit_affordance()
                    && terminal.chat_pending_submit != Some(sequence)
            }
            Self::Interrupt { sequence } => {
                sequence == record.sequence
                    && terminal.chat_surface() == jterm_core::chat_mode::ChatSurface::Running
            }
        }
    }
}

impl super::TerminalState {
    pub(crate) fn note_chat_input_accepted(&mut self, action: ChatInputAction) {
        if let ChatInputAction::Submit { sequence } = action {
            self.chat_pending_submit = Some(sequence);
        }
    }
}
