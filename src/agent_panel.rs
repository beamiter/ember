//! Agent mode — a multi-turn LLM that proposes shell commands, watches their
//! output, and iterates. UI is egui; the protocol state machine, provider
//! client, transport, and redaction all live in `jterm_core` (shared with
//! anvil/forge).
//!
//! ## Safety model (immutable, by design)
//!
//! 1. **Per-command approval.** Every proposed command renders as an
//!    Approve/Edit/Reject card; nothing reaches the PTY without a click.
//! 2. **Dangerous-command flagging.** `jterm_core::agent::is_dangerous`
//!    switches the approve button to a destructive style.
//! 3. **Single session, single binding.** The panel drives at most one
//!    session, bound to the terminal session it was opened on; command
//!    completions from other sessions are ignored.
//! 4. **Turn cap.** `agent_max_turns` bounds runaway loops.
//! 5. **Strict correlation.** An OSC 133 completion only becomes an
//!    observation when its reported command text matches the approved one.

use crate::config::Config;
use crate::terminal::CompletedCommandOutput;
use jterm_core::agent::{
    is_dangerous, AgentSession, AgentSessionSnapshot, AgentSnapshotError, AgentState, ModelOutcome,
    ProposalId, ProposalStatus, Turn, MAX_AGENT_SNAPSHOT_JSON_BYTES,
};
use jterm_core::ai::{AiCancellationToken, AiClient, BlockContext, Provider};
use std::path::Path;
use std::sync::mpsc;

const MAX_AGENT_MODEL_REPLY_BYTES: usize = 128 * 1024;

fn snapshot_path() -> Option<std::path::PathBuf> {
    Some(
        dirs::config_dir()?
            .join("ember")
            .join("agent_session.json"),
    )
}

/// Read an Agent snapshot through ember's hardened persistence boundary.
/// Failures are deliberately best-effort so a hostile or corrupt entry never
/// prevents opening a fresh Agent session. Production restores go through
/// [`claim_snapshot_session`], which claims the file before reading it.
#[cfg(test)]
fn read_snapshot_file(path: &Path) -> Option<AgentSessionSnapshot> {
    let encoded =
        crate::persistence_file::read_bounded(path, MAX_AGENT_SNAPSHOT_JSON_BYTES as u64).ok()?;
    AgentSessionSnapshot::from_json(&encoded).ok()
}

/// Atomically claim the persisted snapshot and consume it into a session.
///
/// A read followed by a separate delete lets two windows opening at the same
/// moment both restore the same transcript, and loses the session entirely if
/// the process dies between the two calls. Claiming first means exactly one
/// caller ever sees the file. Evidence that cannot become a session is left at
/// the claim path instead of being deleted, so a corrupt or hostile snapshot
/// stays available for inspection and is never restored by a later opener.
fn claim_snapshot_session(path: &Path) -> Option<AgentSession> {
    let claimed = crate::persistence_file::claim_exclusive(path).ok()?;
    let restored =
        crate::persistence_file::read_bounded(&claimed, MAX_AGENT_SNAPSHOT_JSON_BYTES as u64)
            .ok()
            .and_then(|encoded| AgentSessionSnapshot::from_json(&encoded).ok())
            .and_then(|snapshot| restore_snapshot_session(snapshot).ok());
    match restored {
        Some(session) => {
            let _ = std::fs::remove_file(&claimed);
            Some(session)
        }
        None => {
            log::warn!(
                "agent: quarantined an unusable session snapshot at {}",
                claimed.display()
            );
            None
        }
    }
}

fn restore_snapshot_session(
    snapshot: AgentSessionSnapshot,
) -> Result<AgentSession, AgentSnapshotError> {
    let session = AgentSession::restore(snapshot)?;
    let mut proposal_ids = std::collections::HashSet::new();
    let mut pending = Vec::new();
    for turn in session.transcript() {
        if let Turn::AssistantProposed {
            id,
            command,
            status,
        } = turn
        {
            if crate::review_text::validate_single_line(
                command,
                crate::review_text::MAX_AGENT_COMMAND_BYTES,
            )
            .is_err()
            {
                return Err(AgentSnapshotError::Invalid(
                    "proposal command is unsafe to display or execute",
                ));
            }
            if !proposal_ids.insert(id.get()) {
                return Err(AgentSnapshotError::Invalid("duplicate proposal id"));
            }
            if *status == ProposalStatus::Pending {
                pending.push(*id);
            }
        }
    }
    match session.state() {
        AgentState::AwaitingApproval { proposal_id } if pending.as_slice() == [proposal_id] => {}
        AgentState::AwaitingApproval { .. } => {
            return Err(AgentSnapshotError::Invalid(
                "pending proposal state does not match transcript",
            ));
        }
        _ if !pending.is_empty() => {
            return Err(AgentSnapshotError::Invalid(
                "pending proposal exists outside approval state",
            ));
        }
        _ => {}
    }
    Ok(session)
}

fn proposal_command(session: &AgentSession, proposal_id: ProposalId) -> Option<&str> {
    session.transcript().iter().find_map(|turn| match turn {
        Turn::AssistantProposed { id, command, .. } if *id == proposal_id => Some(command.as_str()),
        _ => None,
    })
}

/// The pinned jagent parser mutates the transcript before returning its
/// proposal. Keep a safe checkpoint so a command rejected only by the newer
/// visual-spoof contract never survives long enough to reach the next frame.
fn accept_model_reply_compat(session: &mut AgentSession, raw: &str) -> Result<(), String> {
    let checkpoint = session.snapshot();
    let outcome = session
        .accept_model_reply(raw)
        .map_err(|error| error.to_string())?;
    let ModelOutcome::Proposal { command, .. } = outcome else {
        return Ok(());
    };
    let Err(error) = crate::review_text::validate_single_line(
        &command,
        crate::review_text::MAX_AGENT_COMMAND_BYTES,
    ) else {
        return Ok(());
    };

    let message = format!("model proposal rejected before display: {error}");
    if let Some(snapshot) = checkpoint {
        match restore_snapshot_session(snapshot) {
            Ok(mut restored) => {
                let _ = restored.model_failed(message.clone());
                *session = restored;
            }
            Err(_) => session.cancel(),
        }
    } else {
        session.cancel();
    }
    Err(message)
}

/// Serialize under jagent's exact snapshot budget, then use ember's
/// create-new, same-directory atomic replacement instead of the pinned core's
/// legacy predictable staging name.
fn write_snapshot_file(
    path: &Path,
    snapshot: &AgentSessionSnapshot,
) -> Result<(), AgentSnapshotError> {
    let encoded = snapshot.to_json()?;
    if encoded.len() > MAX_AGENT_SNAPSHOT_JSON_BYTES {
        return Err(AgentSnapshotError::TooLarge {
            limit: MAX_AGENT_SNAPSHOT_JSON_BYTES,
        });
    }
    crate::persistence_file::write_atomic(path, encoded.as_bytes())
        .map_err(|error| AgentSnapshotError::Encode(format!("write {}: {error}", path.display())))
}

/// Side effects the panel asks the app to perform. The panel itself never
/// touches a PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEffect {
    /// Write this protocol-validated single-line command plus a carriage
    /// return to the bound session's input queue.
    RunCommand {
        session_id: String,
        command: String,
        generation: u64,
    },
}

#[derive(Clone, Debug)]
struct PendingAgentExecution {
    proposal_id: ProposalId,
    command: String,
    generation: u64,
}

fn client_from_config(config: &Config) -> Result<AiClient, String> {
    if !config.ai_enabled {
        return Err("AI features are disabled by configuration".to_string());
    }
    let provider = config
        .ai_provider
        .parse::<Provider>()
        .map_err(|error| error.to_string())?;
    let app_key_name = format!(
        "{}_AI_API_KEY",
        jterm_core::identity::get().app_name.to_ascii_uppercase()
    );
    let provider_key_name = match provider {
        Provider::Anthropic => "ANTHROPIC_API_KEY",
        Provider::OpenAiCompatible => "OPENAI_API_KEY",
        Provider::Ollama => "OLLAMA_API_KEY",
    };
    let nonempty_env = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let api_key = match nonempty_env(&app_key_name).or_else(|| nonempty_env(provider_key_name)) {
        Some(key) => Some(key),
        None => jterm_core::ai::resolve_api_key_file(config.ai_api_key_file.as_deref())
            .as_deref()
            .map(crate::persistence_file::read_api_key_file)
            .transpose()
            .map_err(|error| format!("AI API key file: {error}"))?,
    };
    AiClient::new(
        provider,
        api_key,
        config.ai_model.clone(),
        config.ai_base_url.clone(),
        config.ai_max_tokens,
        config.ai_temperature,
        config.ai_redact_secrets,
    )
    .map_err(|error| error.to_string())
}

pub struct AgentPanel {
    pub is_open: bool,
    session: Option<AgentSession>,
    bound_session_id: Option<String>,
    /// Approved proposal currently executing in the bound session.
    awaiting: Option<PendingAgentExecution>,
    /// Most recent command the user ran manually in the bound session while
    /// the panel was open. Attached to model requests as untrusted block
    /// context so "why did that fail?" has something to look at.
    last_manual_completed: Option<BlockContext>,
    input: String,
    /// Proposal currently being edited inline: (id, buffer).
    edit: Option<(ProposalId, String)>,
    loading: bool,
    status: String,
    provider_label: String,
    result_rx: Option<mpsc::Receiver<Result<String, String>>>,
    /// Task generation the in-flight request was started for. A reply that
    /// lands after New Task, a restore, or a session replacement belongs to a
    /// transcript that no longer exists and must not be accepted.
    request_epoch: Option<jterm_core::agent::AgentSessionEpoch>,
    cancel: Option<AiCancellationToken>,
    execution_generation: u64,
}

impl AgentPanel {
    pub fn new() -> Self {
        Self {
            is_open: false,
            session: None,
            bound_session_id: None,
            awaiting: None,
            last_manual_completed: None,
            input: String::new(),
            edit: None,
            loading: false,
            status: String::new(),
            provider_label: String::new(),
            result_rx: None,
            request_epoch: None,
            cancel: None,
            execution_generation: 0,
        }
    }

    /// Open the panel bound to stable `session_id`, replacing any previous
    /// session (whose in-flight request is cancelled first). A snapshot
    /// persisted by the previous run is restored one-shot and rebound to the
    /// current terminal session.
    pub fn open(&mut self, config: &Config, session_id: String) {
        self.close_session();
        self.is_open = true;
        self.bound_session_id = Some(session_id);
        self.status.clear();
        let restored = snapshot_path().and_then(|path| claim_snapshot_session(&path));
        match restored {
            Some(session) => {
                self.session = Some(session);
                self.status = "restored the previous agent session".to_string();
            }
            None => self.session = Some(AgentSession::new(config.agent_max_turns)),
        }
        match client_from_config(config) {
            Ok(client) => self.provider_label = client.display_name(),
            Err(error) => {
                self.provider_label.clear();
                self.status = error;
            }
        }
    }

    /// Persist the live session (if any) for the next run. Called on app
    /// exit, before the session is dropped.
    pub fn persist(&self) {
        let Some(path) = snapshot_path() else {
            return;
        };
        if self.session.as_ref().is_some_and(|session| {
            session.transcript().iter().any(|turn| {
                matches!(
                    turn,
                    Turn::AssistantProposed { command, .. }
                        if crate::review_text::validate_single_line(
                            command,
                            crate::review_text::MAX_AGENT_COMMAND_BYTES,
                        )
                        .is_err()
                )
            })
        }) {
            log::warn!("agent: refusing to persist an unsafe proposal command");
            jterm_core::agent::remove_snapshot_file(&path);
            return;
        }
        match self.session.as_ref().and_then(|session| session.snapshot()) {
            Some(snapshot) => {
                if let Err(error) = write_snapshot_file(&path, &snapshot) {
                    log::warn!("agent: could not persist session: {error}");
                }
            }
            None => jterm_core::agent::remove_snapshot_file(&path),
        }
    }

    pub fn toggle(&mut self, config: &Config, session_id: String) {
        if self.is_open {
            self.close();
        } else {
            self.open(config, session_id);
        }
    }

    /// Close the panel and cancel the whole session.
    pub fn close(&mut self) {
        self.close_session();
        self.is_open = false;
    }

    fn close_session(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        if let Some(session) = self.session.as_mut() {
            session.cancel();
        }
        self.session = None;
        self.bound_session_id = None;
        self.awaiting = None;
        self.last_manual_completed = None;
        self.result_rx = None;
        self.loading = false;
        self.edit = None;
    }

    /// Advance the session: harvest a finished LLM reply and start the next
    /// request when the protocol is waiting on the model. Called every frame
    /// from the app loop; cheap when idle.
    pub fn drive(&mut self, config: &Config, cwd: Option<&str>, shell: &str) {
        if !self.is_open {
            return;
        }
        if let Some(rx) = &self.result_rx {
            match rx.try_recv() {
                Ok(result) => {
                    self.result_rx = None;
                    self.cancel = None;
                    self.loading = false;
                    let expected_epoch = self.request_epoch.take();
                    if let Some(session) = self
                        .session
                        .as_mut()
                        .filter(|session| expected_epoch == Some(session.epoch()))
                    {
                        let outcome = match result {
                            Ok(raw) if raw.len() <= MAX_AGENT_MODEL_REPLY_BYTES => {
                                accept_model_reply_compat(session, &raw)
                            }
                            Ok(_) => session
                                .model_failed(format!(
                                    "AI reply exceeded the {MAX_AGENT_MODEL_REPLY_BYTES}-byte limit"
                                ))
                                .map(|_| ())
                                .map_err(|error| error.to_string()),
                            Err(error) => session
                                .model_failed(error)
                                .map(|_| ())
                                .map_err(|error| error.to_string()),
                        };
                        if let Err(error) = outcome {
                            self.status = error;
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.result_rx = None;
                    self.cancel = None;
                    self.loading = false;
                    if let Some(session) = self.session.as_mut() {
                        let _ = session.model_failed("AI worker thread exited unexpectedly");
                    }
                }
            }
        }
        let needs_model = self
            .session
            .as_ref()
            .is_some_and(|session| session.state() == AgentState::AwaitingModel);
        if needs_model && !self.loading && self.result_rx.is_none() {
            self.request_model(config, cwd, shell);
        }
    }

    fn request_model(&mut self, config: &Config, cwd: Option<&str>, shell: &str) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let client = match client_from_config(config) {
            Ok(client) => client,
            Err(error) => {
                self.status = error.clone();
                if let Some(session) = self.session.as_mut() {
                    let _ = session.model_failed(error);
                }
                return;
            }
        };
        self.provider_label = client.display_name();
        let system = jterm_core::ai::build_agent_system_prompt();
        // Cached repo probe with a bounded UI wait; None outside a repo.
        let git = cwd.and_then(|cwd| jterm_core::git_meta::read(std::path::Path::new(cwd)));
        let user = jterm_core::ai::agent_user_prompt(
            &session.build_user_prompt(),
            cwd.unwrap_or("."),
            shell,
            std::env::consts::OS,
            git.as_ref(),
            self.last_manual_completed.as_ref(),
        );
        let token = AiCancellationToken::new();
        let worker_token = token.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = client
                .send_turns_blocking_cancellable(
                    Some(&system),
                    &[jterm_core::ai::Turn {
                        role: jterm_core::ai::Role::User,
                        text: user,
                    }],
                    &worker_token,
                )
                .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
        self.cancel = Some(token);
        self.result_rx = Some(rx);
        self.request_epoch = self.session.as_ref().map(|session| session.epoch());
        self.loading = true;
        self.status.clear();
    }

    /// Feed one OSC 133 command completion from the bound session. A
    /// completion matching the approved proposal becomes an observation;
    /// anything else is remembered as the user's most recent manual command
    /// and attached to later model requests as untrusted block context.
    pub fn handle_completed(&mut self, session_id: &str, completed: &CompletedCommandOutput) {
        if !self.is_open || self.bound_session_id.as_deref() != Some(session_id) {
            return;
        }
        let output = if completed.output_available {
            completed.output.as_str()
        } else {
            "(command output was not captured)"
        };
        let reported = completed.command.as_deref();
        if let Some(pending) = self.awaiting.as_ref() {
            if completed.agent_generation == Some(pending.generation) {
                if completed.command.as_deref() != Some(pending.command.as_str()) {
                    let generation = pending.generation;
                    self.execution_start_failed(
                        generation,
                        "Agent stopped: approved command completion failed strict correlation",
                    );
                    return;
                }
                let Some(exit_code) = completed.exit_code else {
                    let generation = pending.generation;
                    self.execution_start_failed(
                        generation,
                        "Agent stopped: approved command completion had no exit status",
                    );
                    return;
                };
                let Some(pending) = self.awaiting.take() else {
                    return;
                };
                if let Some(session) = self.session.as_mut() {
                    match session.observe(pending.proposal_id, exit_code, output) {
                        Ok(()) => {}
                        Err(error) => self.status = error.to_string(),
                    }
                }
                return;
            }
        }
        if completed.agent_generation.is_some() {
            return;
        }
        let Some(command) = reported else {
            return;
        };
        let Ok(command) = crate::review_text::sanitize_history_replay(
            command,
            crate::review_text::MAX_HISTORY_COMMAND_BYTES,
        ) else {
            return;
        };
        let command = command
            .trim_matches(|character| matches!(character, ' ' | '\n' | '\t'))
            .to_string();
        if command.is_empty() {
            return;
        }
        self.last_manual_completed = Some(BlockContext {
            cmd: command,
            output: output.to_string(),
            cwd: completed.cwd.clone(),
            exit_code: completed.exit_code.unwrap_or(1),
            truncated: completed.truncated,
        });
    }

    /// Render the panel. Returns effects for the app to apply (PTY writes).
    pub fn show(&mut self, ctx: &egui::Context) -> Vec<AgentEffect> {
        let mut effects = Vec::new();
        if !self.is_open {
            return effects;
        }
        if self.loading {
            // A worker thread finishes without producing an egui event; keep
            // ticking so `drive` can harvest the reply promptly.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        let mut open = self.is_open;
        let mut submit = false;
        let mut approve: Option<(ProposalId, Option<String>)> = None;
        let mut reject: Option<ProposalId> = None;
        let mut cancel_edit = false;
        let mut edit_rejected: Option<String> = None;
        let mut continue_task = false;
        let mut new_task = false;
        let mut clear_context = false;

        egui::Window::new("AI Agent")
            .open(&mut open)
            .default_width(560.0)
            .default_height(520.0)
            .resizable(true)
            .show(ctx, |ui| {
                let Some(session) = self.session.as_ref() else {
                    ui.label("No active agent session.");
                    return;
                };

                ui.horizontal(|ui| {
                    if self.provider_label.is_empty() {
                        ui.label(egui::RichText::new("AI is not configured").weak());
                    } else {
                        ui.label(egui::RichText::new(&self.provider_label).weak());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "turns {}/{}",
                                session.turns_used(),
                                session.max_turns()
                            ))
                            .weak(),
                        );
                    });
                });
                ui.label(
                    egui::RichText::new(
                        "Every command needs your approval before it is typed into the \
                         bound terminal session.",
                    )
                    .weak()
                    .small(),
                );
                ui.separator();

                let row_count = session.transcript().len();
                egui::ScrollArea::vertical()
                    .max_height(ui.available_height() - 96.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for index in 0..row_count {
                            let turn = &session.transcript()[index];
                            match turn {
                                Turn::User(text) => {
                                    ui.label(egui::RichText::new(format!("You: {text}")).strong());
                                }
                                Turn::AssistantThought(text) => {
                                    ui.label(
                                        egui::RichText::new(format!("thought: {text}"))
                                            .weak()
                                            .italics(),
                                    );
                                }
                                Turn::AssistantSay(text) => {
                                    ui.label(format!("Agent: {text}"));
                                }
                                Turn::ProtocolError(text) => {
                                    ui.colored_label(
                                        ui.visuals().warn_fg_color,
                                        format!("protocol: {text}"),
                                    );
                                }
                                Turn::Observation {
                                    exit_code,
                                    output_sample,
                                    ..
                                } => {
                                    let header = format!(
                                        "Output (exit {exit_code}, {} bytes)",
                                        output_sample.len()
                                    );
                                    egui::CollapsingHeader::new(header)
                                        .id_salt(("agent-observation", index))
                                        .show(ui, |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(output_sample.as_str())
                                                        .monospace(),
                                                )
                                                .wrap(),
                                            );
                                        });
                                }
                                Turn::AssistantProposed {
                                    id,
                                    command,
                                    status,
                                } => {
                                    let danger = is_dangerous(command);
                                    let is_current = matches!(
                                        session.state(),
                                        AgentState::AwaitingApproval { proposal_id }
                                            if proposal_id == *id
                                    );
                                    egui::Frame::group(ui.style()).show(ui, |ui| {
                                        if let Some(reason) = danger {
                                            ui.colored_label(
                                                ui.visuals().error_fg_color,
                                                format!("⚠ destructive: {reason}"),
                                            );
                                        }
                                        if let Some((edit_id, buffer)) = self.edit.as_mut() {
                                            if edit_id == id {
                                                if !buffer.is_empty() {
                                                    if let Err(error) =
                                                        crate::review_text::validate_single_line(
                                                            buffer,
                                                            crate::review_text::MAX_AGENT_COMMAND_BYTES,
                                                        )
                                                    {
                                                        buffer.clear();
                                                        edit_rejected = Some(format!(
                                                            "Agent edit cleared before display: {error}"
                                                        ));
                                                    }
                                                }
                                                let response = ui.add(
                                                    egui::TextEdit::singleline(buffer)
                                                        .font(egui::TextStyle::Monospace)
                                                        .desired_width(f32::INFINITY),
                                                );
                                                if response.changed() && !buffer.is_empty() {
                                                    if let Err(error) =
                                                        crate::review_text::validate_single_line(
                                                            buffer,
                                                            crate::review_text::MAX_AGENT_COMMAND_BYTES,
                                                        )
                                                    {
                                                        buffer.clear();
                                                        edit_rejected = Some(format!(
                                                            "Agent edit cleared before display: {error}"
                                                        ));
                                                    }
                                                }
                                                ui.horizontal(|ui| {
                                                    if ui.button("Approve edited").clicked() {
                                                        approve = Some((*id, Some(buffer.clone())));
                                                    }
                                                    if ui.button("Cancel edit").clicked() {
                                                        cancel_edit = true;
                                                    }
                                                });
                                                return;
                                            }
                                        }
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(
                                                    crate::review_text::visible_bounded(
                                                        command,
                                                        crate::review_text::MAX_AGENT_COMMAND_BYTES,
                                                    ),
                                                )
                                                .monospace(),
                                            )
                                            .wrap(),
                                        );
                                        match status {
                                            ProposalStatus::Pending if is_current => {
                                                ui.horizontal(|ui| {
                                                    let label = if danger.is_some() {
                                                        "Approve & Run (destructive)"
                                                    } else {
                                                        "Approve & Run"
                                                    };
                                                    let button = if danger.is_some() {
                                                        egui::Button::new(
                                                            egui::RichText::new(label)
                                                                .color(ui.visuals().error_fg_color),
                                                        )
                                                    } else {
                                                        egui::Button::new(label)
                                                    };
                                                    if ui.add(button).clicked() {
                                                        approve = Some((*id, None));
                                                    }
                                                    if ui.button("Edit").clicked() {
                                                        self.edit = Some((*id, command.clone()));
                                                    }
                                                    if ui.button("Reject").clicked() {
                                                        reject = Some(*id);
                                                    }
                                                });
                                            }
                                            ProposalStatus::Pending => {
                                                ui.label(egui::RichText::new("pending").weak());
                                            }
                                            ProposalStatus::Approved => {
                                                ui.label(egui::RichText::new("✓ ran").weak());
                                            }
                                            ProposalStatus::Rejected => {
                                                ui.label(egui::RichText::new("✗ rejected").weak());
                                            }
                                            ProposalStatus::ManualReview => {
                                                ui.label(
                                                    egui::RichText::new("moved to manual review")
                                                        .weak(),
                                                );
                                            }
                                        }
                                    });
                                }
                            }
                        }
                        if self.loading {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(egui::RichText::new("waiting for the model…").weak());
                            });
                        }
                    });

                ui.separator();
                let state_line = match session.state() {
                    AgentState::Ready => "ready",
                    AgentState::AwaitingModel => "waiting for the model",
                    AgentState::AwaitingApproval { .. } => "a command is waiting for approval",
                    AgentState::AwaitingObservation { .. } => {
                        "waiting for the approved command to finish"
                    }
                    AgentState::Completed => "task completed",
                    AgentState::Cancelled => "session cancelled",
                    AgentState::TurnLimitReached => "turn limit reached",
                };
                if self.status.is_empty() {
                    ui.label(egui::RichText::new(state_line).weak().small());
                } else {
                    ui.colored_label(ui.visuals().warn_fg_color, self.status.as_str());
                }

                // A finished task can be followed up (same transcript, budget
                // permitting) or replaced by a fresh one in the same binding.
                let can_continue = session.can_continue_after_completion();
                let can_restart = matches!(
                    session.state(),
                    AgentState::Completed | AgentState::TurnLimitReached
                );
                if can_continue || can_restart {
                    ui.horizontal(|ui| {
                        if can_continue && ui.button("Continue task").clicked() {
                            continue_task = true;
                        }
                        if can_restart && ui.button("New task").clicked() {
                            new_task = true;
                        }
                    });
                }

                if let Some(context) = self.last_manual_completed.as_ref() {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "attached context: `{}` (exit {})",
                                crate::review_text::visible_bounded(
                                    &context.cmd,
                                    crate::review_text::MAX_HISTORY_COMMAND_BYTES,
                                ),
                                context.exit_code
                            ))
                            .weak()
                            .small(),
                        );
                        if ui
                            .small_button("✕")
                            .on_hover_text("Detach this command from future requests")
                            .clicked()
                        {
                            clear_context = true;
                        }
                    });
                }

                let can_submit = session.state() == AgentState::Ready
                    && session.turns_used() < session.max_turns();
                ui.horizontal(|ui| {
                    let response = ui.add_enabled(
                        can_submit,
                        egui::TextEdit::singleline(&mut self.input)
                            .hint_text("What do you want to do? (Enter to send)")
                            .desired_width(ui.available_width() - 72.0),
                    );
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                    if ui
                        .add_enabled(can_submit, egui::Button::new("Send"))
                        .clicked()
                    {
                        submit = true;
                    }
                });
            });

        if submit {
            self.submit_input();
        }
        if let Some(error) = edit_rejected {
            self.status = error;
        }
        if continue_task || new_task {
            self.edit = None;
            self.awaiting = None;
            if let Some(session) = self.session.as_mut() {
                let result = if continue_task {
                    session.continue_after_completion()
                } else {
                    session.start_new_task()
                };
                match result {
                    Ok(()) => self.status.clear(),
                    Err(error) => self.status = error.to_string(),
                }
            }
        }
        if cancel_edit {
            self.edit = None;
        }
        if clear_context {
            self.last_manual_completed = None;
        }
        if let Some((id, edited)) = approve {
            self.edit = None;
            if let Some(effect) = self.approve(id, edited) {
                effects.push(effect);
            }
        }
        if let Some(id) = reject {
            self.edit = None;
            if let Some(session) = self.session.as_mut() {
                if let Err(error) = session.reject(id) {
                    self.status = error.to_string();
                }
            }
        }
        if !open {
            self.close();
        }
        effects
    }

    fn submit_input(&mut self) {
        let message = self.input.trim().to_string();
        if message.is_empty() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.submit_user(message) {
            Ok(()) => {
                self.input.clear();
                self.status.clear();
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    fn approve(&mut self, id: ProposalId, edited: Option<String>) -> Option<AgentEffect> {
        let session_id = self.bound_session_id.clone()?;
        let session = self.session.as_mut()?;
        let candidate = edited.as_deref().or_else(|| proposal_command(session, id));
        let Some(candidate) = candidate else {
            self.status = "proposal command is unavailable".to_string();
            return None;
        };
        if let Err(error) = crate::review_text::validate_single_line(
            candidate,
            crate::review_text::MAX_AGENT_COMMAND_BYTES,
        ) {
            self.status = format!("Agent command rejected: {error}");
            return None;
        }
        let approved = match edited {
            Some(command) => session.edit_and_approve(id, command),
            None => session.approve(id),
        };
        match approved {
            Ok(approved) => {
                if let Err(error) = crate::review_text::validate_single_line(
                    &approved.command,
                    crate::review_text::MAX_AGENT_COMMAND_BYTES,
                ) {
                    session.cancel();
                    self.status = format!("Agent command rejected after approval: {error}");
                    return None;
                }
                // Checked, never wrapped: a reused generation would let a late
                // completion from an earlier execution attach its output to
                // this approval. Exhaustion needs 2^64 approvals in one
                // session, so sealing is the honest response.
                let Some(generation) = self.execution_generation.checked_add(1) else {
                    session.cancel();
                    self.awaiting = None;
                    self.status = "Agent execution identities are exhausted".to_string();
                    return None;
                };
                self.execution_generation = generation;
                self.awaiting = Some(PendingAgentExecution {
                    proposal_id: approved.proposal_id,
                    command: approved.command.clone(),
                    generation,
                });
                self.status.clear();
                Some(AgentEffect::RunCommand {
                    session_id,
                    command: approved.command,
                    generation,
                })
            }
            Err(error) => {
                self.status = error.to_string();
                None
            }
        }
    }

    pub fn execution_start_failed(&mut self, generation: u64, message: impl Into<String>) {
        if self
            .awaiting
            .as_ref()
            .is_some_and(|pending| pending.generation == generation)
        {
            self.awaiting = None;
            if let Some(session) = self.session.as_mut() {
                session.cancel();
            }
            self.status = message.into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_private(path: &std::path::Path, contents: impl AsRef<[u8]>) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn completed(command: &str, exit: i32, output: &str) -> CompletedCommandOutput {
        CompletedCommandOutput {
            id: "test".into(),
            command: Some(command.to_string()),
            cwd: None,
            exit_code: Some(exit),
            duration_ms: Some(5),
            output: output.to_string(),
            output_available: true,
            truncated: false,
            total_bytes: output.len(),
            agent_generation: None,
        }
    }

    fn ai_config() -> Config {
        Config {
            ai_enabled: true,
            ai_provider: "ollama".into(),
            ai_base_url: "http://localhost:11434".into(),
            ai_model: "codellama:7b".into(),
            ..Config::default()
        }
    }

    fn snapshot_fixture() -> AgentSessionSnapshot {
        let mut session = AgentSession::new(4);
        session.submit_user("persist this session").unwrap();
        session
            .snapshot()
            .expect("non-empty session has a snapshot")
    }

    fn private_test_dir(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("ember-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        root
    }

    #[test]
    fn claiming_a_snapshot_has_exactly_one_winner() {
        let root = private_test_dir("agent-claim");
        let path = root.join("agent_session.json");
        write_snapshot_file(&path, &snapshot_fixture()).unwrap();

        let session = claim_snapshot_session(&path).expect("the first opener restores");
        assert!(!session.transcript().is_empty());
        // Consumed: a second opener finds nothing, and no leftover file in the
        // directory can be restored later.
        assert!(!path.exists());
        assert!(claim_snapshot_session(&path).is_none());
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_unusable_claim_is_quarantined_rather_than_deleted() {
        let root = private_test_dir("agent-quarantine");
        let path = root.join("agent_session.json");

        for evidence in ["not json", r#"{"version":99}"#] {
            std::fs::write(&path, evidence).unwrap();
            assert!(claim_snapshot_session(&path).is_none());
            assert!(!path.exists(), "the original name is claimed");
            let preserved: Vec<_> = std::fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect();
            assert_eq!(preserved.len(), 1, "invalid evidence is kept");
            assert_eq!(std::fs::read_to_string(&preserved[0]).unwrap(), evidence);
            // A quarantined file is never restored by a later opener.
            assert!(claim_snapshot_session(&path).is_none());
            std::fs::remove_file(&preserved[0]).unwrap();
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn local_snapshot_io_round_trips_and_enforces_the_exact_budget() {
        let root =
            std::env::temp_dir().join(format!("ember-agent-snapshot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = root.join("agent_session.json");
        let snapshot = snapshot_fixture();

        write_snapshot_file(&path, &snapshot).unwrap();
        let restored = read_snapshot_file(&path).expect("snapshot should round trip");
        assert!(AgentSession::restore(restored).is_ok());

        let oversized = root.join("oversized.json");
        write_private(&oversized, vec![b'x'; MAX_AGENT_SNAPSHOT_JSON_BYTES + 1]);
        assert!(read_snapshot_file(&oversized).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restored_snapshot_rejects_duplicate_proposal_id_confusion() {
        let mut session = AgentSession::new(4);
        session.submit_user("list files").unwrap();
        session
            .accept_model_reply(r#"{"action":"run","command":"ls"}"#)
            .unwrap();
        let snapshot = session.snapshot().unwrap();
        let mut encoded: serde_json::Value =
            serde_json::from_str(&snapshot.to_json().unwrap()).unwrap();
        let transcript = encoded["transcript"].as_array_mut().unwrap();
        let duplicate = transcript
            .iter()
            .find(|turn| turn.get("AssistantProposed").is_some())
            .unwrap()
            .clone();
        transcript.insert(1, duplicate);
        let encoded = serde_json::to_string(&encoded).unwrap();
        let snapshot = AgentSessionSnapshot::from_json(&encoded).unwrap();

        assert!(matches!(
            restore_snapshot_session(snapshot),
            Err(AgentSnapshotError::Invalid(reason)) if reason.contains("proposal id")
        ));
    }

    #[test]
    fn restored_snapshot_rejects_visually_spoofed_proposals() {
        let mut session = AgentSession::new(4);
        session.submit_user("list files").unwrap();
        session
            .accept_model_reply(r#"{"action":"run","command":"ls"}"#)
            .unwrap();
        let snapshot = session.snapshot().unwrap();
        let mut encoded: serde_json::Value =
            serde_json::from_str(&snapshot.to_json().unwrap()).unwrap();
        let transcript = encoded["transcript"].as_array_mut().unwrap();
        let proposed = transcript
            .iter_mut()
            .find(|turn| turn.get("AssistantProposed").is_some())
            .unwrap();
        proposed["AssistantProposed"]["command"] =
            serde_json::Value::String("printf safe\u{202e}; rm -rf important".into());
        let snapshot = AgentSessionSnapshot::from_json(&encoded.to_string()).unwrap();

        assert!(matches!(
            restore_snapshot_session(snapshot),
            Err(AgentSnapshotError::Invalid(reason)) if reason.contains("proposal command")
        ));
    }

    #[test]
    fn model_and_edit_proposals_fail_closed_on_visual_spoofing() {
        let mut session = AgentSession::new(4);
        session.submit_user("run safely").unwrap();
        let error = accept_model_reply_compat(
            &mut session,
            &serde_json::json!({
                "action": "run",
                "command": "printf safe\u{202e}; rm -rf important",
            })
            .to_string(),
        )
        .unwrap_err();
        assert!(error.contains("command"));
        assert!(session.transcript().iter().all(|turn| !matches!(
            turn,
            Turn::AssistantProposed { command, .. }
                if crate::review_text::contains_visual_spoofing(command)
        )));

        session.retry_model().unwrap();
        let ModelOutcome::Proposal { id, .. } = session
            .accept_model_reply(r#"{"action":"run","command":"printf safe"}"#)
            .unwrap()
        else {
            panic!("expected proposal");
        };
        let mut panel = AgentPanel::new();
        panel.is_open = true;
        panel.bound_session_id = Some("session".into());
        panel.session = Some(session);
        assert!(panel
            .approve(id, Some("printf safe\u{2066}hidden".into()))
            .is_none());
        assert!(panel.status.contains("rejected"));
        assert!(matches!(
            panel.session.as_ref().unwrap().state(),
            AgentState::AwaitingApproval { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn local_snapshot_io_rejects_unsafe_entries_and_never_uses_the_legacy_stage() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;
        use std::time::{Duration, Instant};

        let root = std::env::temp_dir().join(format!(
            "ember-agent-snapshot-unsafe-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("agent_session.json");
        let victim = root.join("victim.json");
        let legacy_stage = root.join(format!(".agent_session.json.next.{}", std::process::id()));
        write_private(&victim, b"sentinel");
        symlink(&victim, &legacy_stage).unwrap();

        write_snapshot_file(&path, &snapshot_fixture()).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"sentinel");
        assert!(std::fs::symlink_metadata(&legacy_stage)
            .unwrap()
            .file_type()
            .is_symlink());

        let linked = root.join("linked.json");
        symlink(&path, &linked).unwrap();
        assert!(read_snapshot_file(&linked).is_none());

        let hard_linked = root.join("hard-linked.json");
        std::fs::hard_link(&path, &hard_linked).unwrap();
        assert!(read_snapshot_file(&hard_linked).is_none());

        let fifo = root.join("fifo.json");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_name is NUL-terminated and remains live for this call.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        assert!(read_snapshot_file(&fifo).is_none());
        assert!(started.elapsed() < Duration::from_secs(1));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn approval_yields_a_run_effect_for_the_bound_session_only() {
        let mut panel = AgentPanel::new();
        panel.open(&ai_config(), "session-three".into());
        let session = panel.session.as_mut().unwrap();
        session.submit_user("list files").unwrap();
        let outcome = session
            .accept_model_reply(r#"{"action":"run","command":"ls -la"}"#)
            .unwrap();
        let jterm_core::agent::ModelOutcome::Proposal { id, .. } = outcome else {
            panic!("expected proposal");
        };
        let effect = panel.approve(id, None).expect("approval must yield effect");
        let AgentEffect::RunCommand {
            session_id,
            command,
            generation,
        } = effect;
        assert_eq!(session_id, "session-three");
        assert_eq!(command, "ls -la");
        assert_ne!(generation, 0);
        assert!(panel.awaiting.is_some());

        // Completion from another session is ignored entirely.
        panel.handle_completed("session-one", &completed("ls -la", 0, "total 0"));
        assert!(panel.awaiting.is_some());
        assert!(panel.last_manual_completed.is_none());
        // A different command in the bound session becomes manual context,
        // not an observation.
        panel.handle_completed("session-three", &completed("pwd", 0, "/tmp"));
        assert!(panel.awaiting.is_some());
        assert_eq!(
            panel.last_manual_completed.as_ref().map(|c| c.cmd.as_str()),
            Some("pwd")
        );
        // Identical text alone is not authorization.
        panel.handle_completed(
            "session-three",
            &completed("ls -la", 0, "unrelated same command"),
        );
        assert!(panel.awaiting.is_some());

        // Only the locally armed generation becomes the observation.
        let mut approved = completed("ls -la", 0, "total 0");
        approved.agent_generation = Some(generation);
        panel.handle_completed("session-three", &approved);
        assert!(panel.awaiting.is_none());
        assert_eq!(
            panel.session.as_ref().unwrap().state(),
            AgentState::AwaitingModel
        );
    }

    #[test]
    fn approved_completion_without_exit_status_fails_closed() {
        let mut panel = AgentPanel::new();
        panel.open(&ai_config(), "session-three".into());
        let session = panel.session.as_mut().unwrap();
        session.submit_user("list files").unwrap();
        let outcome = session
            .accept_model_reply(r#"{"action":"run","command":"ls -la"}"#)
            .unwrap();
        let jterm_core::agent::ModelOutcome::Proposal { id, .. } = outcome else {
            panic!("expected proposal");
        };
        let AgentEffect::RunCommand { generation, .. } =
            panel.approve(id, None).expect("approval must yield effect");
        let mut completion = completed("ls -la", 0, "total 0");
        completion.exit_code = None;
        completion.agent_generation = Some(generation);

        panel.handle_completed("session-three", &completion);

        assert!(panel.awaiting.is_none());
        assert_eq!(
            panel.session.as_ref().unwrap().state(),
            AgentState::Cancelled
        );
        assert!(panel.status.contains("no exit status"));
    }

    #[test]
    fn closing_the_panel_seals_the_session() {
        let mut panel = AgentPanel::new();
        panel.open(&ai_config(), "session-zero".into());
        panel.session.as_mut().unwrap().submit_user("hi").unwrap();
        panel.close();
        assert!(!panel.is_open);
        assert!(panel.session.is_none());
        assert!(panel.result_rx.is_none());
    }

    #[test]
    fn disabled_ai_reports_a_configuration_status() {
        let mut panel = AgentPanel::new();
        panel.open(&Config::default(), "session-zero".into());
        assert!(panel.status.contains("disabled"));
    }
}
