//! Provider-neutral agent/task domain types.
//!
//! Provider adapters and UI surfaces should depend on these types rather than
//! on a provider's wire protocol. Compatibility conversions into the current
//! `jterm_core` agent prompt live beside the domain model so any loss of
//! provenance is explicit and testable.

pub mod context;
pub mod diff;
pub mod driver;
pub mod drivers;
pub mod event;
pub mod launcher;
pub mod native;
pub mod runtime;
pub mod task;
pub mod validation;
pub mod worktree;

pub use context::{ContextError, SemanticCommandContext};
pub use diff::{AgentDiffPanel, AgentDiffState, DiffRequestError};
pub use driver::{
    AgentCancellation, AgentCommand, AgentDriver, AgentDriverError, AgentEventQueueLimits,
    AgentEventQueueStats, AgentEventReceiveError, AgentEventReceiver, AgentEventSendError,
    AgentEventSender, AgentEventSink, AgentPrompt, AgentStartRequest, ApprovalDecision,
};
pub use drivers::{
    CodexAppServerApproval, CodexAppServerApprovalFileChange, CodexAppServerApprovalKind,
    CodexAppServerCommandView, CodexAppServerExitCause, CodexAppServerExitReport,
    CodexAppServerFileChange, CodexAppServerFileChangeView, CodexAppServerPhase,
    CodexAppServerProcessExit, CodexAppServerTurnCommandSummary, CodexAppServerTurnFileSummary,
    CodexAppServerTurnHistory, CodexAppServerViewSnapshot, CODEX_APP_SERVER_LIVE_TURN_MAX,
    CODEX_APP_SERVER_TURN_HISTORY_CAPACITY, CODEX_APP_SERVER_TURN_HISTORY_MAX_BYTES,
};
pub use event::{
    AgentEvent, AgentEventEpoch, AgentEventError, AgentEventKind, AgentEventStream,
    AgentSessionOutcome, AgentTurnId, ApprovalId, InvalidNativeAgentSessionId,
    InvalidProviderSessionId, NativeAgentSessionId, ProviderSessionId,
};
pub use launcher::{AgentLaunchError, AgentLaunchSpec};
pub use native::{
    NativeCodexHomeError, NativePromptError, NativePromptPolicy, NativeWorkspaceError,
    NATIVE_AGENT_FOLLOW_UP_MAX_BYTES,
};
pub use runtime::{
    AgentRuntimeCompletion, AgentRuntimeError, AgentRuntimeIssue, AgentRuntimeManager,
    AgentRuntimePollReport,
};
pub use task::{
    AgentProvider, AgentTask, NewTask, TaskError, TaskId, TaskManager, TaskRuntimeKind, TaskSource,
    TaskStatus, TaskTerminalRole, TaskValidationState, TaskValidationStatus,
};
pub use validation::{
    prepare_task_validation, PreparedTaskValidation, TaskValidationError, TaskValidationPath,
};
pub use worktree::{
    CreateWorktreeRequest, ManagedWorktree, RetireOutcome, RetirePolicy, WorktreeError,
    WorktreeService,
};
