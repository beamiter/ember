//! rsh execution-journal bridge, now shared via `jterm_core::execution_journal`.
//!
//! This module adapts the terminal's [`CompletedCommandOutput`] into the core
//! journal's neutral input type; everything else re-exports unchanged.

pub(crate) use jterm_core::execution_journal::{
    flush, request_history, HistoryLoad, HistoryRequestError, PersistedExecution, SubmitError,
};

use crate::terminal::CompletedCommandOutput;

pub(crate) fn submit(completed: CompletedCommandOutput) -> Result<(), SubmitError> {
    jterm_core::execution_journal::submit(jterm_core::execution_journal::CompletedExecution {
        id: completed.id,
        output: completed.output,
        output_available: completed.output_available,
        truncated: completed.truncated,
        total_bytes: completed.total_bytes,
    })
}
