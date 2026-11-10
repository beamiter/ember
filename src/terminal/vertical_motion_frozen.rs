//! Original published vertical algorithms, supplemental only.
use super::*;
impl TerminalState {
    pub(in crate::terminal) fn frozen_vertical_line_feed(&mut self) {
        self.screen.pending_wrap = false;
        let departed_row_id = self.screen.grid.row_id(self.screen.cursor_row);
        let departed_boundary = BufferAnchor {
            line_id: self
                .total_lines_scrolled
                .saturating_add(self.screen.cursor_row as u64),
            column: self.screen.grid.row_len(),
        };
        if self.screen.cursor_row == self.screen.scroll_region_bottom {
            // 恰在滚动区底边距:向上滚动区域,光标保持在底行
            self.frozen_vertical_scroll_region_up(
                self.screen.scroll_region_top,
                self.screen.scroll_region_bottom,
            );
        } else if self.screen.cursor_row + 1 < self.screen.grid.rows() {
            // 区内或区外(底边距下方)正常下移,不滚动
            self.screen.cursor_row += 1;
        }
        let next_boundary = BufferAnchor {
            line_id: self
                .total_lines_scrolled
                .saturating_add(self.screen.cursor_row as u64),
            column: 0,
        };
        self.note_output_hard_line_advance(departed_row_id, departed_boundary, next_boundary);
    }
    pub(in crate::terminal) fn frozen_vertical_wrap_to_next_line(&mut self) {
        let departed_row_id = self.screen.grid.row_id(self.screen.cursor_row);
        self.screen.grid.row_wrapped[self.screen.cursor_row] = true;
        self.screen.cursor_col = 0;
        if self.screen.cursor_row == self.screen.scroll_region_bottom {
            self.frozen_vertical_scroll_region_up(
                self.screen.scroll_region_top,
                self.screen.scroll_region_bottom,
            );
        } else if self.screen.cursor_row + 1 < self.screen.grid.rows() {
            self.screen.cursor_row += 1;
        }
        self.rebase_active_output_start_after_initial_wrap(departed_row_id);
    }
    pub(in crate::terminal) fn frozen_vertical_scroll_region_up(
        &mut self,
        top: usize,
        bottom: usize,
    ) {
        // Real primary top-margin transfers remain eligible during an inline
        // CLI session. Alternate rows transfer only during sync output without
        // an inline owner; copied frame archival has its separate host policy.
        let allow_alt_scrollback = self.screen.use_alt_buffer
            && self.sync_output_active
            && !self.inline_cli_owner_active();
        let transfer_outgoing = top == 0 && (!self.screen.use_alt_buffer || allow_alt_scrollback);
        let next = &mut self.next_raw_row_id;
        let Some(effect) = self.screen.scroll_region_one(
            RegionDirection::Up,
            top,
            bottom,
            transfer_outgoing,
            || allocate_raw_row_id_from(next),
        ) else {
            return;
        };
        self.apply_region_identity_effects(&effect);
        self.kitty_graphics.scroll_region_up(
            effect.top,
            effect.bottom,
            effect.count,
            transfer_outgoing,
        );
        self.dirty_region.mark_rows(effect.top, effect.bottom);
        self.mark_rows_dirty(effect.top, effect.bottom);
        for removed in effect.removed {
            if let Some(line) = removed.captured {
                // Consume a removed row with its original identity. Never route
                // this through the copied-snapshot helper that allocates an ID.
                self.push_scrollback_transferred_with_options(line, allow_alt_scrollback);
                if !self.screen.use_alt_buffer
                    && bottom + 1 == self.screen.grid.rows()
                    && self.full_screen_scroll_revision != 0
                {
                    self.full_screen_scroll_revision =
                        self.full_screen_scroll_revision.checked_add(1).unwrap_or(0);
                }
            }
        }
    }
    pub(in crate::terminal) fn frozen_vertical_scroll_region_down(
        &mut self,
        top: usize,
        bottom: usize,
    ) {
        let next = &mut self.next_raw_row_id;
        let Some(effect) =
            self.screen
                .scroll_region_one(RegionDirection::Down, top, bottom, false, || {
                    allocate_raw_row_id_from(next)
                })
        else {
            return;
        };
        self.apply_region_identity_effects(&effect);
        self.kitty_graphics
            .scroll_region_down(effect.top, effect.bottom, effect.count);
        self.dirty_region.mark_rows(effect.top, effect.bottom);
        self.mark_rows_dirty(effect.top, effect.bottom);
    }
    pub(in crate::terminal) fn frozen_vertical_reverse_index(&mut self) {
        self.screen.pending_wrap = false;
        if self.screen.cursor_row == self.screen.scroll_region_top {
            if self.screen.scroll_region_bottom < self.screen.grid.rows()
                && self.screen.scroll_region_top <= self.screen.scroll_region_bottom
            {
                self.frozen_vertical_scroll_region_down(
                    self.screen.scroll_region_top,
                    self.screen.scroll_region_bottom,
                );
            }
        } else {
            self.screen.cursor_row = self.screen.cursor_row.saturating_sub(1);
        }
    }
}
