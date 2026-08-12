//! Runtime owner for provider-native Agent sessions.
//!
//! [`TaskManager`] remains the authoritative lifecycle reducer. This module
//! owns the live provider adapters, drains their already-bounded event queues
//! with an additional frame budget, and retains the adapter's bounded view
//! after its worker has stopped. No method here waits for a running worker.

use super::drivers::codex_app_server::{
    CodexAppServerDriver, CodexAppServerExitCause, CodexAppServerExitReport,
    CodexAppServerViewSnapshot,
};
use super::native::{
    build_native_task_prompt, prepare_native_agent_workspace, NativePromptError,
    NativePromptPolicy, NativeWorkspaceError, PreparedNativeCodexHome,
};
use super::{
    AgentCommand, AgentDriver, AgentDriverError, AgentEventError, AgentEventStream,
    AgentLaunchError, AgentLaunchSpec, AgentProvider, AgentSessionOutcome, AgentStartRequest,
    ApprovalDecision, ApprovalId, NativeCodexHomeError, TaskId, TaskManager,
};
use std::collections::HashMap;
use std::fmt;

/// Global event-work limit for one UI frame.
///
/// Driver events are independently capped at 64 KiB, so this also places a
/// conservative 4 MiB upper bound on provider event data inspected per frame.
pub const NATIVE_AGENT_EVENTS_PER_FRAME: usize = 64;
/// Prevent one noisy provider from consuming the whole global frame budget.
pub const NATIVE_AGENT_EVENTS_PER_TASK_PER_FRAME: usize = 16;

struct RunningCodexAgent {
    driver: CodexAppServerDriver,
    stream: AgentEventStream,
    worker_joined: bool,
    forced_failure: Option<String>,
    exit_report: Option<CodexAppServerExitReport>,
}

struct RetainedCodexAgent {
    view: CodexAppServerViewSnapshot,
    exit_report: Option<CodexAppServerExitReport>,
}

/// Process-local owner of native provider adapters and their bounded UI views.
///
/// Runtime instances are deliberately not serialized. Stable task metadata is
/// owned by [`TaskManager`]; provider workers and descriptor capabilities must
/// be recreated explicitly after process restart.
#[derive(Default)]
pub struct AgentRuntimeManager {
    running: HashMap<TaskId, RunningCodexAgent>,
    retained: HashMap<TaskId, RetainedCodexAgent>,
    next_poll_index: usize,
}

#[derive(Debug)]
pub enum AgentRuntimeError {
    UnknownTask(TaskId),
    AlreadyRunning(TaskId),
    NotRunning(TaskId),
    UnsupportedProvider {
        task_id: TaskId,
        provider: AgentProvider,
    },
    Workspace(NativeWorkspaceError),
    Prompt(NativePromptError),
    NativeHome(NativeCodexHomeError),
    Launch(AgentLaunchError),
    Driver(AgentDriverError),
    Event(AgentEventError),
    StartRollback {
        start: Box<AgentRuntimeError>,
        rollback: AgentEventError,
    },
}

impl fmt::Display for AgentRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTask(task_id) => write!(formatter, "Agent task {task_id} is unavailable"),
            Self::AlreadyRunning(task_id) => {
                write!(formatter, "native Agent task {task_id} is already running")
            }
            Self::NotRunning(task_id) => {
                write!(formatter, "native Agent task {task_id} is not running")
            }
            Self::UnsupportedProvider { task_id, provider } => write!(
                formatter,
                "native Agent task {task_id} uses unsupported provider {}",
                provider.display_name()
            ),
            Self::Workspace(error) => error.fmt(formatter),
            Self::Prompt(error) => error.fmt(formatter),
            Self::NativeHome(error) => error.fmt(formatter),
            Self::Launch(error) => error.fmt(formatter),
            Self::Driver(error) => error.fmt(formatter),
            Self::Event(error) => error.fmt(formatter),
            Self::StartRollback { start, rollback } => write!(
                formatter,
                "{start}; native stream rollback also failed: {rollback}"
            ),
        }
    }
}

impl std::error::Error for AgentRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Workspace(error) => Some(error),
            Self::Prompt(error) => Some(error),
            Self::NativeHome(error) => Some(error),
            Self::Launch(error) => Some(error),
            Self::Driver(error) => Some(error),
            Self::Event(error) => Some(error),
            Self::StartRollback { start, .. } => Some(start.as_ref()),
            Self::UnknownTask(_)
            | Self::AlreadyRunning(_)
            | Self::NotRunning(_)
            | Self::UnsupportedProvider { .. } => None,
        }
    }
}

impl From<NativeWorkspaceError> for AgentRuntimeError {
    fn from(error: NativeWorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

impl From<NativePromptError> for AgentRuntimeError {
    fn from(error: NativePromptError) -> Self {
        Self::Prompt(error)
    }
}

impl From<NativeCodexHomeError> for AgentRuntimeError {
    fn from(error: NativeCodexHomeError) -> Self {
        Self::NativeHome(error)
    }
}

impl From<AgentLaunchError> for AgentRuntimeError {
    fn from(error: AgentLaunchError) -> Self {
        Self::Launch(error)
    }
}

impl From<AgentDriverError> for AgentRuntimeError {
    fn from(error: AgentDriverError) -> Self {
        Self::Driver(error)
    }
}

impl From<AgentEventError> for AgentRuntimeError {
    fn from(error: AgentEventError) -> Self {
        Self::Event(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRuntimeIssue {
    pub task_id: TaskId,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentRuntimePollReport {
    pub events_drained: usize,
    pub events_applied: usize,
    pub workers_finished: usize,
    pub budget_exhausted: bool,
    pub issues: Vec<AgentRuntimeIssue>,
    pub completions: Vec<AgentRuntimeCompletion>,
}

impl AgentRuntimePollReport {
    pub fn made_progress(&self) -> bool {
        self.events_drained > 0 || self.workers_finished > 0 || !self.issues.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRuntimeCompletion {
    pub task_id: TaskId,
    pub outcome: AgentSessionOutcome,
    pub cause: CodexAppServerExitCause,
    pub detail: Option<String>,
}

impl AgentRuntimeManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a real Codex app-server session for one registered task.
    ///
    /// Workspace identity and prompt consent are checked before selecting the
    /// task's runtime. Once the native stream exists, every later failure
    /// converges through TaskManager only after the worker's stop authority is
    /// proven; only a synchronous pre-worker failure is rolled back to Created.
    pub fn start_codex(
        &mut self,
        task_manager: &mut TaskManager,
        task_id: TaskId,
        policy: NativePromptPolicy,
    ) -> Result<(), AgentRuntimeError> {
        if self.running.contains_key(&task_id) {
            return Err(AgentRuntimeError::AlreadyRunning(task_id));
        }
        let task = task_manager
            .get(task_id)
            .cloned()
            .ok_or(AgentRuntimeError::UnknownTask(task_id))?;
        if task.provider != AgentProvider::Codex {
            return Err(AgentRuntimeError::UnsupportedProvider {
                task_id,
                provider: task.provider,
            });
        }

        // The capability owns its pinned directory descriptor. Move the whole
        // value into the driver; never retain only its raw descriptor here.
        let workspace = prepare_native_agent_workspace(&task)?;
        let prompt = build_native_task_prompt(&task, workspace.relative_cwd(), policy)?;
        // Resolve every fallible host prerequisite before selecting the native
        // runtime. A missing or unsafe executable leaves a Created task
        // retryable instead of manufacturing a failed stream incarnation.
        let launch_argv = AgentLaunchSpec::resolve_native(
            AgentProvider::Codex,
            &task.repo_root,
            &task.worktree_path,
        )?;
        let native_home = PreparedNativeCodexHome::prepare()?;
        let stream = task_manager.start_agent_event_stream(task_id)?;

        let mut driver = CodexAppServerDriver::new(launch_argv, workspace, native_home);
        let request = AgentStartRequest {
            provider: AgentProvider::Codex,
            stream: stream.clone(),
            worktree_path: task.worktree_path,
            // The prompt contains the policy-approved, redacted evidence. Do
            // not also hand the adapter the raw semantic snapshot.
            source_context: None,
            initial_prompt: Some(prompt),
            resume_from: None,
        };
        if let Err(error) = driver.start(request) {
            // CodexAppServerDriver::start has a strict pre-spawn error
            // contract: once it has created a worker it returns Ok and reports
            // every later spawn/protocol/process failure asynchronously. That
            // makes this stopped-runtime rollback authoritative.
            driver.cancel();
            return Err(rollback_failed_start(
                task_manager,
                &stream,
                AgentRuntimeError::Driver(error),
            ));
        }

        self.retained.remove(&task_id);
        self.running.insert(
            task_id,
            RunningCodexAgent {
                driver,
                stream,
                worker_joined: false,
                forced_failure: None,
                exit_report: None,
            },
        );
        Ok(())
    }

    /// Drain native events without waiting for future provider work.
    ///
    /// A completed worker is joined only after `worker_is_finished` reports
    /// true and its event queue has been drained for this frame. The exit report
    /// is accepted as stop authority only when no child was spawned or the
    /// spawned child was reaped.
    pub fn poll(&mut self, task_manager: &mut TaskManager) -> AgentRuntimePollReport {
        let mut report = AgentRuntimePollReport::default();
        let mut task_ids: Vec<_> = self.running.keys().copied().collect();
        if task_ids.is_empty() {
            self.next_poll_index = 0;
            return report;
        }

        let start = self.next_poll_index % task_ids.len();
        task_ids.rotate_left(start);
        self.next_poll_index = (start + 1) % task_ids.len();
        let mut remaining = NATIVE_AGENT_EVENTS_PER_FRAME;

        for task_id in task_ids {
            if remaining == 0 {
                report.budget_exhausted = true;
                break;
            }
            let allowance = remaining.min(NATIVE_AGENT_EVENTS_PER_TASK_PER_FRAME);
            let mut queue_drained = false;
            let mut completion = None;

            {
                let Some(runtime) = self.running.get_mut(&task_id) else {
                    continue;
                };
                for _ in 0..allowance {
                    match runtime.driver.try_next_event() {
                        Ok(Some(event)) => {
                            remaining -= 1;
                            report.events_drained += 1;

                            // Once a terminal event has removed the stream,
                            // discard any provider protocol tail. It has no
                            // remaining lifecycle authority.
                            if runtime.forced_failure.is_some()
                                || !task_manager.has_active_agent_event_stream(task_id)
                            {
                                continue;
                            }
                            match task_manager.apply_agent_event(event) {
                                Ok(_) => report.events_applied += 1,
                                Err(error) => {
                                    let detail = bounded_runtime_detail(format!(
                                        "native Agent event was rejected: {error}"
                                    ));
                                    if runtime.forced_failure.is_none() {
                                        runtime.forced_failure = Some(detail.clone());
                                    }
                                    runtime.driver.cancel();
                                    report.issues.push(AgentRuntimeIssue { task_id, detail });
                                }
                            }
                        }
                        Ok(None) | Err(AgentDriverError::Closed) => {
                            queue_drained = true;
                            break;
                        }
                        Err(error) => {
                            let detail = bounded_runtime_detail(format!(
                                "native Agent event transport failed: {error}"
                            ));
                            if runtime.forced_failure.is_none() {
                                runtime.forced_failure = Some(detail.clone());
                            }
                            runtime.driver.cancel();
                            report.issues.push(AgentRuntimeIssue { task_id, detail });
                            queue_drained = true;
                            break;
                        }
                    }
                }

                let worker_stopped = runtime.worker_joined || runtime.driver.worker_is_finished();
                if queue_drained && worker_stopped {
                    if !runtime.worker_joined {
                        match runtime.driver.join_finished_worker() {
                            Ok(true) => runtime.worker_joined = true,
                            Ok(false) => {}
                            Err(error) => {
                                runtime.worker_joined = true;
                                let detail = bounded_runtime_detail(format!(
                                    "native Agent worker join failed: {error}"
                                ));
                                if runtime.forced_failure.is_none() {
                                    runtime.forced_failure = Some(detail.clone());
                                }
                                report.issues.push(AgentRuntimeIssue { task_id, detail });
                            }
                        }
                    }
                    if runtime.worker_joined && runtime.exit_report.is_none() {
                        runtime.exit_report = runtime.driver.take_exit_report();
                    }
                    if runtime.exit_report.as_ref().is_some_and(|exit| {
                        !exit.process.spawned
                            || (exit.process.reaped && exit.process.containment_verified_empty)
                    }) {
                        let exit = runtime
                            .exit_report
                            .take()
                            .expect("exit report was checked above");
                        let outcome = if runtime.forced_failure.is_some() {
                            AgentSessionOutcome::Failed
                        } else {
                            exit.outcome
                        };
                        let detail = runtime
                            .forced_failure
                            .clone()
                            .or_else(|| exit.detail.clone());
                        completion = Some((
                            runtime.stream.clone(),
                            runtime.driver.view_snapshot(),
                            exit,
                            outcome,
                            detail,
                        ));
                    }
                }
            }

            let Some((stream, view, exit_report, outcome, detail)) = completion else {
                continue;
            };
            // The report proves the worker has stopped and its child either was
            // never spawned or was reaped. Only now may validation be unlocked.
            if task_manager.has_active_agent_event_stream(task_id) {
                if let Err(error) = task_manager.finish_agent_event_stream_after_stop(
                    &stream,
                    outcome,
                    detail.clone(),
                ) {
                    report.issues.push(AgentRuntimeIssue {
                        task_id,
                        detail: bounded_runtime_detail(format!(
                            "native Agent exit could not update its task: {error}"
                        )),
                    });
                }
            }
            self.running.remove(&task_id);
            report.completions.push(AgentRuntimeCompletion {
                task_id,
                outcome,
                cause: exit_report.cause,
                detail: detail.clone(),
            });
            self.retained.insert(
                task_id,
                RetainedCodexAgent {
                    view,
                    exit_report: Some(exit_report),
                },
            );
            report.workers_finished += 1;
        }

        // Consuming the full global allowance means more provider work may be
        // queued even if the final dequeue happened to empty a queue exactly.
        report.budget_exhausted |= remaining == 0;
        report
    }

    pub fn cancel(&mut self, task_id: TaskId) -> Result<(), AgentRuntimeError> {
        let runtime = self
            .running
            .get_mut(&task_id)
            .ok_or(AgentRuntimeError::NotRunning(task_id))?;
        runtime.driver.cancel();
        Ok(())
    }

    pub fn decide_approval(
        &mut self,
        task_id: TaskId,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    ) -> Result<(), AgentRuntimeError> {
        let runtime = self
            .running
            .get_mut(&task_id)
            .ok_or(AgentRuntimeError::NotRunning(task_id))?;
        runtime
            .driver
            .send(AgentCommand::DecideApproval {
                id: approval_id,
                decision,
            })
            .map_err(AgentRuntimeError::Driver)
    }

    /// Return the current bounded view, or the final view retained after exit.
    pub fn snapshot(&self, task_id: TaskId) -> Option<CodexAppServerViewSnapshot> {
        self.running
            .get(&task_id)
            .map(|runtime| runtime.driver.view_snapshot())
            .or_else(|| {
                self.retained
                    .get(&task_id)
                    .map(|retained| retained.view.clone())
            })
    }

    pub fn exit_report(&self, task_id: TaskId) -> Option<&CodexAppServerExitReport> {
        self.retained
            .get(&task_id)
            .and_then(|retained| retained.exit_report.as_ref())
    }

    pub fn take_exit_report(&mut self, task_id: TaskId) -> Option<CodexAppServerExitReport> {
        self.retained
            .get_mut(&task_id)
            .and_then(|retained| retained.exit_report.take())
    }

    pub fn has_running(&self, task_id: TaskId) -> bool {
        self.running.contains_key(&task_id)
    }

    pub fn has_any_running(&self) -> bool {
        !self.running.is_empty()
    }

    pub fn clear_retained(&mut self, task_id: TaskId) {
        self.retained.remove(&task_id);
    }
}

impl Drop for AgentRuntimeManager {
    fn drop(&mut self) {
        for runtime in self.running.values_mut() {
            runtime.driver.cancel();
        }
    }
}

fn rollback_failed_start(
    task_manager: &mut TaskManager,
    stream: &AgentEventStream,
    start: AgentRuntimeError,
) -> AgentRuntimeError {
    let detail = bounded_runtime_detail(start.to_string());
    match task_manager.rollback_agent_event_stream_before_spawn(stream, detail) {
        Ok(_) => start,
        Err(rollback) => AgentRuntimeError::StartRollback {
            start: Box::new(start),
            rollback,
        },
    }
}

fn bounded_runtime_detail(detail: String) -> String {
    super::event::bounded_event_detail(Some(detail))
        .unwrap_or_else(|| "native Agent runtime failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_runtime_has_no_running_or_retained_state() {
        let mut runtime = AgentRuntimeManager::new();
        let task_id = TaskId::new();
        assert!(!runtime.has_running(task_id));
        assert!(!runtime.has_any_running());
        assert!(runtime.snapshot(task_id).is_none());
        assert!(runtime.exit_report(task_id).is_none());
        assert!(matches!(
            runtime.cancel(task_id),
            Err(AgentRuntimeError::NotRunning(id)) if id == task_id
        ));
    }

    #[test]
    fn empty_poll_is_bounded_and_idle() {
        let mut runtime = AgentRuntimeManager::new();
        let mut tasks = TaskManager::new();
        let report = runtime.poll(&mut tasks);
        assert_eq!(report, AgentRuntimePollReport::default());
        assert!(!report.made_progress());
    }
}
