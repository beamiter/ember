//! jsh execution-journal bridge, now shared via `jterm_core::execution_journal`.
//!
//! This module adapts the terminal's [`CompletedCommandEvent`] into the core
//! journal's neutral input type; everything else re-exports unchanged.

pub(crate) use jterm_core::execution_journal::{
    flush, request_history, HistoryLoad, HistoryRequestError, PersistedExecution, SubmitError,
};

use crate::terminal::CompletedCommandEvent;

pub(crate) fn submit(completed: CompletedCommandEvent) -> Result<(), SubmitError> {
    if !completed.is_trusted_completion() {
        // Boundary inference exists to release local UI/Agent lifecycle state;
        // persisting it would turn missing OSC evidence into a durable false
        // completion that later sessions could mistake for journal recovery.
        return Ok(());
    }
    let completed = completed.completed;
    jterm_core::execution_journal::submit(jterm_core::execution_journal::CompletedExecution {
        id: completed.id,
        output: completed.output,
        output_available: completed.output_available,
        truncated: completed.truncated,
        total_bytes: completed.total_bytes,
    })
}
