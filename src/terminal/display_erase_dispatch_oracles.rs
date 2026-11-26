//! Frozen pre-extraction J admission and per-row product-effect ordering.
//! Existing recorded fixtures are consumed separately, never regenerated here.
use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

type Snapshot = BTreeMap<String, String>;
type Trace = Vec<(String, Snapshot)>;
thread_local! { static TRACE: RefCell<Option<Trace>> = const { RefCell::new(None) }; }

pub(crate) fn observe(t: &TerminalState, effect: &RowMutation) {
    TRACE.with(|slot| {
        if let Some(trace) = slot.borrow_mut().as_mut() {
            trace.push((
                format!("{:?}", (effect.row, &effect.changed, &effect.printed)),
                snapshot(t),
            ));
        }
    });
}

fn snapshot(t: &TerminalState) -> Snapshot {
    let mut result = super::super::alternate_parity_tests::snapshot(t);
    result.insert(
        "chat_native_takeover_sequence".into(),
        format!("{:?}", t.chat_native_takeover_sequence),
    );
    result.insert(
        "chat_pending_submit".into(),
        format!("{:?}", t.chat_pending_submit),
    );
    result.insert(
        "inline_cli_launch_floor".into(),
        format!("{:?}", t.inline_cli_launch_floor),
    );
    result.insert(
        "inline_cli_command_scope".into(),
        format!("{:?}", t.inline_cli_command_scope),
    );
    result
}

impl TerminalState {
    fn frozen_display_erase_dispatch(
        &mut self,
        params: &[u16],
        private_prefix: Option<u8>,
        intermediates: &[u8],
    ) {
        let cmd = 'J';
        match cmd {
            'J' if intermediates.is_empty() && matches!(private_prefix, None | Some(b'?')) => {
                let selective = private_prefix == Some(b'?');
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        if !selective && self.vt.screen.cursor_row == 0 && self.vt.screen.cursor_col == 0
                        {
                            self.inline_cli_screen_erased();
                        }
                        // Clear from cursor to end of display
                        // ClearRight cancels wrap even if the tail is protected.
                        self.vt.screen.pending_wrap = false;
                        let cols = self.vt.screen.grid.row_len();
                        self.erase_grid_span(
                            self.vt.screen.cursor_row,
                            self.vt.screen.cursor_col,
                            cols,
                            selective,
                        );
                        for row in (self.vt.screen.cursor_row + 1)..self.vt.screen.grid.rows() {
                            self.erase_grid_span(row, 0, cols, selective);
                        }
                    }
                    1 => {
                        // Clear from start to cursor
                        let cols = self.vt.screen.grid.row_len();
                        for row in 0..=self.vt.screen.cursor_row {
                            let end_col = if row == self.vt.screen.cursor_row {
                                self.vt.screen.cursor_col + 1
                            } else {
                                cols
                            };
                            self.erase_grid_span(row, 0, end_col, selective);
                        }
                    }
                    2 if selective => {
                        // DECSED preserves protected text and does not archive
                        // a partially retained screen or delete Kitty images.
                        for row in 0..self.vt.screen.grid.rows() {
                            self.erase_grid_span(row, 0, self.vt.screen.grid.row_len(), true);
                        }
                    }
                    2 => {
                        // ED 擦除显示不移动光标(VT 规范)
                        if self.vt.sync_output.active {
                            if self.vt.screen.use_alt_buffer {
                                self.archive_visible_screen_to_scrollback_with_options(true, true);
                            } else {
                                self.archive_primary_screen_unless_last_synced_snapshot();
                            }
                        } else {
                            self.archive_visible_screen_to_scrollback();
                        }
                        self.inline_cli_screen_erased();
                        self.erase_screen();
                    }
                    3 if self.vt.screen.use_alt_buffer && self.inline_cli_owner_active() => {
                        // A temporary CLI screen cannot erase its primary
                        // conversation's saved lines.
                    }
                    3 => {
                        // Clear scrollback buffer (xterm extension)
                        self.clear_scrollback_with_projection_provenance();
                        self.invalidate_scrollback_view_cache();
                        self.kitty_graphics.clear_scrollback_placements();
                        self.scroll_offset = 0;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        self.observe_chat_native_takeover();
    }
}

fn seeded(seed: usize, pending: bool, protected: bool) -> TerminalState {
    let mut t = super::super::alternate_parity_tests::seeded(seed);
    if seed & 1 != 0 {
        t.process_input(b"\x1b[?1049h");
    }
    if seed & 2 != 0 {
        t.set_inline_cli_launch(true);
    }
    for row in 0..5 {
        for col in 0..6 {
            let cell = t.vt.screen.grid.get_mut(row, col);
            cell.character = if col == 1 {
                '界'
            } else if col == 2 {
                ' '
            } else {
                'X'
            };
            cell.flags.set_wide(col == 1);
            cell.flags.set_wide_continuation(col == 2);
            cell.flags.set_protected(
                protected
                    || if col == 1 || col == 2 {
                        row & 1 == 0
                    } else {
                        col & 1 == 0
                    },
            );
        }
    }
    t.vt.screen.cursor_row = seed % 5;
    t.vt.screen.cursor_col = seed % 6;
    t.vt.screen.pending_wrap = pending;
    t.chat_native_takeover_sequence = None;
    t
}

fn run(
    mut t: TerminalState,
    params: &[u16],
    prefix: Option<u8>,
    intermediates: &[u8],
    original: bool,
) -> (Snapshot, Trace) {
    TRACE.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
    if original {
        t.frozen_display_erase_dispatch(params, prefix, intermediates);
    } else {
        t.handle_escape_sequence(params, 'J', prefix, intermediates);
    }
    let trace = TRACE.with(|slot| slot.borrow_mut().take().unwrap());
    (snapshot(&t), trace)
}

#[test]
fn display_erase_admission_mutations_and_order_match_frozen_host() {
    for seed in [0, 3, 7] {
        for pending in [false, true] {
            for protected in [false, true] {
                for params in [
                    vec![],
                    vec![0],
                    vec![1],
                    vec![2],
                    vec![3],
                    vec![9],
                    vec![0, 3],
                ] {
                    for prefix in [None, Some(b'?'), Some(b'>'), Some(b'=')] {
                        for intermediates in [b"".as_slice(), b"!", b"$"] {
                            let params = params.as_slice();
                            assert_eq!(run(seeded(seed, pending, protected), params, prefix, intermediates, false), run(seeded(seed, pending, protected), params, prefix, intermediates, true), "seed={seed} pending={pending} protected={protected} params={params:?} prefix={prefix:?} intermediates={intermediates:?}");
                        }
                    }
                }
            }
        }
    }
}
