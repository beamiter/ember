//! Owned semantic command context passed from terminal history to agent tasks.

use jterm_core::ai::BlockContext;
use std::fmt;
use std::time::SystemTime;

/// Compatibility budgets enforced by the pinned jagent prompt adapter.
/// Keeping them explicit here prevents it from silently eliding evidence
/// while Ember still labels the attached command/context as complete.
pub const AGENT_BLOCK_COMMAND_PROMPT_BYTES: usize = 16 * 1024;
pub const AGENT_BLOCK_OUTPUT_PROMPT_BYTES: usize = 64 * 1024;
pub const AGENT_BLOCK_CWD_PROMPT_BYTES: usize = 4 * 1024;

/// An owned snapshot of one semantic command execution.
///
/// The source identifiers remain stable when tabs and split panes are
/// reordered. Terminal buffer anchors deliberately do not appear here: a task
/// must keep its evidence after scrollback or the live
/// [`crate::terminal::CommandRecord`] has been evicted.
///
/// `command_exact` means the shell supplied the complete command as OSC 133
/// metadata. A display-reconstructed command can still be useful as untrusted
/// evidence, but must never authorize Retry. `command_truncated` means the
/// producer explicitly omitted or shortened the command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticCommandContext {
    pub source_session_id: String,
    pub source_execution_id: String,
    pub source_sequence: u64,

    pub command: Option<String>,
    pub command_exact: bool,
    pub command_truncated: bool,

    /// Directory in which the source command started.
    pub cwd: Option<String>,
    /// Directory reported after completion; may differ for a stateful `cd`.
    pub cwd_after: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,

    /// Normalized rendered PTY text for the OSC 133 C..D range.
    pub output_text: String,
    /// False distinguishes an unavailable capture from genuine empty output.
    pub output_available: bool,
    pub output_truncated: bool,
    /// UTF-8 byte count before bounded head/tail capture.
    pub output_total_bytes: usize,

    pub started_at: Option<SystemTime>,
    pub finished_at: Option<SystemTime>,
}

/// Why a semantic snapshot cannot be represented by the current compatibility
/// agent prompt context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextError {
    MissingSourceSessionId,
    MissingSourceExecutionId,
    MissingCommand,
    CommandTruncated,
    CommandExceedsPromptBudget,
    CwdExceedsPromptBudget,
    MissingExitCode,
    OutputUnavailable,
}

impl fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingSourceSessionId => "semantic command context has no source session id",
            Self::MissingSourceExecutionId => "semantic command context has no source execution id",
            Self::MissingCommand => "semantic command context has no command text",
            Self::CommandTruncated => "semantic command context contains a truncated command",
            Self::CommandExceedsPromptBudget => {
                "semantic command exceeds the Agent prompt's exact-command budget"
            }
            Self::CwdExceedsPromptBudget => {
                "semantic command cwd exceeds the Agent prompt's workspace budget"
            }
            Self::MissingExitCode => "semantic command context has no reported exit code",
            Self::OutputUnavailable => "semantic command context has no captured command output",
        })
    }
}

impl std::error::Error for ContextError {}

impl SemanticCommandContext {
    /// Convert to the compatibility context consumed by the current
    /// `jterm_core` agent prompt.
    ///
    /// This adapter is intentionally strict about unavailable evidence. In
    /// particular, it never turns an unreported exit status into a made-up
    /// failure code and never turns a failed output capture into genuine empty
    /// output. The compatibility type has no command provenance fields, so an
    /// explicitly truncated command is rejected rather than silently losing
    /// that fact. `command_exact == false` remains representable as untrusted
    /// evidence; callers must still require `command_exact` for Retry or any
    /// other execution-authorizing action.
    pub fn to_block_context(&self) -> Result<BlockContext, ContextError> {
        if self.source_session_id.is_empty() {
            return Err(ContextError::MissingSourceSessionId);
        }
        if self.source_execution_id.is_empty() {
            return Err(ContextError::MissingSourceExecutionId);
        }
        let command = self
            .command
            .as_ref()
            .filter(|command| !command.trim().is_empty())
            .ok_or(ContextError::MissingCommand)?;
        if self.command_truncated {
            return Err(ContextError::CommandTruncated);
        }
        if command.len() > AGENT_BLOCK_COMMAND_PROMPT_BYTES {
            return Err(ContextError::CommandExceedsPromptBudget);
        }
        if self
            .cwd
            .as_ref()
            .is_some_and(|cwd| cwd.len() > AGENT_BLOCK_CWD_PROMPT_BYTES)
        {
            return Err(ContextError::CwdExceedsPromptBudget);
        }
        let exit_code = self.exit_code.ok_or(ContextError::MissingExitCode)?;
        if !self.output_available {
            return Err(ContextError::OutputUnavailable);
        }

        Ok(BlockContext {
            cmd: command.clone(),
            output: self.output_text.clone(),
            cwd: self.cwd.clone(),
            exit_code,
            truncated: self.output_truncated
                || self.output_text.len() > AGENT_BLOCK_OUTPUT_PROMPT_BYTES
                || self.output_total_bytes > AGENT_BLOCK_OUTPUT_PROMPT_BYTES,
        })
    }
}

impl TryFrom<&SemanticCommandContext> for BlockContext {
    type Error = ContextError;

    fn try_from(context: &SemanticCommandContext) -> Result<Self, Self::Error> {
        context.to_block_context()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn context() -> SemanticCommandContext {
        SemanticCommandContext {
            source_session_id: "session-stable".to_string(),
            source_execution_id: "execution-stable".to_string(),
            // Sequence zero is the first valid local record and must not be
            // mistaken for a missing identity.
            source_sequence: 0,
            command: Some("cargo test".to_string()),
            command_exact: true,
            command_truncated: false,
            cwd: Some("/workspace/ember".to_string()),
            cwd_after: Some("/workspace/ember".to_string()),
            exit_code: Some(101),
            duration_ms: Some(8420),
            output_text: "failures:\n    terminal::test".to_string(),
            output_available: true,
            output_truncated: true,
            output_total_bytes: 300_000,
            started_at: Some(UNIX_EPOCH + Duration::from_secs(10)),
            finished_at: Some(UNIX_EPOCH + Duration::from_secs(18)),
        }
    }

    #[test]
    fn conversion_preserves_prompt_fields_without_mutating_domain_provenance() {
        let semantic = context();
        let block = semantic.to_block_context().expect("complete context");

        assert_eq!(block.cmd, "cargo test");
        assert_eq!(block.output, "failures:\n    terminal::test");
        assert_eq!(block.cwd.as_deref(), Some("/workspace/ember"));
        assert_eq!(block.exit_code, 101);
        assert!(block.truncated);

        assert_eq!(semantic.source_sequence, 0);
        assert_eq!(semantic.output_total_bytes, 300_000);
        assert!(semantic.command_exact);
        assert_eq!(semantic.cwd_after.as_deref(), Some("/workspace/ember"));
    }

    #[test]
    fn missing_or_blank_command_is_an_explicit_error() {
        let mut semantic = context();
        semantic.command = None;
        assert_eq!(
            semantic.to_block_context(),
            Err(ContextError::MissingCommand)
        );

        semantic.command = Some(" \t\n".to_string());
        assert_eq!(
            semantic.to_block_context(),
            Err(ContextError::MissingCommand)
        );
    }

    #[test]
    fn missing_exit_code_is_never_coerced_to_failure() {
        let mut semantic = context();
        semantic.exit_code = None;

        assert_eq!(
            semantic.to_block_context(),
            Err(ContextError::MissingExitCode)
        );
    }

    #[test]
    fn unavailable_output_is_not_conflated_with_real_empty_output() {
        let mut unavailable = context();
        unavailable.output_available = false;
        unavailable.output_text.clear();
        unavailable.output_total_bytes = 0;
        assert_eq!(
            unavailable.to_block_context(),
            Err(ContextError::OutputUnavailable)
        );

        let mut empty = context();
        empty.output_text.clear();
        empty.output_truncated = false;
        empty.output_total_bytes = 0;
        let block = empty
            .to_block_context()
            .expect("captured empty output is valid evidence");
        assert!(block.output.is_empty());
        assert!(!block.truncated);
    }

    #[test]
    fn truncated_command_is_rejected_because_compat_context_cannot_mark_it() {
        let mut semantic = context();
        semantic.command_exact = false;
        semantic.command_truncated = true;

        assert_eq!(
            semantic.to_block_context(),
            Err(ContextError::CommandTruncated)
        );
    }

    #[test]
    fn prompt_budgets_never_silently_elide_exact_command_or_cwd() {
        let mut semantic = context();
        semantic.command = Some("x".repeat(AGENT_BLOCK_COMMAND_PROMPT_BYTES));
        assert!(semantic.to_block_context().is_ok());
        semantic.command = Some("x".repeat(AGENT_BLOCK_COMMAND_PROMPT_BYTES + 1));
        assert_eq!(
            semantic.to_block_context(),
            Err(ContextError::CommandExceedsPromptBudget)
        );

        semantic.command = Some("cargo test".to_string());
        semantic.cwd = Some("/".repeat(AGENT_BLOCK_CWD_PROMPT_BYTES + 1));
        assert_eq!(
            semantic.to_block_context(),
            Err(ContextError::CwdExceedsPromptBudget)
        );
    }

    #[test]
    fn prompt_level_output_elision_is_reported_as_truncation() {
        let mut semantic = context();
        semantic.output_truncated = false;
        semantic.output_text = "x".repeat(AGENT_BLOCK_OUTPUT_PROMPT_BYTES);
        semantic.output_total_bytes = semantic.output_text.len();
        assert!(!semantic.to_block_context().unwrap().truncated);

        semantic.output_text.push('x');
        semantic.output_total_bytes += 1;
        assert!(semantic.to_block_context().unwrap().truncated);

        semantic.output_text.truncate(128);
        semantic.output_total_bytes = AGENT_BLOCK_OUTPUT_PROMPT_BYTES + 1;
        assert!(semantic.to_block_context().unwrap().truncated);
    }

    #[test]
    fn display_reconstructed_command_remains_untrusted_analyzable_evidence() {
        let mut semantic = context();
        semantic.command_exact = false;

        let block = semantic
            .to_block_context()
            .expect("inexact display text is safe as user-role evidence");
        assert_eq!(block.cmd, "cargo test");
    }

    #[test]
    fn source_identity_must_be_present_even_though_compat_type_drops_it() {
        let mut semantic = context();
        semantic.source_session_id.clear();
        assert_eq!(
            semantic.to_block_context(),
            Err(ContextError::MissingSourceSessionId)
        );

        semantic.source_session_id = "session-stable".to_string();
        semantic.source_execution_id.clear();
        assert_eq!(
            BlockContext::try_from(&semantic),
            Err(ContextError::MissingSourceExecutionId)
        );
    }
}
