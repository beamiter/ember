//! Provider-neutral agent/task domain types.
//!
//! Provider adapters and UI surfaces should depend on these types rather than
//! on a provider's wire protocol. Compatibility conversions into the current
//! `jterm_core` agent prompt live beside the domain model so any loss of
//! provenance is explicit and testable.

pub mod context;
pub mod diff;
pub mod launcher;
pub mod task;
pub mod worktree;

pub use context::{ContextError, SemanticCommandContext};
pub use diff::{AgentDiffPanel, AgentDiffState, DiffRequestError};
pub use launcher::{AgentLaunchError, AgentLaunchSpec};
pub use task::{
    AgentProvider, AgentTask, NewTask, TaskError, TaskId, TaskManager, TaskSource, TaskStatus,
};
pub use worktree::{
    CreateWorktreeRequest, ManagedWorktree, RetireOutcome, RetirePolicy, WorktreeError,
    WorktreeService,
};
