//! Original structural CSI sequencing uses unchanged test-only native methods.
//! RegionOperation is never used by the expected path. Record the live rows
//! at identity-effect entry to catch mutation aggregation across callbacks.
use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
type Snapshot = BTreeMap<String, String>;
type Trace = Vec<(String, Snapshot)>;
thread_local! { static TRACE: RefCell<Option<Trace>> = const { RefCell::new(None) }; }

fn snapshot(t: &TerminalState) -> Snapshot {
    let mut out = super::super::alternate_parity_tests::snapshot(t);
    out.insert(
        "chat_native_takeover_sequence".into(),
        format!("{:?}", t.chat_native_takeover_sequence),
    );
    out.insert(
        "inline_cli_launch_floor".into(),
        format!("{:?}", t.inline_cli_launch_floor),
    );
    out.insert(
        "inline_cli_command_scope".into(),
        format!("{:?}", t.inline_cli_command_scope),
    );
    out
}

pub(super) fn observe(t: &TerminalState, effect: &RegionMutation) {
    TRACE.with(|slot| {
        if let Some(trace) = slot.borrow_mut().as_mut() {
            trace.push((
                format!(
                    "{:?}",
                    (effect.direction, effect.top, effect.bottom, effect.count)
                ),
                snapshot(t),
            ));
        }
    });
}

impl TerminalState {
    fn frozen_region_dispatch(&mut self, params: &[u16], cmd: char) {
        let count = params.first().copied().unwrap_or(1).max(1) as usize;
        match cmd {
            'L' => {
                self.edit_grid_lines(RegionDirection::Down, count);
                self.mark_rows_dirty(self.vt.screen.cursor_row, self.vt.screen.scroll_region_bottom);
            }
            'M' => {
                self.edit_grid_lines(RegionDirection::Up, count);
                self.mark_rows_dirty(self.vt.screen.cursor_row, self.vt.screen.scroll_region_bottom);
            }
            'S' => {
                // SU — Scroll Up. Cap to region height: more would just blank
                // an already-blank region while doing O(rows) work each step.
                let n = count;
                let region_height = self
                    .vt.screen
                    .scroll_region_bottom
                    .saturating_sub(self.vt.screen.scroll_region_top)
                    + 1;
                let n = n.min(region_height);
                for _ in 0..n {
                    self.scroll_region_up(
                        self.vt.screen.scroll_region_top,
                        self.vt.screen.scroll_region_bottom,
                    );
                }
            }
            'T' => {
                // SD — Scroll Down. Same cap as SU.
                let n = count;
                let region_height = self
                    .vt.screen
                    .scroll_region_bottom
                    .saturating_sub(self.vt.screen.scroll_region_top)
                    + 1;
                let n = n.min(region_height);
                for _ in 0..n {
                    self.scroll_region_down(
                        self.vt.screen.scroll_region_top,
                        self.vt.screen.scroll_region_bottom,
                    );
                }
            }
            _ => {}
        }
        self.observe_chat_native_takeover();
    }
}

fn seeded(context: usize, geometry: usize, cursor: usize) -> TerminalState {
    let mut t = super::super::alternate_parity_tests::seeded(context);
    if context & 1 != 0 {
        t.process_input(b"\x1b[?1049h");
    }
    if context & 2 != 0 {
        t.vt.sync_output.active = true;
    }
    if context & 4 != 0 {
        t.set_inline_cli_launch(true);
    }
    let (top, bottom) = [(0, 4), (1, 3), (2, 2), (3, 1), (0, 7)][geometry];
    t.vt.screen.scroll_region_top = top;
    t.vt.screen.scroll_region_bottom = bottom;
    t.vt.screen.cursor_row = cursor;
    t.vt.screen.pending_wrap = true;
    t.scroll_offset = 1.min(t.scrollback.len());
    t.chat_native_takeover_sequence = None;
    t
}

fn run(
    mut t: TerminalState,
    params: &[u16],
    cmd: char,
    prefix: Option<u8>,
    intermediate: &[u8],
    original: bool,
) -> (Snapshot, Trace) {
    TRACE.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
    if original {
        t.frozen_region_dispatch(params, cmd);
    } else {
        t.handle_escape_sequence(params, cmd, prefix, intermediate);
    }
    let trace = TRACE.with(|slot| slot.borrow_mut().take().unwrap());
    (snapshot(&t), trace)
}

#[test]
fn region_ops_match_frozen_dispatch_effect_order_and_host_state() {
    for context in [0, 1, 2, 3, 7] {
        for geometry in 0..5 {
            for cursor in [0, 2, 4] {
                for cmd in ['L', 'M', 'S', 'T'] {
                    for params in [vec![], vec![0], vec![1], vec![2], vec![u16::MAX]] {
                        for (prefix, intermediate) in [
                            (None, b"".as_slice()),
                            (Some(b'?'), b""),
                            (Some(b'>'), b"! "),
                        ] {
                            assert_eq!(run(seeded(context, geometry, cursor), &params, cmd, prefix, intermediate, false), run(seeded(context, geometry, cursor), &params, cmd, prefix, intermediate, true), "context={context} geometry={geometry} cursor={cursor} cmd={cmd} params={params:?} prefix={prefix:?} intermediate={intermediate:?}");
                        }
                    }
                }
            }
        }
    }
}
