//! Window IME events have no pane identity. Own and cancel terminal compositions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Owner {
    pub epoch: u64,
    pub instance: usize,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Event {
    Opened,
    Preedit { empty: bool },
    Commit,
    Closed,
    Other,
}
#[derive(Default)]
enum Composition {
    #[default]
    Idle,
    Active {
        empty: bool,
    },
    Cancelled {
        closed: bool,
    },
}
#[derive(Default)]
pub(crate) struct Ownership {
    owner: Option<Owner>,
    composition: Composition,
    enter_claimed: bool,
}
impl Ownership {
    /// Epoch catches A -> B -> A; instance is a live terminal allocation, not an index.
    pub fn sync(&mut self, owner: Option<Owner>) -> bool {
        if self.owner == owner {
            return false;
        }
        self.owner = owner;
        if matches!(self.composition, Composition::Active { .. }) {
            self.composition = Composition::Cancelled { closed: false };
            return true;
        }
        false
    }
    /// Filter cancelled terminal events before modal routing. Idle overlay IME
    /// stays untouched; an old Commit there retires exactly the old tombstone.
    pub fn admit(&mut self, event: Event) -> bool {
        if let Composition::Cancelled { closed } = self.composition {
            match event {
                Event::Commit => {
                    self.composition = Composition::Idle;
                    return false;
                }
                Event::Closed => {
                    self.composition = Composition::Cancelled { closed: true };
                    return false;
                }
                Event::Opened => self.composition = Composition::Idle,
                Event::Preedit { empty: false } if closed => self.composition = Composition::Idle,
                _ => return false,
            }
        }
        if self.owner.is_none() {
            return true;
        }
        match event {
            Event::Preedit { empty: false } => {
                if !matches!(self.composition, Composition::Active { empty: false }) {
                    self.enter_claimed = false;
                }
                self.composition = Composition::Active { empty: false };
            }
            Event::Preedit { empty: true }
                if matches!(self.composition, Composition::Active { .. }) =>
            {
                // Modern egui can report cancellation only as empty preedit.
                // Keep the owner for a possible following Commit, but never
                // require a Closed/Commit event to recover a fresh Enter.
                self.composition = Composition::Active { empty: true };
            }
            Event::Commit | Event::Closed => self.composition = Composition::Idle,
            _ => {}
        }
        true
    }
    pub fn has_composition(&self) -> bool {
        !matches!(self.composition, Composition::Idle)
    }

    /// A preedit first observed after a pointer ownership boundary has no
    /// proven terminal recipient. Keep its eventual Commit quarantined too.
    pub fn reject_unowned_preedit(&mut self) {
        if matches!(
            self.composition,
            Composition::Idle | Composition::Cancelled { closed: true }
        ) {
            self.composition = Composition::Cancelled { closed: false };
            self.enter_claimed = false;
        }
    }

    /// Claim only one cancelled candidate hold. NativeEnterOwnership handles
    /// repeats and real releases, with no second persistent egui release latch.
    pub fn claim_enter(&mut self) -> bool {
        match self.composition {
            Composition::Active { empty: false } => {
                self.enter_claimed = true;
                true
            }
            Composition::Active { empty: true } | Composition::Cancelled { closed: false }
                if !self.enter_claimed =>
            {
                self.enter_claimed = true;
                true
            }
            _ => false,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn owner(epoch: u64, instance: usize) -> Option<Owner> {
        Some(Owner { epoch, instance })
    }
    const PREEDIT: Event = Event::Preedit { empty: false };
    #[test]
    fn empty_without_commit_recovers_enter_while_retaining_commit_owner() {
        let mut s = Ownership::default();
        s.sync(owner(1, 10));
        s.admit(PREEDIT);
        s.admit(Event::Preedit { empty: true });
        assert!(s.claim_enter());
        assert!(!s.claim_enter());
        assert!(
            s.admit(Event::Commit),
            "same owner may still commit after empty"
        );
        s.admit(PREEDIT);
        s.admit(Event::Preedit { empty: true });
        s.sync(owner(2, 20));
        assert!(
            !s.admit(Event::Commit),
            "empty never relinquishes source ownership"
        );
    }

    #[test]
    fn pane_return_and_reused_index_cannot_inherit_late_commit() {
        for next in [owner(2, 20), owner(2, 10), None] {
            let mut s = Ownership::default();
            s.sync(owner(1, 10));
            assert!(s.admit(PREEDIT));
            assert!(s.admit(Event::Preedit { empty: true }));
            assert!(s.sync(next));
            s.sync(owner(3, 10));
            assert!(!s.admit(Event::Commit));
            assert!(s.admit(Event::Commit));
        }
    }
    #[test]
    fn cancelled_commit_in_overlay_retires_only_old_cycle() {
        let mut s = Ownership::default();
        s.sync(owner(1, 10));
        s.admit(PREEDIT);
        s.sync(None);
        assert!(!s.admit(Event::Commit));
        assert!(s.admit(PREEDIT));
        assert!(s.admit(Event::Commit));
        s.sync(owner(1, 10));
        assert!(s.admit(PREEDIT));
        assert!(s.admit(Event::Commit));
    }
    #[test]
    fn silent_cancellation_claims_only_one_candidate_hold() {
        let mut s = Ownership::default();
        s.sync(owner(1, 10));
        s.admit(PREEDIT);
        s.sync(owner(2, 20));
        assert!(s.claim_enter());
        assert!(!s.claim_enter());
        assert!(!s.admit(Event::Commit));
        assert!(s.admit(Event::Commit));
    }
    #[test]
    fn claimed_candidate_does_not_claim_another_after_switch() {
        let mut s = Ownership::default();
        s.sync(owner(1, 10));
        s.admit(PREEDIT);
        assert!(s.claim_enter());
        s.sync(None);
        assert!(!s.claim_enter());
    }
    #[test]
    fn lifecycle_order_distinguishes_candidate_enter_from_submit() {
        let mut s = Ownership::default();
        s.sync(owner(1, 10));
        assert!(s.admit(Event::Commit));
        assert!(!s.claim_enter());
        s.admit(PREEDIT);
        assert!(s.claim_enter());
        assert!(s.admit(Event::Commit));
        assert!(!s.claim_enter());
    }
    #[test]
    fn closed_then_preedit_or_open_recovers_cancelled_cycle() {
        for closed in [true, false] {
            let mut s = Ownership::default();
            s.sync(owner(1, 10));
            s.admit(PREEDIT);
            s.sync(owner(2, 20));
            if closed {
                assert!(!s.admit(Event::Closed));
            } else {
                assert!(s.admit(Event::Opened));
            }
            assert!(s.admit(PREEDIT));
            assert!(s.admit(Event::Commit));
        }
    }
}
