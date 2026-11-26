//! Host effects at the shared engine alternate-screen boundaries.
use super::*;

impl jterm_engine::native_alternate::NativeAlternateHost for TerminalState {
    fn alternate_state(&mut self) -> jterm_engine::native_alternate::NativeAlternateState<'_> {
        jterm_engine::native_alternate::NativeAlternateState {
            screen: &mut self.vt.screen,
            protocol: &mut self.vt.protocol,
            global_background: &mut self.global_bg,
        }
    }

    fn alternate_hook(&mut self, hook: jterm_engine::native_alternate::NativeAlternateHook) {
        use jterm_engine::native_alternate::NativeAlternateHook as Hook;
        match hook {
            Hook::PrepareEntry => {
                self.selection = None;
                self.scroll_offset = 0;
            }
            Hook::PrepareExit => self.selection = None,
            Hook::GridSwapped => {
                self.mark_row_identity_changed();
                self.kitty_graphics.switch_screen();
            }
            Hook::OwnershipChanged { entering } => {
                self.watch.boundary();
                self.invalidate_search_scrollback_seals();
                if entering {
                    self.organism_working_dir = None;
                }
            }
            Hook::EntryCleared { rows } => {
                let cols = self.vt.screen.grid.row_len();
                let spans: Vec<_> = rows.clone().map(|row| (row, 0, cols)).collect();
                self.invalidate_grid_mutation_spans(&spans);
                self.dirty_region.mark_all(self.vt.screen.grid.rows());
                self.mark_rows_dirty(rows.start, rows.end.saturating_sub(1));
                self.kitty_graphics.clear_placements();
                self.kitty_graphics.clear_current_screen_placements();
            }
            Hook::Repaint { rows } => {
                self.grid_version += rows.len() as u64 + 1;
                self.row_versions.fill(self.grid_version);
                self.dirty_region.mark_all(self.vt.screen.grid.rows());
            }
            Hook::ClearSelection | Hook::ClearAlternate | Hook::FinalizeAlternate => {
                unreachable!("Frost-only alternate-screen hook")
            }
        }
    }
}
