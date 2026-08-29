//! Thin shim over the shared `jterm_core::ai::chat_store`.
//!
//! Ember used to carry its own port of anvil's multi-chat store — ~900 lines
//! of toolkit-free state plus the unit tests that pinned it. All four family
//! terminals carried that same state machine and all four drifted; their union
//! now lives in the core, and every app keeps only this shim.
//!
//! The core's copy is stricter than ember's port was, and adopting it is the
//! point of the migration: a library-wide 8 MiB live-history budget with real
//! compaction, compaction *before* serialising (without it the live library
//! grows until `ConversationSnapshot::from_chats` refuses it and nothing can
//! be saved at all), spoof-sanitised library previews, idempotent draft
//! merging so a recovered retry cannot multiply itself across saves, and an
//! at-capacity guard so archiving cannot mutate and then fail.
//!
//! What stays here is the one decision ember owns: its panel has no
//! cancel-then-mutate step, so archiving or deleting a chat while its request
//! is in flight is refused (forge's panel cancels first and takes
//! `BusyChatPolicy::Allow`). Both constructors below carry that choice, so no
//! call site can pick a policy by accident.

pub(crate) use jterm_core::ai::{
    ChatStatus, ChatStore, ChatStoreError, RequestToken, MAX_LIVE_MESSAGE_BYTES,
};

use jterm_core::ai::{BusyChatPolicy, ConversationSnapshot};

/// Ember's archive/delete behaviour while a chat has a request in flight.
const BUSY_POLICY: BusyChatPolicy = BusyChatPolicy::Refuse;

/// A fresh single-chat library under ember's busy-chat policy.
pub(crate) fn new_store() -> ChatStore {
    ChatStore::with_busy_policy(BUSY_POLICY)
}

/// Restore a persisted library under ember's busy-chat policy.
pub(crate) fn restore_store(snapshot: ConversationSnapshot) -> ChatStore {
    ChatStore::restore_with_busy_policy(snapshot, BUSY_POLICY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_store_built_here_refuses_to_mutate_a_busy_chat() {
        // Refuse is the core's current default, but ember's panel *depends* on
        // it: nothing in the panel cancels an in-flight request before
        // archiving or deleting, so a policy flip would silently orphan a
        // running request's chat. Pin it on both construction paths.
        let mut store = new_store();
        assert_eq!(store.busy_policy(), BusyChatPolicy::Refuse);
        store
            .begin_turn("hello".into(), None, "Thinking…".into(), true)
            .expect("a fresh chat accepts a turn");
        assert!(store.is_active_busy());
        assert_eq!(
            store.toggle_archive_active(),
            Err(ChatStoreError::Busy),
            "archiving a busy chat must be refused"
        );
        assert_eq!(
            store.delete_active().map(|outcome| outcome.deleted_chat_id),
            Err(ChatStoreError::Busy),
            "deleting a busy chat must be refused"
        );

        let (snapshot, _) = new_store()
            .snapshot_for_persistence(false)
            .expect("an empty library serialises");
        assert_eq!(
            restore_store(snapshot).busy_policy(),
            BusyChatPolicy::Refuse
        );
    }
}
