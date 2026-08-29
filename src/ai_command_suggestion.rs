//! Natural-language → shell command drafting, review-first. Port of anvil's
//! `ai_palette_ops.rs` inline suggestion flow (the palette `?` request) to
//! ember's egui surface, reusing the command-correction card idiom.
//!
//! The invariant from the source (`ai_palette_ops.rs`): **generated commands
//! never run automatically**. The reply renders as an editable review card;
//! the only primary action inserts the text at the shell prompt, where the
//! user reviews it and presses Enter themselves. Stop, Regenerate, and
//! Dismiss never touch the PTY either.
//!
//! ember deviations, all fail-closed:
//!
//! - The card is a floating egui window bound to the terminal session that
//!   was active when the request started (ember has no inline-notice surface
//!   inside the terminal canvas), rendered only while that session is active.
//! - anvil attaches the selected Block's command/output as untrusted context;
//!   ember routes block context through the Agent panel instead, so this flow
//!   sends no block context. The payload still carries the pane cwd, which is
//!   command context, so drafting requires
//!   [`crate::agent_panel::ensure_semantic_context_sharing_allowed`].
//! - Replies are harvested through a bounded channel with the correction
//!   monitor's 30 s deadline instead of anvil's glib timeout poll.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use jterm_core::ai::AiCancellationToken;

use crate::config::Config;
use crate::theme::ThemeExt as _;

/// The sources' generated-command budget (`MAX_GENERATED_COMMAND_BYTES` in
/// `jterm_core::ai`, `MAX_AGENT_COMMAND_BYTES` in ember's review_text).
const MAX_SUGGESTION_COMMAND_BYTES: usize = 16 * 1024;
/// Ask-AI request budget. Shared with the palette entry that produces these
/// requests so the two gates cannot drift apart.
const MAX_SUGGESTION_REQUEST_BYTES: usize = crate::command_palette::MAX_AI_QUERY_BYTES;
const SUGGESTION_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// One worker reply: the issuing `(generation, request_id)` and the drafted
/// command (or the provider's error).
type SuggestionReply = (u64, u64, Result<String, String>);

/// Process-wide suggestion-session counter.
///
/// `generation` is the session identity that `suggestion_reply_is_current` and
/// [`AiCommandSuggestion::complete_accept`] compare against. It used to be the
/// constant `1` on every session, which made both comparisons `x == x` — the
/// staleness defence was inert, and every card in every pane also shared one
/// egui window identity. A monotonic counter gives each session its own value,
/// so a reply or an accept-effect that outlived its session is provably stale.
static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_generation() -> u64 {
    NEXT_GENERATION
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .max(1)
}

/// anvil's `suggestion_reply_is_current`: a reply may publish only while the
/// session that issued it is still waiting on exactly that request.
pub(crate) fn suggestion_reply_is_current(
    session_generation: u64,
    session_request_id: u64,
    busy: bool,
    generation: u64,
    request_id: u64,
) -> bool {
    busy && session_generation == generation && session_request_id == request_id
}

struct SuggestionCard {
    request_id: u64,
    /// Editable review buffer; initialized from the validated model reply.
    command: String,
    feedback: Option<String>,
    /// The first frame only presents; Enter/Escape arm from the second, so a
    /// key held from the palette Enter cannot accept its own card.
    armed: bool,
}

/// An accepted, re-validated command for the app to insert at the bound
/// session's prompt through the guarded prompt-write path. Never auto-run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SuggestionEffect {
    pub(crate) session_id: String,
    pub(crate) generation: u64,
    pub(crate) command: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SuggestionUiOutcome {
    None,
    /// The user closed the card (Dismiss, Escape, or the window's ✕). The app
    /// must drop the session: nothing else ever clears it, and the card is
    /// re-shown on the very next frame while it lives. anvil routes the same
    /// three gestures to `close_command_suggestion`.
    Dismissed,
    Accepted(SuggestionEffect),
}

/// One pane-bound suggestion session (anvil's `CommandSuggestionSession`).
/// Owned by the app as an `Option`; a new `?` request replaces any open one.
pub(crate) struct AiCommandSuggestion {
    session_id: String,
    request: String,
    cwd: String,
    shell: String,
    provider: String,
    generation: u64,
    request_id: u64,
    busy: bool,
    started: Option<Instant>,
    cancel: Option<AiCancellationToken>,
    /// Carries the issuing `(generation, request_id)` so a reply arriving after
    /// Regenerate (which replaces this receiver) is still provably stale if it
    /// ever reached the current one.
    reply_rx: Option<mpsc::Receiver<SuggestionReply>>,
    card: Option<SuggestionCard>,
    status: String,
    error: bool,
}

impl AiCommandSuggestion {
    /// Preflight, fail-closed: AI disabled or unconfigured, or cloud context
    /// sharing not consented, all produce the returned message for a toast
    /// and no session is created (anvil: toasts before opening the card).
    pub(crate) fn start(
        config: &Config,
        session_id: String,
        request: String,
        cwd: String,
        shell: String,
    ) -> Result<Self, String> {
        // Preflight order matters: a disabled or unconfigured provider reports
        // its own graceful reason before the consent gate speaks about a
        // request that was never going to be drafted.
        let client = crate::agent_panel::client_from_config(config)?;
        crate::agent_panel::ensure_semantic_context_sharing_allowed(config)?;
        // Fail closed on an oversized request instead of letting core elide
        // its middle: a drafted command must answer the whole instruction the
        // user gave, or none of it.
        if request.len() > MAX_SUGGESTION_REQUEST_BYTES {
            return Err(format!(
                "AI request is too large ({} KiB limit)",
                MAX_SUGGESTION_REQUEST_BYTES / 1024
            ));
        }
        let provider = client.display_name();
        let mut session = Self {
            session_id,
            request,
            cwd,
            shell,
            provider,
            generation: next_generation(),
            request_id: 0,
            busy: false,
            started: None,
            cancel: None,
            reply_rx: None,
            card: None,
            status: String::new(),
            error: false,
        };
        session.begin_request(config);
        Ok(session)
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// (Re)issue the provider request. Config is re-read every time so a
    /// hot-reload (disabled AI, rotated key) applies to Regenerate too.
    fn begin_request(&mut self, config: &Config) {
        let client = match crate::agent_panel::client_from_config(config) {
            Ok(client) => client,
            Err(error) => {
                self.busy = false;
                self.error = true;
                self.status = error;
                return;
            }
        };
        self.card = None;
        self.busy = true;
        self.error = false;
        self.request_id = self.request_id.wrapping_add(1).max(1);
        self.status = format!("Drafting with {}…", self.provider);
        let request_id = self.request_id;
        let generation = self.generation;
        let cancellation = AiCancellationToken::new();
        let worker_token = cancellation.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let (request, cwd, shell) = (self.request.clone(), self.cwd.clone(), self.shell.clone());
        let spawn = std::thread::Builder::new()
            .name("ember-ai-command-suggestion".to_string())
            .spawn(move || {
                let result = jterm_core::ai::nl_to_command_with_context_blocking_cancellable(
                    &client,
                    &request,
                    &cwd,
                    &shell,
                    std::env::consts::OS,
                    None,
                    &worker_token,
                )
                .map_err(|error| error.to_string());
                if worker_token.is_cancelled() {
                    return;
                }
                let _ = tx.send((generation, request_id, result));
            });
        match spawn {
            Ok(_) => {
                self.cancel = Some(cancellation);
                self.reply_rx = Some(rx);
                self.started = Some(Instant::now());
            }
            Err(error) => {
                self.busy = false;
                self.error = true;
                self.status = format!("could not start AI command worker: {error}");
            }
        }
    }

    /// Per-frame driver: harvest the worker reply and enforce the shared
    /// deadline. A hot-reload that turns AI off cancels the in-flight request
    /// (the correction monitor's rule). Cheap while idle.
    pub(crate) fn drive(&mut self, config: &Config, ctx: &egui::Context) {
        if !self.busy {
            return;
        }
        if !config.ai_enabled {
            self.stop_in_flight();
            self.error = true;
            self.status = "AI features are disabled by configuration".to_string();
            return;
        }
        // The worker finishes without producing an egui event; keep ticking
        // so the card appears promptly (correction monitor's pattern).
        ctx.request_repaint_after(Duration::from_millis(50));
        if self
            .started
            .is_some_and(|started| started.elapsed() >= SUGGESTION_REQUEST_TIMEOUT)
        {
            self.stop_in_flight();
            self.error = true;
            self.status = format!(
                "Command suggestion timed out after {} seconds. Retry when ready.",
                SUGGESTION_REQUEST_TIMEOUT.as_secs()
            );
            return;
        }
        let reply = self.reply_rx.as_ref().map(|rx| rx.try_recv());
        match reply {
            Some(Ok((reply_generation, reply_request_id, result))) => {
                // A Stop/Regenerate has already replaced this receiver, so a
                // reply that still lands here must name the live request *and*
                // the live session. The reply carries its issuing generation,
                // so this is a real comparison rather than `x == x`.
                if !suggestion_reply_is_current(
                    self.generation,
                    self.request_id,
                    self.busy,
                    reply_generation,
                    reply_request_id,
                ) {
                    return;
                }
                self.reply_rx = None;
                self.started = None;
                self.cancel = None;
                self.busy = false;
                match result {
                    Ok(command) => {
                        self.error = false;
                        self.status =
                            "Review the proposal below. Nothing has been inserted or run."
                                .to_string();
                        self.card = Some(SuggestionCard {
                            request_id: reply_request_id,
                            command,
                            feedback: None,
                            armed: false,
                        });
                    }
                    Err(error) => {
                        self.error = true;
                        self.status = format!("Command suggestion failed: {error}");
                    }
                }
            }
            Some(Err(mpsc::TryRecvError::Empty)) => {}
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.reply_rx = None;
                self.started = None;
                self.cancel = None;
                self.busy = false;
                self.error = true;
                self.status = "AI command worker exited unexpectedly".to_string();
            }
            None => {
                self.reply_rx = None;
                self.started = None;
                self.busy = false;
            }
        }
    }

    fn stop_in_flight(&mut self) {
        if let Some(cancellation) = self.cancel.take() {
            cancellation.cancel();
        }
        self.reply_rx = None;
        self.started = None;
        self.busy = false;
    }

    /// anvil's Stop button: cancel the in-flight request, keep the session.
    fn stop(&mut self) {
        if !self.busy {
            return;
        }
        self.stop_in_flight();
        self.error = true;
        self.status = "Suggestion request stopped. Retry when ready.".to_string();
    }

    /// Render the session card while its bound session is active.
    /// `prompt_clean_idle` gates the initial focus grab, exactly like the
    /// correction card: a prompt the user is typing into keeps its keys.
    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        config: &Config,
        theme: &crate::theme::Theme,
        active_session_id: Option<&str>,
        prompt_clean_idle: bool,
    ) -> SuggestionUiOutcome {
        if active_session_id != Some(self.session_id.as_str()) {
            return SuggestionUiOutcome::None;
        }

        let mut open = true;
        let mut accept = false;
        let mut dismiss = false;
        let mut regenerate = false;
        let mut stop = false;
        let provider = self.provider.clone();
        let cwd = jterm_core::review_input::safe_inline_display(&self.cwd, 4 * 1024);
        let request_line = jterm_core::review_input::safe_inline_display(&self.request, 16 * 1024);

        egui::Window::new("AI command suggestion")
            .id(egui::Id::new(("ai-command-suggestion", self.generation)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            // Above the correction card's anchor: both surfaces can be up at
            // once (a failure card plus a fresh `?` request).
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -200.0))
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
                    egui::RichText::new(format!("{cwd} · {provider} · review only"))
                        .weak()
                        .small(),
                );
                ui.label(format!("Request: {request_line}"));

                if self.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new(self.status.as_str()).weak());
                    });
                } else if self.card.is_none() {
                    let text = egui::RichText::new(self.status.as_str()).small();
                    if self.error {
                        ui.colored_label(ui.visuals().error_fg_color, text);
                    } else {
                        ui.label(text.weak());
                    }
                }

                let mut edit_response = None;
                if let Some(card) = self.card.as_mut() {
                    ui.label(egui::RichText::new(self.status.as_str()).weak().small());
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut card.command)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                    if prompt_clean_idle && !card.armed {
                        response.request_focus();
                    }
                    if let Some(feedback) = card.feedback.as_deref() {
                        ui.colored_label(ui.visuals().error_fg_color, feedback);
                    }
                    edit_response = Some(response);
                }

                ui.horizontal(|ui| {
                    if self.busy {
                        if ui.button("Stop").clicked() {
                            stop = true;
                        }
                    } else {
                        if self.card.is_some()
                            && ui.button("Insert for review").clicked()
                        {
                            accept = true;
                        }
                        if ui.button("Regenerate").clicked() {
                            regenerate = true;
                        }
                        if ui.button("Dismiss").clicked() {
                            dismiss = true;
                        }
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "Enter uses the labelled action · generated commands never run automatically",
                    )
                    .weak()
                    .small(),
                );

                // The edit field owns Enter/Escape while focused, and a
                // focused text edit already blocks terminal input routing, so
                // neither key can leak into the PTY underneath (correction
                // card's arming pattern).
                if let Some(card) = self.card.as_mut() {
                    if card.armed {
                        let enter_pressed =
                            ui.input(|input| input.key_pressed(egui::Key::Enter));
                        let escape_pressed =
                            ui.input(|input| input.key_pressed(egui::Key::Escape));
                        let edit_owned_key = edit_response
                            .as_ref()
                            .is_some_and(|response| response.has_focus() || response.lost_focus());
                        if enter_pressed && edit_owned_key {
                            accept = true;
                        }
                        if escape_pressed && edit_owned_key {
                            dismiss = true;
                        }
                    }
                    card.armed = true;
                }
            });

        if !open {
            dismiss = true;
        }
        if stop {
            self.stop();
        }
        if regenerate && !self.busy {
            self.begin_request(config);
        }
        if dismiss {
            // Cancel any request still running behind the card: the user asked
            // for this surface to go away, and the app drops the session on
            // `Dismissed`, so nothing would ever harvest the reply.
            self.stop_in_flight();
            return SuggestionUiOutcome::Dismissed;
        }
        if accept {
            let Some(card) = self.card.as_mut() else {
                return SuggestionUiOutcome::None;
            };
            // The user may have edited the proposal: the accepted text passes
            // the shared single-line gate at the sources' 16 KiB review
            // budget before the app writes it anywhere.
            let command = match crate::review_text::validate_single_line(
                card.command.as_str(),
                MAX_SUGGESTION_COMMAND_BYTES,
            ) {
                Ok(command) => command.to_string(),
                Err(error) => {
                    card.feedback = Some(format!("Cannot insert: {error}"));
                    return SuggestionUiOutcome::None;
                }
            };
            if card.request_id != self.request_id {
                return SuggestionUiOutcome::None;
            }
            card.feedback = None;
            return SuggestionUiOutcome::Accepted(SuggestionEffect {
                session_id: self.session_id.clone(),
                generation: self.generation,
                command,
            });
        }
        SuggestionUiOutcome::None
    }

    /// Settle an accepted effect after the app tried to write it to the PTY.
    /// Success consumes the session (the app drops it); failure keeps the
    /// card and shows the reason inline, matching the sources.
    pub(crate) fn complete_accept(&mut self, generation: u64, result: Result<(), String>) {
        if generation != self.generation {
            return;
        }
        if let Err(error) = result {
            if let Some(card) = self.card.as_mut() {
                card.feedback = Some(error);
            }
        }
    }
}

impl Drop for AiCommandSuggestion {
    fn drop(&mut self) {
        // Replacing or closing the session cancels its curl transport instead
        // of detaching it (anvil's `close_command_suggestion`).
        if let Some(cancellation) = self.cancel.take() {
            cancellation.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_or_retried_request_cannot_publish_a_stale_reply() {
        // anvil's `suggestion_reply_is_current` truth table, verbatim.
        assert!(suggestion_reply_is_current(4, 2, true, 4, 2));
        assert!(!suggestion_reply_is_current(4, 2, false, 4, 2));
        assert!(!suggestion_reply_is_current(4, 3, true, 4, 2));
        assert!(!suggestion_reply_is_current(5, 2, true, 4, 2));
    }

    #[test]
    fn accept_budget_matches_the_sources_review_limit() {
        const { assert!(MAX_SUGGESTION_COMMAND_BYTES == crate::review_text::MAX_AGENT_COMMAND_BYTES) }
    }

    #[test]
    fn preflight_fails_closed_when_ai_is_disabled() {
        // Config::default has ai_enabled = false; the error must name the
        // cause before any worker or network exists.
        let config = Config::default();
        let Err(error) = AiCommandSuggestion::start(
            &config,
            "session-1".into(),
            "list files".into(),
            ".".into(),
            "sh".into(),
        ) else {
            panic!("AI-disabled preflight must not create a session");
        };
        assert!(error.contains("disabled"), "{error}");
    }

    #[test]
    fn preflight_requires_context_consent_for_cloud_providers() {
        // A private temp key file lets client construction succeed without
        // any environment credential, so the consent gate is the unit under
        // test. Anthropic is never loopback-exempt.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // The key file's parent must be owned by this user, so the file goes
        // in a private directory of our own rather than straight into a
        // world-writable /tmp (which `write_api_key_file` rightly refuses).
        let key_dir = std::env::temp_dir().join(format!(
            "ember-ai-suggestion-test-{}-{unique}",
            std::process::id()
        ));
        crate::persistence_file::ensure_private_directory(&key_dir).unwrap();
        let key_path = key_dir.join("provider.key");
        crate::persistence_file::write_api_key_file(
            key_path.to_str().unwrap(),
            "sk-test-placeholder",
        )
        .unwrap();
        let config = Config {
            ai_enabled: true,
            ai_provider: "anthropic".into(),
            ai_api_key_file: Some(key_path.to_str().unwrap().to_string()),
            ..Config::default()
        };
        let Err(error) = AiCommandSuggestion::start(
            &config,
            "session-1".into(),
            "list files".into(),
            ".".into(),
            "sh".into(),
        ) else {
            panic!("unconsented cloud preflight must not create a session");
        };
        let _ = std::fs::remove_dir_all(&key_dir);
        assert!(error.contains("context sharing"), "{error}");
    }

    /// Drive one headless egui frame. The texture delta must be cleared or
    /// epaint panics when the output is dropped.
    fn run_frame(ctx: &egui::Context, input: egui::RawInput, run: impl FnOnce(&egui::Context)) {
        ctx.begin_pass(input);
        run(ctx);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
    }

    fn key_input(key: egui::Key) -> egui::RawInput {
        egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        }
    }

    /// A session with a ready card, built without a provider: `start` would
    /// need a configured client and a live worker thread.
    fn session_with_card(command: &str) -> AiCommandSuggestion {
        AiCommandSuggestion {
            session_id: "session-1".into(),
            request: "delete every log file".into(),
            cwd: "/tmp".into(),
            shell: "sh".into(),
            provider: "test provider".into(),
            generation: next_generation(),
            request_id: 7,
            busy: false,
            started: None,
            cancel: None,
            reply_rx: None,
            card: Some(SuggestionCard {
                request_id: 7,
                command: command.to_string(),
                feedback: None,
                armed: false,
            }),
            status: "Review the proposal below.".into(),
            error: false,
        }
    }

    #[test]
    fn escape_dismisses_the_card_instead_of_reporting_nothing() {
        // The card is anchored, non-movable and re-shown every frame while the
        // session lives, and only a *successful* insert used to clear it. A
        // dismiss that returns `None` is indistinguishable from "nothing
        // happened", so the review surface stayed on screen forever.
        let ctx = egui::Context::default();
        let config = Config::default();
        let theme = crate::theme::Theme::default();
        let mut session = session_with_card("rm -rf /var/log/*");

        // First frame presents and arms the card (and grabs focus for the
        // editable command), so Escape cannot be the palette's own keypress.
        let mut first = SuggestionUiOutcome::None;
        run_frame(&ctx, egui::RawInput::default(), |ctx| {
            first = session.show(ctx, &config, &theme, Some("session-1"), true);
        });
        assert_eq!(first, SuggestionUiOutcome::None);

        let mut second = SuggestionUiOutcome::None;
        run_frame(&ctx, key_input(egui::Key::Escape), |ctx| {
            second = session.show(ctx, &config, &theme, Some("session-1"), true);
        });
        assert_eq!(
            second,
            SuggestionUiOutcome::Dismissed,
            "Escape must tell the app to drop the session"
        );
    }

    #[test]
    fn a_reply_from_a_replaced_session_cannot_publish_into_the_card() {
        // `generation` used to be the constant 1 on every session, so the
        // comparison in `drive` was `x == x` and this reply would land.
        let ctx = egui::Context::default();
        let config = Config {
            ai_enabled: true,
            ..Config::default()
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let mut session = session_with_card("true");
        session.card = None;
        session.busy = true;
        session.started = Some(Instant::now());
        session.reply_rx = Some(rx);
        let stale_generation = session.generation.wrapping_sub(1);
        tx.send((
            stale_generation,
            session.request_id,
            Ok("rm -rf /".to_string()),
        ))
        .unwrap();

        run_frame(&ctx, egui::RawInput::default(), |ctx| {
            session.drive(&config, ctx);
        });
        assert!(session.card.is_none(), "a stale reply must not publish");
        assert!(session.busy, "the live request is still outstanding");

        // The live session's own reply does publish.
        let (tx, rx) = mpsc::sync_channel(1);
        session.reply_rx = Some(rx);
        tx.send((session.generation, session.request_id, Ok("ls".to_string())))
            .unwrap();
        run_frame(&ctx, egui::RawInput::default(), |ctx| {
            session.drive(&config, ctx);
        });
        assert_eq!(
            session.card.as_ref().map(|card| card.command.as_str()),
            Some("ls")
        );
    }

    #[test]
    fn every_session_gets_its_own_generation() {
        // Distinct generations are what make the reply guard and
        // `complete_accept` real comparisons, and what give each pane's card
        // its own egui window identity.
        let first = next_generation();
        let second = next_generation();
        assert!(second > first);
        assert!(first >= 1 && second >= 1);
    }

    #[test]
    fn an_oversized_request_is_refused_instead_of_silently_truncated() {
        // Past this bound core's `sample_output` elides the middle of the
        // request, and the card would present a command drafted from an
        // instruction with a hole in it as an ordinary suggestion.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let key_dir = std::env::temp_dir().join(format!(
            "ember-ai-suggestion-size-{}-{unique}",
            std::process::id()
        ));
        crate::persistence_file::ensure_private_directory(&key_dir).unwrap();
        let key_path = key_dir.join("provider.key");
        crate::persistence_file::write_api_key_file(
            key_path.to_str().unwrap(),
            "sk-test-placeholder",
        )
        .unwrap();
        let config = Config {
            ai_enabled: true,
            ai_provider: "anthropic".into(),
            ai_api_key_file: Some(key_path.to_str().unwrap().to_string()),
            // Consented, so the size gate is the only thing left to refuse.
            ai_share_command_context: true,
            ..Config::default()
        };
        let Err(error) = AiCommandSuggestion::start(
            &config,
            "session-1".into(),
            "x".repeat(MAX_SUGGESTION_REQUEST_BYTES + 1),
            ".".into(),
            "sh".into(),
        ) else {
            panic!("an oversized request must not create a session");
        };
        let _ = std::fs::remove_dir_all(&key_dir);
        assert!(error.contains("too large"), "{error}");
    }
}
