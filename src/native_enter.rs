//! Physical Enter ownership at the native window boundary. No timing or repeat
//! metadata is used: physical identity and conservative native focus snapshots
//! control ownership; only real releases can clear it.
use std::collections::HashSet;
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

#[derive(Default)]
pub(crate) struct NativeEnterOwnership {
    pressed: HashSet<PhysicalKey>,
    owned: HashSet<PhysicalKey>,
}

impl NativeEnterOwnership {
    pub fn claim_pressed(&mut self) {
        self.owned.extend(self.pressed.iter().copied());
    }

    /// True consumes only a press. Every release still reaches egui so its
    /// logical key state cannot remain down after the original press it saw.
    pub fn observe(&mut self, event: &winit::event::KeyEvent, synthetic: bool) -> bool {
        let is_enter = matches!(event.logical_key, Key::Named(NamedKey::Enter))
            || matches!(
                event.physical_key,
                PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter)
            );
        self.transition(
            event.physical_key,
            event.state.is_pressed(),
            synthetic,
            is_enter,
        )
    }

    fn transition(
        &mut self,
        key: PhysicalKey,
        pressed: bool,
        synthetic: bool,
        is_enter: bool,
    ) -> bool {
        // A synthetic press on refocus is not a new action, but it is a
        // native snapshot that this physical key may already be held. Retain
        // it passively so a later mouse/modal recall can claim that key before
        // its first repeat. Synthetic releases still cannot prove release.
        if synthetic {
            if pressed && is_enter {
                self.pressed.insert(key);
                // A newly observed held Enter joins an existing protected
                // gesture even if its partner is released before rendering.
                if !self.owned.is_empty() {
                    self.owned.insert(key);
                }
            }
            return false;
        }
        if !pressed {
            self.pressed.remove(&key);
            self.owned.remove(&key);
            return false;
        }
        if !is_enter && !self.pressed.contains(&key) {
            return false;
        }
        self.pressed.insert(key);
        // Overlapping Enter keys join the same owned gesture. Releasing one
        // must not let the other key's repeats submit the recalled command.
        if !self.owned.is_empty() {
            self.owned.insert(key);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MAIN: PhysicalKey = PhysicalKey::Code(KeyCode::Enter);
    const PAD: PhysicalKey = PhysicalKey::Code(KeyCode::NumpadEnter);

    #[test]
    fn overlapping_enters_stay_owned_in_both_release_orders() {
        for (first, second) in [(MAIN, PAD), (PAD, MAIN)] {
            for release_first in [true, false] {
                let mut state = NativeEnterOwnership::default();
                assert!(!state.transition(first, true, false, true));
                state.claim_pressed();
                assert!(state.transition(second, true, false, true));
                let (released, held) = if release_first {
                    (first, second)
                } else {
                    (second, first)
                };
                assert!(
                    !state.transition(released, false, false, true),
                    "all releases reach egui"
                );
                assert!(state.transition(held, true, false, true));
                assert!(!state.transition(held, false, false, true));
                assert!(
                    !state.transition(first, true, false, true),
                    "fresh gesture passes after all releases"
                );
                assert!(
                    !state.transition(first, true, false, true),
                    "ordinary terminal repeats remain allowed"
                );
            }
        }
    }

    #[test]
    fn claim_adopts_both_keys_already_down_before_mouse_fill() {
        let mut state = NativeEnterOwnership::default();
        assert!(!state.transition(MAIN, true, false, true));
        assert!(!state.transition(PAD, true, false, true));
        state.claim_pressed();
        assert!(state.transition(MAIN, true, false, true));
        assert!(state.transition(PAD, true, false, true));
    }

    #[test]
    fn coalesced_modal_opener_partial_release_still_claims_remaining_key() {
        let mut state = NativeEnterOwnership::default();
        state.transition(MAIN, true, false, true);
        state.transition(PAD, true, false, true);
        state.transition(MAIN, false, false, true);
        // The modal owns this input frame even though egui's aliased Enter
        // state is now released. Accepting into an argument form still adopts
        // the actual held keypad identity before the next native repeat.
        state.claim_pressed();
        assert!(state.transition(PAD, true, false, true));
        state.transition(PAD, false, false, true);
        assert!(!state.transition(MAIN, true, false, true));
    }

    #[test]
    fn synthetic_blur_releases_and_refocus_presses_do_not_change_ownership() {
        let mut state = NativeEnterOwnership::default();
        state.transition(MAIN, true, false, true);
        state.claim_pressed();
        state.transition(PAD, true, false, true);
        for key in [MAIN, PAD] {
            assert!(!state.transition(key, false, true, true));
            assert!(!state.transition(key, true, true, true));
            assert!(state.transition(key, true, false, true));
        }
        state.transition(MAIN, false, false, true);
        assert!(state.transition(PAD, true, false, true));
        state.transition(PAD, false, false, true);
        assert!(!state.transition(PAD, true, false, true));
    }

    #[test]
    fn synthetic_pressed_snapshot_can_be_adopted_without_becoming_an_action() {
        for key in [MAIN, PAD] {
            let mut state = NativeEnterOwnership::default();
            assert!(!state.transition(key, true, true, true));
            assert!(state.pressed.contains(&key));
            assert!(state.owned.is_empty(), "snapshot alone is not confirmation");
            state.claim_pressed();
            assert!(state.transition(key, true, false, true));
            assert!(!state.transition(key, false, true, true));
            assert!(state.transition(key, true, false, true));
            assert!(!state.transition(key, false, false, true));
            assert!(
                !state.transition(key, true, false, true),
                "fresh Enter recovers after its real release"
            );
        }
    }

    #[test]
    fn newly_observed_synthetic_partner_joins_existing_owned_gesture() {
        for (first, second) in [(MAIN, PAD), (PAD, MAIN)] {
            let mut state = NativeEnterOwnership::default();
            state.transition(first, true, false, true);
            state.claim_pressed();
            assert!(!state.transition(second, true, true, true));
            state.transition(first, false, false, true);
            assert!(state.transition(second, true, false, true));
            state.transition(second, false, false, true);
            assert!(!state.transition(first, true, false, true));
        }
    }

    #[test]
    fn unrelated_keys_and_layout_changed_releases_pass() {
        let mut state = NativeEnterOwnership::default();
        state.transition(MAIN, true, false, true);
        state.claim_pressed();
        let letter = PhysicalKey::Code(KeyCode::KeyA);
        assert!(!state.transition(letter, true, false, false));
        assert!(!state.transition(letter, false, false, false));
        assert_eq!(state.pressed.len(), 1);
        assert!(!state.transition(MAIN, false, false, false));
        assert!(state.owned.is_empty());
    }
}
