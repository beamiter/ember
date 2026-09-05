//! jsh execution-journal bridge, now shared via `jterm_core::execution_journal`.
//!
//! This module adapts the terminal's [`CompletedCommandEvent`] into the core
//! journal's neutral input type; everything else re-exports unchanged.

pub(crate) use jterm_core::execution_journal::{
    flush, request_history, HistoryLoad, HistoryRequestError, PersistedExecution, SubmitError,
};

use jterm_core::execution_journal::ExecutionLifecycle;

use crate::terminal::{CompletedCommandEvent, CompletedCommandOutput};

/// Derive the journal capability for one completed command, or `None` when
/// this execution may not write durable output.
///
/// The journal binds output to the exact Start generation the terminal saw,
/// not to a correlation id alone: `ExecutionLifecycle::from_command_meta`
/// refuses unless `id`, `session_id`, `seq` and `started_at_ms` all arrived on
/// one OSC 133 `C` packet. That is why ember carries the identity triple from
/// `C` on the record rather than reading it off the `D` that closes the block
/// — jsh sends none of the three on `D`, and the shared parser will not accept
/// them there, so anything assembled at completion would name a Start nobody
/// observed. The core writer re-checks all four against the authoritative
/// on-disk Start under the journal lock, so a stale token fails closed there
/// too; this side simply never mints one.
///
/// A shell that sends no jsh identity, and ember's own `local:{sequence}`
/// fallback ids — which `is_valid_jsh_execution_id` rejects for the colon —
/// therefore produce no journal row at all rather than a mis-keyed one.
fn lifecycle_for(completed: &CompletedCommandOutput) -> Option<ExecutionLifecycle> {
    ExecutionLifecycle::from_command_meta(&jterm_core::parser::CommandMeta {
        id: Some(completed.id.clone()),
        session_id: completed.session_id.clone(),
        seq: completed.seq,
        started_at_ms: completed.started_at_ms,
        ..Default::default()
    })
}

pub(crate) fn submit(completed: CompletedCommandEvent) -> Result<(), SubmitError> {
    if !completed.is_trusted_completion() {
        // Boundary inference exists to release local UI/Agent lifecycle state;
        // persisting it would turn missing OSC evidence into a durable false
        // completion that later sessions could mistake for journal recovery.
        return Ok(());
    }
    let completed = completed.completed;
    let Some(lifecycle) = lifecycle_for(&completed) else {
        return Ok(());
    };
    jterm_core::execution_journal::submit(jterm_core::execution_journal::CompletedExecution {
        lifecycle,
        output: completed.output,
        output_available: completed.output_available,
        truncated: completed.truncated,
        total_bytes: completed.total_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completed(id: &str) -> CompletedCommandOutput {
        CompletedCommandOutput {
            id: id.to_owned(),
            session_id: Some("probe-session-1".to_owned()),
            seq: Some(1),
            started_at_ms: Some(1_788_571_993_465),
            command: Some("echo hello-journal".to_owned()),
            cwd: Some("/home/yj/projects/jsh".to_owned()),
            exit_code: Some(0),
            duration_ms: Some(0),
            output: "hello-journal\n".to_owned(),
            output_available: true,
            truncated: false,
            total_bytes: 14,
            agent_generation: None,
        }
    }

    #[test]
    fn a_complete_start_identity_yields_the_journal_capability() {
        let lifecycle =
            lifecycle_for(&completed("jsh-b8c6f0d1-1")).expect("all four slots present");
        assert_eq!(lifecycle.id(), "jsh-b8c6f0d1-1");
        assert_eq!(lifecycle.session_id(), "probe-session-1");
        assert_eq!(lifecycle.seq(), 1);
        assert_eq!(lifecycle.started_at_ms(), 1_788_571_993_465);
    }

    #[test]
    fn every_missing_start_identity_slot_fails_closed() {
        // Each of the four is individually load-bearing. A token assembled
        // from three observed slots and one guessed one would key a durable
        // row the shell never announced.
        let mut no_session = completed("jsh-b8c6f0d1-1");
        no_session.session_id = None;
        assert!(lifecycle_for(&no_session).is_none());

        let mut no_seq = completed("jsh-b8c6f0d1-1");
        no_seq.seq = None;
        assert!(lifecycle_for(&no_seq).is_none());

        let mut no_started_at = completed("jsh-b8c6f0d1-1");
        no_started_at.started_at_ms = None;
        assert!(lifecycle_for(&no_started_at).is_none());

        // A session id that is not jsh's grammar is not repaired into one.
        let mut bad_session = completed("jsh-b8c6f0d1-1");
        bad_session.session_id = Some("../other-session".to_owned());
        assert!(lifecycle_for(&bad_session).is_none());
    }

    #[test]
    fn embers_own_local_fallback_id_never_reaches_the_journal() {
        // `local:{sequence}` is what ember names a block a shell never
        // identified. The colon is outside `is_valid_jsh_execution_id`, so the
        // fallback cannot collide with a real jsh execution — and, because the
        // whole triple is present on a jsh-spawned pane whose *first* blocks
        // predate the shell's first `C`, the id is the slot that must refuse.
        let mut local = completed("local:7");
        local.session_id = Some("probe-session-1".to_owned());
        assert!(lifecycle_for(&local).is_none());
    }

    #[test]
    fn a_boundary_inferred_completion_is_dropped_before_any_identity_check() {
        // Provenance is checked first and independently: an inferred close
        // must not become a durable row even when the terminal did observe a
        // full `C` identity for the command it is closing.
        let event = CompletedCommandEvent {
            completed: completed("jsh-b8c6f0d1-1"),
            start_mark_seen: true,
            completion_provenance: crate::block_mode::CompletionProvenance::BoundaryInferred,
        };
        assert!(!event.is_trusted_completion());
        assert!(matches!(submit(event), Ok(())));
    }
}
