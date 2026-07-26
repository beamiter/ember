//! Agent mode — a multi-turn LLM that proposes shell commands, watches their
//! output, and iterates. UI is egui; the protocol state machine, provider
//! client, transport, and redaction all live in `jterm_core` (shared with
//! jterm1/jterm4).
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
use jterm_core::agent::{is_dangerous, AgentSession, AgentState, ProposalId, ProposalStatus, Turn};
use jterm_core::ai::{AiCancellationToken, AiClient, AiSettings, BlockContext};
use std::sync::mpsc;

fn snapshot_path() -> Option<std::path::PathBuf> {
    Some(
        dirs::config_dir()?
            .join("jterm2")
            .join("agent_session.json"),
    )
}

/// Side effects the panel asks the app to perform. The panel itself never
/// touches a PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEffect {
    /// Write this protocol-validated single-line command plus a carriage
    /// return to the bound session's input queue.
    RunCommand {
        session_index: usize,
        command: String,
    },
}

fn client_from_config(config: &Config) -> Result<AiClient, String> {
    AiClient::from_settings(&AiSettings {
        enabled: config.ai_enabled,
        provider: config.ai_provider.clone(),
        api_key_file: jterm_core::ai::resolve_api_key_file(config.ai_api_key_file.as_deref()),
        model: config.ai_model.clone(),
        base_url: config.ai_base_url.clone(),
        max_tokens: config.ai_max_tokens,
        temperature: config.ai_temperature,
        redact_secrets: config.ai_redact_secrets,
    })
    .map_err(|error| error.to_string())
}

pub struct AgentPanel {
    pub is_open: bool,
    session: Option<AgentSession>,
    bound_session: Option<usize>,
    /// Approved proposal currently executing in the bound session.
    awaiting: Option<(ProposalId, String)>,
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
    cancel: Option<AiCancellationToken>,
}

impl AgentPanel {
    pub fn new() -> Self {
        Self {
            is_open: false,
            session: None,
            bound_session: None,
            awaiting: None,
            last_manual_completed: None,
            input: String::new(),
            edit: None,
            loading: false,
            status: String::new(),
            provider_label: String::new(),
            result_rx: None,
            cancel: None,
        }
    }

    /// Open the panel bound to `session_index`, replacing any previous
    /// session (whose in-flight request is cancelled first). A snapshot
    /// persisted by the previous run is restored one-shot and rebound to the
    /// current terminal session.
    pub fn open(&mut self, config: &Config, session_index: usize) {
        self.close_session();
        self.is_open = true;
        self.bound_session = Some(session_index);
        self.status.clear();
        let restored = snapshot_path().and_then(|path| {
            let snapshot = jterm_core::agent::read_snapshot_file(&path)?;
            jterm_core::agent::remove_snapshot_file(&path);
            AgentSession::restore(snapshot).ok()
        });
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
        match self.session.as_ref().and_then(|session| session.snapshot()) {
            Some(snapshot) => {
                if let Err(error) = jterm_core::agent::write_snapshot_file(&path, &snapshot) {
                    log::warn!("agent: could not persist session: {error}");
                }
            }
            None => jterm_core::agent::remove_snapshot_file(&path),
        }
    }

    pub fn toggle(&mut self, config: &Config, session_index: usize) {
        if self.is_open {
            self.close();
        } else {
            self.open(config, session_index);
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
        self.bound_session = None;
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
                    if let Some(session) = self.session.as_mut() {
                        let outcome = match result {
                            Ok(raw) => session.accept_model_reply(&raw).map(|_| ()),
                            Err(error) => session.model_failed(error).map(|_| ()),
                        };
                        if let Err(error) = outcome {
                            self.status = error.to_string();
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
        self.loading = true;
        self.status.clear();
    }

    /// Feed one OSC 133 command completion from the bound session. A
    /// completion matching the approved proposal becomes an observation;
    /// anything else is remembered as the user's most recent manual command
    /// and attached to later model requests as untrusted block context.
    pub fn handle_completed(&mut self, session_index: usize, completed: &CompletedCommandOutput) {
        if !self.is_open || self.bound_session != Some(session_index) {
            return;
        }
        let output = if completed.output_available {
            completed.output.as_str()
        } else {
            "(command output was not captured)"
        };
        let reported = completed.command.as_deref().map(str::trim);
        if let Some((proposal_id, approved_command)) = self.awaiting.clone() {
            if reported == Some(approved_command.trim()) {
                if let Some(session) = self.session.as_mut() {
                    match session.observe(proposal_id, completed.exit_code.unwrap_or(0), output) {
                        Ok(()) => self.awaiting = None,
                        Err(error) => self.status = error.to_string(),
                    }
                }
                return;
            }
        }
        let Some(command) = reported.filter(|command| !command.is_empty()) else {
            return;
        };
        self.last_manual_completed = Some(BlockContext {
            cmd: command.to_string(),
            output: output.to_string(),
            cwd: completed.cwd.clone(),
            exit_code: completed.exit_code.unwrap_or(0),
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
                                                ui.add(
                                                    egui::TextEdit::singleline(buffer)
                                                        .font(egui::TextStyle::Monospace)
                                                        .desired_width(f32::INFINITY),
                                                );
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
                                                egui::RichText::new(command.as_str()).monospace(),
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
                                context.cmd, context.exit_code
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
        let session_index = self.bound_session?;
        let session = self.session.as_mut()?;
        let approved = match edited {
            Some(command) => session.edit_and_approve(id, command),
            None => session.approve(id),
        };
        match approved {
            Ok(approved) => {
                self.awaiting = Some((approved.proposal_id, approved.command.clone()));
                self.status.clear();
                Some(AgentEffect::RunCommand {
                    session_index,
                    command: approved.command,
                })
            }
            Err(error) => {
                self.status = error.to_string();
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn approval_yields_a_run_effect_for_the_bound_session_only() {
        let mut panel = AgentPanel::new();
        panel.open(&ai_config(), 3);
        let session = panel.session.as_mut().unwrap();
        session.submit_user("list files").unwrap();
        let outcome = session
            .accept_model_reply(r#"{"action":"run","command":"ls -la"}"#)
            .unwrap();
        let jterm_core::agent::ModelOutcome::Proposal { id, .. } = outcome else {
            panic!("expected proposal");
        };
        let effect = panel.approve(id, None).expect("approval must yield effect");
        assert_eq!(
            effect,
            AgentEffect::RunCommand {
                session_index: 3,
                command: "ls -la".into()
            }
        );
        assert!(panel.awaiting.is_some());

        // Completion from another session is ignored entirely.
        panel.handle_completed(1, &completed("ls -la", 0, "total 0"));
        assert!(panel.awaiting.is_some());
        assert!(panel.last_manual_completed.is_none());
        // A different command in the bound session becomes manual context,
        // not an observation.
        panel.handle_completed(3, &completed("pwd", 0, "/tmp"));
        assert!(panel.awaiting.is_some());
        assert_eq!(
            panel.last_manual_completed.as_ref().map(|c| c.cmd.as_str()),
            Some("pwd")
        );
        // The matching completion becomes the observation.
        panel.handle_completed(3, &completed("ls -la", 0, "total 0"));
        assert!(panel.awaiting.is_none());
        assert_eq!(
            panel.session.as_ref().unwrap().state(),
            AgentState::AwaitingModel
        );
    }

    #[test]
    fn closing_the_panel_seals_the_session() {
        let mut panel = AgentPanel::new();
        panel.open(&ai_config(), 0);
        panel.session.as_mut().unwrap().submit_user("hi").unwrap();
        panel.close();
        assert!(!panel.is_open);
        assert!(panel.session.is_none());
        assert!(panel.result_rx.is_none());
    }

    #[test]
    fn disabled_ai_reports_a_configuration_status() {
        let mut panel = AgentPanel::new();
        panel.open(&Config::default(), 0);
        assert!(panel.status.contains("disabled"));
    }
}
