//! Provider-neutral task lifecycle and stable PTY-session bindings.
//!
//! A task is deliberately independent from tab and pane indices: both are UI
//! positions that change when sessions are inserted, moved, or closed.  The
//! manager only stores stable session IDs, so opaque PTY agents and future
//! native drivers can share the same task/dashboard model.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const MAX_TASK_TITLE_BYTES: usize = 256;
const MAX_BRANCH_BYTES: usize = 512;

/// Stable identity for a task, independent of its worktree and UI location.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(Uuid);

impl TaskId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Provider identity without leaking a provider's transport into task/UI code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProvider {
    Codex,
    Claude,
    OpenCode,
}

impl AgentProvider {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::OpenCode => "OpenCode",
        }
    }
}

/// Normalized task activity used by both opaque PTY agents and native drivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Created,
    Starting,
    Working,
    WaitingForApproval,
    WaitingForHuman,
    ReadyForReview,
    Completed,
    Failed,
    Cancelled,
    Archived,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Created => "Created",
            Self::Starting => "Starting",
            Self::Working => "Working",
            Self::WaitingForApproval => "Waiting for approval",
            Self::WaitingForHuman => "Waiting for you",
            Self::ReadyForReview => "Ready for review",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::Archived => "Archived",
        }
    }

    pub fn is_running(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Working | Self::WaitingForApproval | Self::WaitingForHuman
        )
    }

    /// Whether the dashboard should pull this task to the user's attention.
    pub fn needs_attention(self) -> bool {
        matches!(
            self,
            Self::WaitingForApproval | Self::WaitingForHuman | Self::ReadyForReview | Self::Failed
        )
    }
}

/// Provenance link back to the semantic command that created the task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSource {
    pub session_id: String,
    pub execution_id: String,
}

/// Validated input for registering an already-created task worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewTask {
    pub title: String,
    pub provider: AgentProvider,
    pub repo_root: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
    /// Immutable commit the isolated worktree was created from. Native review
    /// compares against this baseline even if the Agent creates commits.
    pub base_commit: String,
    /// Immutable owned evidence captured when a semantic command created this
    /// task. It survives source-session closure and scrollback eviction.
    pub source_context: Option<super::SemanticCommandContext>,
}

/// One isolated unit of work. Runtime links use stable IDs, never pane/index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTask {
    pub id: TaskId,
    pub title: String,
    pub provider: AgentProvider,
    pub status: TaskStatus,
    pub repo_root: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub source: Option<TaskSource>,
    pub source_context: Option<super::SemanticCommandContext>,
    pub agent_session_id: Option<String>,
    pub exit_code: Option<i32>,
    pub status_detail: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl AgentTask {
    pub fn needs_attention(&self) -> bool {
        self.status.needs_attention()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskError {
    InvalidTitle,
    InvalidBranch,
    InvalidBaseCommit,
    RepoRootMustBeAbsolute,
    WorktreePathMustBeAbsolute,
    WorktreeMatchesRepoRoot,
    InvalidSourceContext,
    InvalidSessionId,
    UnknownTask(TaskId),
    SessionAlreadyBound { session_id: String, task_id: TaskId },
    TaskAlreadyBound { task_id: TaskId, session_id: String },
    CannotArchiveRunning(TaskId),
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTitle => {
                formatter.write_str("task title is empty, too long, or contains control characters")
            }
            Self::InvalidBranch => formatter
                .write_str("task branch is empty, too long, or contains control characters"),
            Self::InvalidBaseCommit => {
                formatter.write_str("task base commit is not a full Git object ID")
            }
            Self::RepoRootMustBeAbsolute => formatter.write_str("repository root must be absolute"),
            Self::WorktreePathMustBeAbsolute => {
                formatter.write_str("worktree path must be absolute")
            }
            Self::WorktreeMatchesRepoRoot => {
                formatter.write_str("task worktree must differ from the source repository")
            }
            Self::InvalidSourceContext => {
                formatter.write_str("task source context has invalid stable identifiers")
            }
            Self::InvalidSessionId => formatter.write_str("agent session ID is invalid"),
            Self::UnknownTask(task_id) => write!(formatter, "unknown task {task_id}"),
            Self::SessionAlreadyBound {
                session_id,
                task_id,
            } => {
                write!(
                    formatter,
                    "session {session_id} is already bound to task {task_id}"
                )
            }
            Self::TaskAlreadyBound {
                task_id,
                session_id,
            } => {
                write!(
                    formatter,
                    "task {task_id} is already bound to session {session_id}"
                )
            }
            Self::CannotArchiveRunning(task_id) => {
                write!(formatter, "cannot archive running task {task_id}")
            }
        }
    }
}

impl std::error::Error for TaskError {}

/// Runtime task registry and stable task↔PTY lookup table.
#[derive(Debug, Default)]
pub struct TaskManager {
    tasks: Vec<AgentTask>,
    task_indices: HashMap<TaskId, usize>,
    tasks_by_session: HashMap<String, TaskId>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&mut self, new_task: NewTask) -> Result<TaskId, TaskError> {
        validate_new_task(&new_task)?;
        let id = TaskId::new();
        let now = unix_time_ms();
        let task = AgentTask {
            id,
            title: new_task.title.trim().to_string(),
            provider: new_task.provider,
            status: TaskStatus::Created,
            repo_root: new_task.repo_root,
            worktree_path: new_task.worktree_path,
            branch: new_task.branch,
            base_commit: new_task.base_commit,
            source: new_task.source_context.as_ref().map(|context| TaskSource {
                session_id: context.source_session_id.clone(),
                execution_id: context.source_execution_id.clone(),
            }),
            source_context: new_task.source_context,
            agent_session_id: None,
            exit_code: None,
            status_detail: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.task_indices.insert(id, self.tasks.len());
        self.tasks.push(task);
        Ok(id)
    }

    pub fn tasks(&self) -> &[AgentTask] {
        &self.tasks
    }

    pub fn get(&self, task_id: TaskId) -> Option<&AgentTask> {
        self.task_indices
            .get(&task_id)
            .and_then(|index| self.tasks.get(*index))
    }

    pub fn task_for_session(&self, session_id: &str) -> Option<&AgentTask> {
        self.tasks_by_session
            .get(session_id)
            .and_then(|task_id| self.get(*task_id))
    }

    /// Bind a freshly spawned PTY to a task. Existing bindings are immutable:
    /// replacement must be an explicit future lifecycle operation, never a
    /// side effect of a tab/index change.
    pub fn bind_agent_session(
        &mut self,
        task_id: TaskId,
        session_id: String,
    ) -> Result<(), TaskError> {
        if !crate::session::is_valid_jsh_session_id(&session_id) {
            return Err(TaskError::InvalidSessionId);
        }
        if let Some(existing_task_id) = self.tasks_by_session.get(&session_id).copied() {
            if existing_task_id == task_id {
                return Ok(());
            }
            return Err(TaskError::SessionAlreadyBound {
                session_id,
                task_id: existing_task_id,
            });
        }

        let task = self.task_mut(task_id)?;
        if let Some(existing_session_id) = &task.agent_session_id {
            return Err(TaskError::TaskAlreadyBound {
                task_id,
                session_id: existing_session_id.clone(),
            });
        }
        task.agent_session_id = Some(session_id.clone());
        // PTY creation returns only after chdir + exec crossed the startup
        // pipe, so a successful binding is already a reliable Working signal.
        task.status = TaskStatus::Working;
        task.status_detail = None;
        task.updated_at_ms = unix_time_ms();
        self.tasks_by_session.insert(session_id, task_id);
        Ok(())
    }

    pub fn update_status(
        &mut self,
        task_id: TaskId,
        status: TaskStatus,
        detail: Option<String>,
    ) -> Result<(), TaskError> {
        let task = self.task_mut(task_id)?;
        if task.status == TaskStatus::Archived {
            return Ok(());
        }
        task.status = status;
        task.status_detail = detail.filter(|value| !value.trim().is_empty());
        task.updated_at_ms = unix_time_ms();
        Ok(())
    }

    /// Apply the authoritative child-process result before its tab disappears.
    /// A zero exit only means the opaque Agent process finished; its worktree
    /// still requires human review and is never treated as accepted/merged.
    pub fn handle_session_exit(
        &mut self,
        session_id: &str,
        exit_code: Option<i32>,
    ) -> Option<TaskId> {
        let task_id = self.tasks_by_session.get(session_id).copied()?;
        let task = self.task_mut(task_id).ok()?;
        if matches!(
            task.status,
            TaskStatus::ReadyForReview
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Archived
        ) {
            return Some(task_id);
        }
        task.exit_code = exit_code;
        match exit_code {
            Some(0) => {
                task.status = TaskStatus::ReadyForReview;
                task.status_detail =
                    Some("Agent process finished; review its worktree changes".to_string());
            }
            Some(code) => {
                task.status = TaskStatus::Failed;
                task.status_detail = Some(format!("Agent process exited with code {code}"));
            }
            None => {
                task.status = TaskStatus::Failed;
                task.status_detail = Some("Agent process ended without an exit status".to_string());
            }
        }
        task.updated_at_ms = unix_time_ms();
        Some(task_id)
    }

    /// Record an explicit UI/session close when no child wait status was
    /// observed. This is not a process failure and must not leave the task
    /// looking perpetually active in the dashboard.
    pub fn handle_session_closed(&mut self, session_id: &str) -> Option<TaskId> {
        let task_id = self.tasks_by_session.get(session_id).copied()?;
        let task = self.task_mut(task_id).ok()?;
        if matches!(
            task.status,
            TaskStatus::ReadyForReview
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Archived
        ) {
            return Some(task_id);
        }
        task.status = TaskStatus::Cancelled;
        task.exit_code = None;
        task.status_detail = Some("Agent terminal was closed".to_string());
        task.updated_at_ms = unix_time_ms();
        Some(task_id)
    }

    pub fn archive(&mut self, task_id: TaskId) -> Result<(), TaskError> {
        let task = self.task_mut(task_id)?;
        if task.status.is_running() {
            return Err(TaskError::CannotArchiveRunning(task_id));
        }
        task.status = TaskStatus::Archived;
        task.updated_at_ms = unix_time_ms();
        Ok(())
    }

    fn task_mut(&mut self, task_id: TaskId) -> Result<&mut AgentTask, TaskError> {
        let index = self
            .task_indices
            .get(&task_id)
            .copied()
            .ok_or(TaskError::UnknownTask(task_id))?;
        self.tasks
            .get_mut(index)
            .ok_or(TaskError::UnknownTask(task_id))
    }
}

fn validate_new_task(task: &NewTask) -> Result<(), TaskError> {
    let title = task.title.trim();
    if title.is_empty() || title.len() > MAX_TASK_TITLE_BYTES || title.chars().any(char::is_control)
    {
        return Err(TaskError::InvalidTitle);
    }
    if task.branch.is_empty()
        || task.branch.len() > MAX_BRANCH_BYTES
        || task.branch.chars().any(char::is_control)
    {
        return Err(TaskError::InvalidBranch);
    }
    if !matches!(task.base_commit.len(), 40 | 64)
        || !task
            .base_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(TaskError::InvalidBaseCommit);
    }
    if !task.repo_root.is_absolute() {
        return Err(TaskError::RepoRootMustBeAbsolute);
    }
    if !task.worktree_path.is_absolute() {
        return Err(TaskError::WorktreePathMustBeAbsolute);
    }
    if task.repo_root == task.worktree_path {
        return Err(TaskError::WorktreeMatchesRepoRoot);
    }
    if task.source_context.as_ref().is_some_and(|context| {
        !crate::session::is_valid_jsh_session_id(&context.source_session_id)
            || context.source_execution_id.is_empty()
            || context.source_execution_id.len() > 256
            || context.source_execution_id.chars().any(char::is_control)
    }) {
        return Err(TaskError::InvalidSourceContext);
    }
    Ok(())
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::SemanticCommandContext;
    use std::path::Path;

    fn new_task(title: &str) -> NewTask {
        NewTask {
            title: title.to_string(),
            provider: AgentProvider::Codex,
            repo_root: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/tasks/task-one"),
            branch: "ember/task-one".to_string(),
            base_commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            source_context: Some(SemanticCommandContext {
                source_session_id: "source-session".to_string(),
                source_execution_id: "execution-7".to_string(),
                source_sequence: 7,
                command: Some("cargo test".to_string()),
                command_exact: true,
                command_truncated: false,
                cwd: Some("/repo".to_string()),
                cwd_after: Some("/repo".to_string()),
                exit_code: Some(101),
                duration_ms: Some(42),
                output_text: "test failed".to_string(),
                output_available: true,
                output_truncated: false,
                output_total_bytes: 11,
                started_at: None,
                finished_at: None,
            }),
        }
    }

    #[test]
    fn creates_owned_task_with_stable_identity() {
        let mut manager = TaskManager::new();
        let id = manager.create(new_task("Fix resize crash")).unwrap();
        let task = manager.get(id).unwrap();

        assert_eq!(task.id, id);
        assert_eq!(task.title, "Fix resize crash");
        assert_eq!(task.status, TaskStatus::Created);
        assert_eq!(task.provider.display_name(), "Codex");
        assert_eq!(task.worktree_path, Path::new("/tasks/task-one"));
        assert_eq!(task.source.as_ref().unwrap().execution_id, "execution-7");
        assert_eq!(
            task.source_context.as_ref().unwrap().command.as_deref(),
            Some("cargo test")
        );

        let serialized = serde_json::to_string(task).expect("task serializes");
        let restored: AgentTask = serde_json::from_str(&serialized).expect("task restores");
        assert_eq!(restored, task.clone());
    }

    #[test]
    fn rejects_ambiguous_or_unsafe_metadata() {
        let mut manager = TaskManager::new();
        let mut task = new_task("   ");
        assert_eq!(manager.create(task.clone()), Err(TaskError::InvalidTitle));

        task.title = "valid".to_string();
        task.branch = "bad\nbranch".to_string();
        assert_eq!(manager.create(task.clone()), Err(TaskError::InvalidBranch));

        task.branch = "ember/valid".to_string();
        task.base_commit = "short".to_string();
        assert_eq!(
            manager.create(task.clone()),
            Err(TaskError::InvalidBaseCommit)
        );

        task.base_commit = "0123456789abcdef0123456789abcdef01234567".to_string();
        task.repo_root = PathBuf::from("relative");
        assert_eq!(
            manager.create(task.clone()),
            Err(TaskError::RepoRootMustBeAbsolute)
        );

        task.repo_root = PathBuf::from("/same");
        task.worktree_path = PathBuf::from("/same");
        assert_eq!(
            manager.create(task),
            Err(TaskError::WorktreeMatchesRepoRoot)
        );
    }

    #[test]
    fn stable_session_binding_survives_unrelated_ui_reindexing() {
        let mut manager = TaskManager::new();
        let id = manager.create(new_task("task")).unwrap();
        manager
            .bind_agent_session(id, "stable-session-42".to_string())
            .unwrap();

        assert_eq!(
            manager
                .task_for_session("stable-session-42")
                .map(|task| task.id),
            Some(id)
        );
        assert!(manager.task_for_session("pane-index-0").is_none());
        assert_eq!(manager.get(id).unwrap().status, TaskStatus::Working);
    }

    #[test]
    fn session_cannot_be_silently_rebound() {
        let mut manager = TaskManager::new();
        let first = manager.create(new_task("first")).unwrap();
        let mut second_task = new_task("second");
        second_task.worktree_path = PathBuf::from("/tasks/task-two");
        second_task.branch = "ember/task-two".to_string();
        let second = manager.create(second_task).unwrap();

        manager
            .bind_agent_session(first, "agent-session".to_string())
            .unwrap();
        assert_eq!(
            manager.bind_agent_session(second, "agent-session".to_string()),
            Err(TaskError::SessionAlreadyBound {
                session_id: "agent-session".to_string(),
                task_id: first,
            })
        );
    }

    #[test]
    fn process_exit_becomes_review_or_failure_before_session_removal() {
        let mut manager = TaskManager::new();
        let success = manager.create(new_task("success")).unwrap();
        manager
            .bind_agent_session(success, "success-session".to_string())
            .unwrap();
        assert_eq!(
            manager.handle_session_exit("success-session", Some(0)),
            Some(success)
        );
        let task = manager.get(success).unwrap();
        assert_eq!(task.status, TaskStatus::ReadyForReview);
        assert_eq!(task.exit_code, Some(0));
        assert!(task.needs_attention());

        let mut failed_task = new_task("failed");
        failed_task.worktree_path = PathBuf::from("/tasks/failed");
        failed_task.branch = "ember/failed".to_string();
        let failed = manager.create(failed_task).unwrap();
        manager
            .bind_agent_session(failed, "failed-session".to_string())
            .unwrap();
        manager.handle_session_exit("failed-session", Some(17));
        let task = manager.get(failed).unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.exit_code, Some(17));
        assert!(task.status_detail.as_deref().unwrap().contains("17"));
    }

    #[test]
    fn disconnect_without_wait_status_fails_closed() {
        let mut manager = TaskManager::new();
        let id = manager.create(new_task("disconnected")).unwrap();
        manager
            .bind_agent_session(id, "disconnected-session".to_string())
            .unwrap();
        manager.handle_session_exit("disconnected-session", None);

        let task = manager.get(id).unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.exit_code, None);
        assert!(task.status_detail.as_deref().unwrap().contains("without"));
    }

    #[test]
    fn active_task_cannot_be_archived() {
        let mut manager = TaskManager::new();
        let id = manager.create(new_task("active")).unwrap();
        manager
            .bind_agent_session(id, "active-session".to_string())
            .unwrap();

        assert_eq!(
            manager.archive(id),
            Err(TaskError::CannotArchiveRunning(id))
        );
        manager.handle_session_exit("active-session", Some(0));
        manager.archive(id).unwrap();
        assert_eq!(manager.get(id).unwrap().status, TaskStatus::Archived);
    }

    #[test]
    fn manually_closed_agent_session_becomes_cancelled() {
        let mut manager = TaskManager::new();
        let id = manager.create(new_task("cancelled")).unwrap();
        manager
            .bind_agent_session(id, "cancelled-session".to_string())
            .unwrap();

        assert_eq!(manager.handle_session_closed("cancelled-session"), Some(id));
        let task = manager.get(id).unwrap();
        assert_eq!(task.status, TaskStatus::Cancelled);
        assert_eq!(task.exit_code, None);

        // A later channel disconnect must not overwrite the explicit reason.
        manager.handle_session_exit("cancelled-session", None);
        assert_eq!(manager.get(id).unwrap().status, TaskStatus::Cancelled);
    }
}
