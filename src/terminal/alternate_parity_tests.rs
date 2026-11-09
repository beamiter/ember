//! Independent pre-migration host algorithms, frozen before adapter changes.
use super::*;
use jterm_engine::screen::SavedCursorState;

impl TerminalState {
    /// 擦除屏幕并把光标归位到左上角。供切换备用缓冲区等场景使用。
    fn reference_clear_screen(&mut self) {
        let bg_color = self.screen.current_bg;
        let cols = self.screen.grid.row_len();
        let spans: Vec<_> = (0..self.screen.grid.rows())
            .map(|row| (row, 0, cols))
            .collect();
        self.invalidate_grid_mutation_spans(&spans);
        for row in self.screen.grid.iter_mut() {
            for cell in row.iter_mut() {
                *cell = TerminalCell {
                    character: ' ',
                    foreground: Color::Default,
                    background: bg_color,
                    flags: StyleFlags::default(),
                    hyperlink_id: HyperlinkId::NONE,
                };
            }
        }
        self.dirty_region.mark_all(self.screen.grid.rows());
        self.mark_rows_dirty(0, self.screen.grid.rows().saturating_sub(1));
        self.kitty_graphics.clear_placements();
        self.screen.cursor_row = 0;
        self.screen.cursor_col = 0;
    }

    /// 切入备用屏幕缓冲。`save_cursor` 控制是否保存主屏光标(1049/1048 语义),
    /// `clear` 控制切入后是否清屏(1047/1049 语义)。已在备用屏则无操作。
    fn reference_enter_alt_screen(&mut self, save_cursor: bool, clear: bool) {
        if self.screen.use_alt_buffer {
            return;
        }
        // A selection belongs to the screen on which it was made. Keeping its
        // absolute cell coordinates while swapping buffers paints an unrelated
        // rectangle at the same viewport position in Vim/less.
        self.selection = None;
        if save_cursor {
            self.screen.saved_cursor_row = self.screen.cursor_row;
            self.screen.saved_cursor_col = self.screen.cursor_col;
            // Snapshot the whole drawing state, not just the cursor: exiting
            // restores it instead of zeroing SGR.
            self.screen.saved_primary_screen_state = Some(SavedCursorState {
                row: self.screen.cursor_row,
                col: self.screen.cursor_col,
                fg: self.screen.current_fg,
                bg: self.screen.current_bg,
                flags: self.screen.current_flags,
                g0: self.screen.g0_charset,
                g1: self.screen.g1_charset,
                active: self.screen.active_charset,
                origin_mode: self.screen.origin_mode,
                autowrap: self.protocol.modes().contains(&7),
                pending_wrap: self.screen.pending_wrap,
            });
        }
        self.screen.saved_primary_cursor_shape = Some(self.screen.cursor_shape);
        // 备用屏不显示 scrollback
        self.scroll_offset = 0;
        std::mem::swap(&mut self.screen.grid, &mut self.screen.alt_grid);
        self.mark_row_identity_changed();
        self.kitty_graphics.switch_screen();
        self.screen.alt_cursor_row = self.screen.cursor_row;
        self.screen.alt_cursor_col = self.screen.cursor_col;
        self.protocol.switch_screen();
        self.watch.boundary();
        self.invalidate_search_scrollback_seals();
        self.screen.use_alt_buffer = true;
        self.organism_working_dir = None;
        // Retained cells keep their own keys, but an unterminated primary-screen
        // OSC 8 link must not arm alternate-screen text.
        self.screen.current_hyperlink = HyperlinkId::NONE;
        // DECSTBM is per-buffer. Carrying the primary screen's partial scroll
        // region into the alternate buffer makes a TUI that never issues its
        // own DECSTBM draw inside somebody else's region.
        self.screen.scroll_region_top = 0;
        self.screen.scroll_region_bottom = self.screen.grid.rows().saturating_sub(1);
        if clear {
            self.reference_clear_screen();
            // ED2 only removes placements intersecting the visible viewport so
            // primary-screen scrollback survives. A freshly entered alternate
            // buffer has no scrollback and must discard every placement left
            // from its previous use, including any off-screen residue.
            self.kitty_graphics.clear_current_screen_placements();
        }
    }

    /// 切回主屏幕缓冲。`restore_cursor` 控制是否恢复进入备用屏前保存的光标(1049 语义)。
    /// 不在备用屏则无操作。
    fn reference_exit_alt_screen(&mut self, restore_cursor: bool) {
        if !self.screen.use_alt_buffer {
            return;
        }
        self.selection = None;
        self.screen.alt_cursor_row = self.screen.cursor_row;
        self.screen.alt_cursor_col = self.screen.cursor_col;
        std::mem::swap(&mut self.screen.grid, &mut self.screen.alt_grid);
        self.mark_row_identity_changed();
        self.kitty_graphics.switch_screen();
        if restore_cursor {
            self.screen.cursor_row = self.screen.saved_cursor_row;
            self.screen.cursor_col = self.screen.saved_cursor_col;
        }
        self.protocol.switch_screen();
        self.watch.boundary();
        self.invalidate_search_scrollback_seals();
        self.screen.use_alt_buffer = false;
        // A TUI's DECSCUSR (vim's insert-mode beam) belongs to its screen.
        if let Some(shape) = self.screen.saved_primary_cursor_shape.take() {
            self.screen.cursor_shape = shape;
        }
        // Alt-screen OSC 8 state never leaks back onto newly printed
        // primary-screen cells, and the alt buffer's scroll region does not
        // carry back into the main one.
        self.screen.current_hyperlink = HyperlinkId::NONE;
        self.screen.scroll_region_top = 0;
        self.screen.scroll_region_bottom = self.screen.grid.rows().saturating_sub(1);

        // Restore the primary screen's drawing state rather than zeroing it:
        // the shell's own SGR, charsets and modes were in force before the
        // fullscreen app started and must be in force again after it exits.
        // Falling back to Default keeps 1047-style entries (no save) safe.
        if let Some(saved) = self.screen.saved_primary_screen_state.take() {
            self.screen.current_fg = saved.fg;
            self.screen.current_bg = saved.bg;
            self.global_bg = saved.bg;
            self.screen.current_flags = saved.flags;
            self.screen.g0_charset = saved.g0;
            self.screen.g1_charset = saved.g1;
            self.screen.active_charset = saved.active;
            self.screen.origin_mode = saved.origin_mode;
            if saved.autowrap {
                self.protocol.modes_mut().insert(7);
            } else {
                self.protocol.modes_mut().remove(&7);
            }
        } else {
            self.screen.current_fg = Color::Default;
            self.screen.current_bg = Color::Default;
            self.global_bg = Color::Default;
            self.screen.current_flags = StyleFlags::default();
        }
        self.screen.pending_wrap = false;

        // 交换缓冲后强制整屏重绘(+rows+1 触发 ui.rs 的 grid_version_jumped)
        self.grid_version += self.screen.grid.rows() as u64 + 1;
        self.row_versions.fill(self.grid_version);
        self.dirty_region.mark_all(self.screen.grid.rows());
    }
}

pub(super) fn snapshot(t: &TerminalState) -> std::collections::BTreeMap<String, String> {
    let mut out = super::structural_parity_tests::snapshot(t);
    macro_rules! host {
        ($($field:ident),* $(,)?) => {$(out.insert(stringify!($field).into(), format!("{:?}", t.$field));)*};
    }
    host!(protocol, watch, global_bg, organism_working_dir);
    macro_rules! screen {
        ($($field:ident),* $(,)?) => {$(out.insert(stringify!($field).into(), format!("{:?}", t.screen.$field));)*};
    }
    screen!(
        saved_cursor_row,
        saved_cursor_col,
        alt_cursor_row,
        alt_cursor_col,
        cursor_shape,
        saved_primary_cursor_shape,
        saved_state,
        saved_primary_screen_state,
        g0_charset,
        g1_charset,
        active_charset,
        origin_mode,
        insert_mode,
        last_printed_char,
        tab_stops
    );
    out
}

pub(super) fn seeded(seed: usize) -> TerminalState {
    let mut t = super::structural_parity_tests::seeded();
    t.process_input(b"\x1b[1;1H\x1b]133;A\x07\x1b]133;C;id=finished\x07AA\x1b]133;D;0;id=finished\x07\x1b[2;1H\x1b]133;A\x07\x1b]133;C;id=active\x07BB");
    t.process_input(b"\x1b[1;1H\x1b_Ga=T,i=41,f=32,s=1,v=1,r=2,c=1,C=1;/wAA/w==\x1b\\");
    // Seed a distinct hidden placement too, to detect clearing the wrong screen.
    t.kitty_graphics.switch_screen();
    t.process_input(b"\x1b[3;2H\x1b_Ga=p,i=41,p=2,r=2,c=1,C=1\x1b\\");
    t.kitty_graphics.switch_screen();
    t.selection = Some(Selection {
        anchor: (0, 0),
        active: (2, 3),
        mode: SelectionMode::Normal,
    });
    t.screen.cursor_row = seed % 5;
    t.screen.cursor_col = seed % 6;
    t.screen.saved_cursor_row = (seed + 1) % 5;
    t.screen.saved_cursor_col = (seed + 1) % 6;
    t.screen.pending_wrap = seed & 1 != 0;
    t.screen.origin_mode = seed & 2 != 0;
    t.screen.g0_charset = Charset::DecSpecialGraphics;
    t.screen.current_hyperlink = HyperlinkId::from_raw(17);
    t.screen.scroll_region_top = 1;
    t.screen.scroll_region_bottom = 3;
    t.screen.current_fg = Color::Indexed(seed as u8);
    t.screen.current_flags.set_bold(true);
    t.screen.current_flags.set_protected(true);
    t.scroll_offset = seed % 3;
    t.organism_working_dir = Some("/alternate-parity".into());
    t.protocol.push_keyboard_enhancement_flags(seed as u16);
    if seed & 4 != 0 {
        t.protocol.modes_mut().remove(&7);
    }
    t.watch.start(1);
    super::structural_parity_tests::prime(&mut t);
    t
}

fn transition(t: &mut TerminalState, operation: usize, reference: bool) {
    match operation % 6 {
        0..=3 => {
            let save = operation & 1 != 0;
            let clear = operation & 2 != 0;
            if reference {
                t.reference_enter_alt_screen(save, clear);
            } else {
                t.enter_alt_screen(save, clear);
            }
        }
        _ => {
            if reference {
                t.reference_exit_alt_screen(operation & 1 != 0);
            } else {
                t.exit_alt_screen(operation & 1 != 0);
            }
        }
    }
}

#[test]
fn alternate_transition_independent_host_snapshots() {
    // 422 comparisons of production state against the frozen host algorithm.
    // Repeated entry must not clear/save; mixed 47/1047/1049 exits restore the
    // dedicated primary snapshot without replacing the shared DECSC slot.
    for seed in 0..211 {
        let mut actual = seeded(seed);
        let mut reference = seeded(seed);
        for step in 0..2 {
            let operation = if step == 0 { seed % 4 } else { seed % 6 };
            for t in [&mut actual, &mut reference] {
                // Repeated entry/exit must retain a newly made selection.
                t.selection = Some(Selection {
                    anchor: (0, 1),
                    active: (1, 2),
                    mode: SelectionMode::Block,
                });
            }
            let actual_epoch = actual.search_scrollback_epoch;
            let reference_epoch = reference.search_scrollback_epoch;
            transition(&mut actual, operation, false);
            transition(&mut reference, operation, true);
            assert_eq!(
                actual.search_scrollback_epoch != actual_epoch,
                reference.search_scrollback_epoch != reference_epoch,
                "seal invalidation seed={seed}, step={step}"
            );
            assert_eq!(
                snapshot(&actual),
                snapshot(&reference),
                "seed={seed}, step={step}"
            );
        }
    }
}

#[test]
fn alternate_transition_old_algorithm_sequences() {
    // Independent host controls include graphics/keyboard ownership, dirty
    // revisions, watch boundaries, row identities and cached projections.
    for seed in 0..250 {
        let mut actual = seeded(seed);
        let mut reference = seeded(seed);
        let mut random = seed as u64 + 1;
        for step in 0..48 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let operation = (random >> 32) as usize % 6;
            for t in [&mut actual, &mut reference] {
                // Repeated entry/exit must retain a newly made selection.
                t.selection = Some(Selection {
                    anchor: (0, 1),
                    active: (1, 2),
                    mode: SelectionMode::Block,
                });
            }
            let actual_epoch = actual.search_scrollback_epoch;
            let reference_epoch = reference.search_scrollback_epoch;
            transition(&mut actual, operation, false);
            transition(&mut reference, operation, true);
            assert_eq!(
                actual.search_scrollback_epoch != actual_epoch,
                reference.search_scrollback_epoch != reference_epoch,
                "seal invalidation seed={seed}, step={step}"
            );
            assert_eq!(
                snapshot(&actual),
                snapshot(&reference),
                "seed={seed}, step={step}, op={operation}"
            );
            // Exercise drawing, DECSC/DECRC, hyperlinks, BCE, margins and
            // keyboard-stack updates between transitions; RIS stays host-owned.
            let bytes: &[u8] = match step % 8 {
                0 => b"\x1b[31;44mX\x1b7",
                1 => b"\x1b[?7l\x1b[2;4r\x1b[3;4H",
                2 => b"\x1b[>3u\x1b[5 q",
                3 => b"\x1b]8;;https://example.test\x07link",
                4 => b"\x1b(0\x1b[?6h\x1b8",
                5 => b"\x1b[2J\x1b[<u",
                6 => b"\x1bc",
                _ => b"\x1b[?7h\x1b[4;6H!",
            };
            actual.process_input(bytes);
            reference.process_input(bytes);
            assert_eq!(
                snapshot(&actual),
                snapshot(&reference),
                "after input seed={seed}, step={step}"
            );
        }
    }
}
