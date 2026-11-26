//! A synchronous scope around exactly one native WGPU presentation.
//! `true` requires VK_KHR_present_wait success for the ID attached to that
//! presentation. Queue submission, resource fences and timeouts never succeed.
//! This is producer publication evidence, not compositor/physical scanout.
use core::cell::Cell;

#[derive(Clone, Copy, Default)]
struct State {
    calls: u32,
    complete: bool,
    submitted: bool,
    wait_for_completion: bool,
}
std::thread_local! { static SCOPE: Cell<Option<State>> = const { Cell::new(None) }; }

/// Run a single native present. Unknown/unsupported/failed/multiple presents
/// return false. A bounded driver wait is enabled only inside this scope.
pub fn observe<T>(present: impl FnOnce() -> T) -> (T, bool) {
    observe_kind(present, true)
}
/// Observe one successful vkQueuePresentKHR call only. This is NOT completed
/// presentation: Wayland callers still require the exact surface's real
/// wp_presentation presented event before acknowledging a native frame.
pub fn observe_submission<T>(present: impl FnOnce() -> T) -> (T, bool) {
    observe_kind(present, false)
}
fn observe_kind<T>(present: impl FnOnce() -> T, wait_for_completion: bool) -> (T, bool) {
    struct Reset(Option<State>);
    impl Drop for Reset {
        fn drop(&mut self) {
            SCOPE.set(self.0.map(|old| State {
                calls: u32::MAX,
                complete: false,
                submitted: false,
                wait_for_completion: old.wait_for_completion,
            }));
        }
    }
    let previous = SCOPE.replace(Some(State {
        wait_for_completion,
        ..State::default()
    }));
    let _reset = Reset(previous);
    let result = present();
    let known = previous.is_none()
        && SCOPE.get().is_some_and(|s| {
            s.calls == 1
                && if wait_for_completion {
                    s.complete
                } else {
                    s.submitted
                }
        });
    (result, known)
}
pub(super) fn begin() -> bool {
    SCOPE.with(|scope| match scope.get() {
        Some(mut state) => {
            state.calls = state.calls.saturating_add(1);
            state.complete = false;
            state.submitted = false;
            scope.set(Some(state));
            state.calls == 1 && state.wait_for_completion
        }
        None => false,
    })
}
pub(super) fn submitted() {
    SCOPE.with(|scope| {
        if let Some(mut state) = scope.get() {
            state.submitted = true;
            scope.set(Some(state));
        }
    });
}
pub(super) fn completed() {
    SCOPE.with(|scope| {
        if let Some(mut state) = scope.get() {
            state.complete = true;
            scope.set(Some(state));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn submission_is_distinct_from_completion_and_needs_exact_success() {
        assert!(!observe_submission(|| {}).1);
        assert!(
            !observe_submission(|| {
                begin();
            })
            .1
        );
        assert!(
            observe_submission(|| {
                assert!(!begin());
                submitted();
            })
            .1
        );
        assert!(
            !observe(|| {
                assert!(begin());
                submitted();
            })
            .1
        );
        assert!(
            !observe_submission(|| {
                begin();
                submitted();
                begin();
                submitted();
            })
            .1
        );
        assert!(
            !observe_submission(|| {
                begin();
                submitted();
                let _ = observe(|| {
                    begin();
                    completed();
                });
            })
            .1
        );
    }
    #[test]
    fn unknown_failed_multiple_nested_and_panicked_scopes_fail_closed() {
        assert!(!observe(|| {}).1);
        assert!(
            !observe(|| {
                assert!(begin());
            })
            .1
        );
        assert!(
            observe(|| {
                assert!(begin());
                completed();
            })
            .1
        );
        assert!(
            !observe(|| {
                begin();
                completed();
                begin();
                completed();
            })
            .1
        );
        assert!(
            !observe(|| {
                assert!(
                    !observe(|| {
                        begin();
                        completed();
                    })
                    .1
                );
            })
            .1
        );
        assert!(
            !observe(|| {
                begin();
                completed();
                let _ = observe(|| {
                    begin();
                    completed();
                });
            })
            .1
        );
        let _ = std::panic::catch_unwind(|| {
            observe(|| {
                begin();
                panic!("test");
            })
        });
        assert!(!observe(|| {}).1);
    }
}
