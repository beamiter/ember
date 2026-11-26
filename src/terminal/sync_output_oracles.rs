//! Frozen native synchronized-output watchdog/end bodies.
use super::*;
use jterm_engine::synchronized_output::SyncEndEffect;
use std::cell::RefCell;
impl TerminalState {
    fn frozen_sync_mode(&mut self, enabled: bool) {
        if enabled {
            self.vt.protocol.modes_mut().insert(2026);
            self.vt.sync_output.active = true;
            self.vt.sync_output.started = Some(self.sync_output_clock.now());
        } else {
            observe(self, SyncEndEffect::Archive);
            if self.vt.screen.use_alt_buffer {
                self.archive_visible_screen_to_scrollback_with_options(true, true);
            } else {
                self.last_synced_primary_screen_snapshot =
                    self.visible_screen_snapshot().unwrap_or_default();
            }
            self.vt.protocol.modes_mut().remove(&2026);
            self.vt.sync_output.active = false;
            self.vt.sync_output.started = None;
            observe(self, SyncEndEffect::Dirty);
            self.dirty_region.mark_all(self.vt.screen.grid.rows());
            self.mark_rows_dirty(0, self.vt.screen.grid.rows().saturating_sub(1));
        }
    }

    fn frozen_sync_timeout(&mut self) {
        if self.vt.sync_output.active {
            if let Some(start) = self.vt.sync_output.started {
                if self
                    .sync_output_clock
                    .now()
                    .saturating_duration_since(start)
                    > std::time::Duration::from_secs(1)
                {
                    observe(self, SyncEndEffect::Archive);
                    if self.vt.screen.use_alt_buffer {
                        self.archive_visible_screen_to_scrollback_with_options(true, true);
                    } else {
                        self.last_synced_primary_screen_snapshot =
                            self.visible_screen_snapshot().unwrap_or_default();
                    }
                    self.vt.sync_output.active = false;
                    self.vt.sync_output.started = None;
                    self.vt.protocol.modes_mut().remove(&2026);
                    observe(self, SyncEndEffect::Dirty);
                    self.dirty_region.mark_all(self.vt.screen.grid.rows());
                    self.mark_rows_dirty(0, self.vt.screen.grid.rows().saturating_sub(1));
                }
            }
        }
    }
}

type Snapshot = std::collections::BTreeMap<String, String>;
thread_local! { static TRACE: RefCell<Option<Vec<(SyncEndEffect,Snapshot)>>> = const { RefCell::new(None) }; }
fn snapshot(t: &TerminalState) -> Snapshot {
    let mut s = super::super::alternate_parity_tests::snapshot(t);
    s.insert("sync-active".into(), t.vt.sync_output.active.to_string());
    s.insert("sync-start".into(), format!("{:?}", t.vt.sync_output.started));
    s
}
pub(super) fn observe(t: &TerminalState, effect: SyncEndEffect) {
    if TRACE.with(|v| v.borrow().is_none()) {
        return;
    }
    let s = snapshot(t);
    TRACE.with(|v| v.borrow_mut().as_mut().unwrap().push((effect, s)));
}
fn capture(f: impl FnOnce()) -> Vec<(SyncEndEffect, Snapshot)> {
    TRACE.with(|v| *v.borrow_mut() = Some(vec![]));
    f();
    TRACE.with(|v| v.borrow_mut().take().unwrap())
}
#[test]
fn sync_owner_matches_frozen_ember_boundaries_and_callback_state() {
    use jterm_engine::mode_mutation::NativeModeEffect;
    for alt in [false, true] {
        for inline in [false, true] {
            let clock = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
            let make = || {
                let mut t = TerminalState::new(7, 3);
                t.process_input(b"old\r\nhistory\r\nmore\r\nlast");
                if alt {
                    t.enter_alt_screen(true, true);
                }
                if inline {
                    t.set_inline_cli_launch(true);
                }
                let clock = clock.clone();
                t.sync_output_clock =
                    super::super::SyncOutputClock::new(move || *clock.lock().unwrap());
                t
            };
            let mut a = make();
            let mut e = make();
            for enabled in [false, true, true, false, true] {
                let x = capture(|| {
                    a.apply_mode_effect(NativeModeEffect::SynchronizedOutput { enabled })
                });
                let y = capture(|| e.frozen_sync_mode(enabled));
                assert_eq!(x, y);
                assert_eq!(snapshot(&a), snapshot(&e));
                *clock.lock().unwrap() += std::time::Duration::from_millis(900);
            }
            let start = a.vt.sync_output.started.unwrap();
            for age in [
                std::time::Duration::from_millis(999),
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(1) + std::time::Duration::from_nanos(1),
            ] {
                *clock.lock().unwrap() = start + age;
                let x = capture(|| a.check_sync_output_timeout());
                let y = capture(|| e.frozen_sync_timeout());
                assert_eq!(x, y);
                assert_eq!(snapshot(&a), snapshot(&e));
            }
        }
    }
}
