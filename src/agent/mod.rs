//! Provider-neutral agent/task domain types.
//!
//! Provider adapters and UI surfaces should depend on these types rather than
//! on a provider's wire protocol. Compatibility conversions into the current
//! `jterm_core` agent prompt live beside the domain model so any loss of
//! provenance is explicit and testable.

pub mod context;
pub mod diff;

pub use context::{ContextError, SemanticCommandContext};
pub use diff::{AgentDiffPanel, AgentDiffState, DiffRequestError};
