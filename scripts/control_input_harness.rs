#![allow(dead_code)]
// Production items are inserted by test-control-input.py. Only application/host
// storage and unrelated UI/PTY boundaries are facades. Greeting admission,
// cancellation, cooldown and retreat use the real production reducers/policy.
use jterm_core::organism::{AmbientMind, RenderContext};
use jterm_core::organism_clock::WindowLife;
use jterm_core::organism_daily::PreviewPose;
use jterm_core::organism_live::{
    LiveActivityKnowledge, LiveAttention, LiveAttentionEvent, LiveAttentionInput,
    LiveAttentionOutcome, LiveAttentionRejection, LiveAttentionSnapshot,
};
use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::time::Duration;

mod keybindings {
    use super::*;
    #[derive(Clone, Debug, PartialEq, Eq)]
    // @COMMAND@
    // @PARSE_COMMAND@
    pub struct KeyBindings {
        pub bindings: HashMap<String, String>,
    }
    impl KeyBindings {
        // @KEYS_NEW@
        // @KEYS_DEFAULT@
        // @KEYS_GET@
        // @KEYS_NORMALIZE@
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// @MODE@
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// @MOTION@
struct Config {
    terminal_mode: Option<TerminalMode>,
    block_mode: bool,
    ascii_organism_enabled: bool,
    ascii_organism_motion: Option<OrganismMotion>,
}
impl Config {
    // @EFFECTIVE_MODE@
    // @CARDS_ENABLED@
}
// @RESOLVED_MOTION@
// @INPUT_RETREAT@
// @FULL_FRAME_INTERVAL@
// @CALM_HEARTBEAT_INTERVAL@
#[derive(Clone, Copy)]
// @POLICY@
// @POLICY_IMPL@

mod session {
    use super::*;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    // @PURPOSE@
    // @INPUT_CAP@
    // @APPEND_INPUT@
    // Parser and mutex facade records the actual accepted byte notification.
    #[derive(Default)]
    pub struct Terminal {
        pub noted_input: Vec<u8>,
    }
    impl Terminal {
        fn note_user_input(&mut self, bytes: &[u8]) {
            self.noted_input.extend_from_slice(bytes);
        }
    }
    #[derive(Default)]
    pub struct TerminalLock(RefCell<Terminal>);
    impl TerminalLock {
        pub fn lock(&self) -> RefMut<'_, Terminal> {
            self.0.borrow_mut()
        }
    }
    pub struct Metadata {
        pub session_id: String,
    }
    pub struct Session {
        pub metadata: Metadata,
        pub purpose: SessionPurpose,
        pub pending_input: Vec<u8>,
        pub terminal: TerminalLock,
    }
    impl Session {
        // @QUEUE_INPUT@
    }
}
struct SessionManager {
    sessions: Vec<session::Session>,
    active: usize,
}
impl SessionManager {
    fn sessions(&self) -> &[session::Session] {
        &self.sessions
    }
    fn active_index(&self) -> usize {
        self.active
    }
    fn get_active_session_mut(&mut self) -> &mut session::Session {
        &mut self.sessions[self.active]
    }
}
// Watch/paint reset storage is unrelated to the attention reducer under test.
#[derive(Default)]
struct ResetBoundary {
    resets: usize,
}
impl ResetBoundary {
    fn reset(&mut self) {
        self.resets += 1;
    }
}
// Deterministic monotonic-clock boundary keeps cooldown assertions independent
// of a paused or overloaded test runner. Production method bodies are unchanged.
struct TestClock(Duration);
impl TestClock {
    fn elapsed(&self) -> Duration {
        self.0
    }
}
struct OrganismHost {
    born: TestClock,
    owner: Option<String>,
    presentable: bool,
    running: bool,
    pointer_inside: bool,
    context: RenderContext,
    attention: LiveAttention<String>,
    activity: LiveActivityKnowledge,
    last_input: Option<Duration>,
    life: WindowLife,
    ambient: AmbientMind,
    watch: ResetBoundary,
    bridge: ResetBoundary,
}
impl OrganismHost {
    // @REQUEST_HELLO@
    // @CANCEL_ATTENTION@
    // @ACCEPTED_INPUT@
    fn snapshot(&self, now: Duration) -> LiveAttentionSnapshot {
        self.attention
            .snapshot(
                now,
                self.owner.as_ref().unwrap(),
                LiveAttentionInput {
                    base_context: self.context,
                    activity: self.activity,
                    hard_eligible: true,
                    hover_available: self.presentable,
                    pointer_inside: self.pointer_inside,
                },
            )
            .unwrap()
    }
    fn policy(&self, config: &Config) -> PresentationPolicy {
        PresentationPolicy {
            enabled: config.ascii_organism_enabled,
            focused_owner: true,
            local: true,
            alternate_screen: false,
            motion: config.ascii_organism_motion,
            last_input: self.last_input,
        }
    }
}
mod block_mode {
    pub enum SelectStep {
        Older,
    }
}
// @SELECTION_ADMISSION@
struct App {
    config: Config,
    session_manager: SessionManager,
    organism: OrganismHost,
    selection: Option<String>,
    status: Option<String>,
    scroll_requests: usize,
}
impl App {
    // @READ_ONLY@
    // @QUEUE_CONTROL@
    fn dispatch_command(&mut self, command: keybindings::Command) {
        match command {
            // @DISPATCH_ARMS@
            _ => panic!("fixture called an unsupported production dispatch arm"),
        }
    }
    fn set_status(&mut self, text: &str) {
        self.status = Some(text.into());
    }
    fn clear_block_selection_for_session(&mut self, session: &str) {
        if self.selection.as_deref() == Some(session) {
            self.selection = None;
        }
    }
    fn block_context_scroll(&mut self, _: block_mode::SelectStep) -> bool {
        false
    }
    fn scroll_active_terminal(&mut self, _: i32) {
        self.scroll_requests += 1;
    }
    fn press(&mut self, bindings: &keybindings::KeyBindings, chord: &str) {
        if let Some(command) = bindings.get_command(chord) {
            self.dispatch_command(command);
        }
    }
    fn greeting(mode: TerminalMode) -> Self {
        let owner = String::from("active-local");
        let mut attention = LiveAttention::default();
        attention.set_owner(Some(owner.clone()));
        let config = Config {
            terminal_mode: Some(mode),
            block_mode: false,
            ascii_organism_enabled: true,
            ascii_organism_motion: Some(OrganismMotion::Full),
        };
        assert_eq!(config.effective_terminal_mode(), mode);
        let mut organism = OrganismHost {
            born: TestClock(Duration::from_millis(25)),
            owner: Some(owner.clone()),
            presentable: config.command_cards_enabled(),
            running: false,
            pointer_inside: false,
            context: PreviewPose::Calm.context(),
            attention,
            activity: LiveActivityKnowledge::KnownIdle,
            last_input: None,
            life: WindowLife::new_at(Duration::ZERO),
            ambient: AmbientMind::seeded(123),
            watch: ResetBoundary::default(),
            bridge: ResetBoundary::default(),
        };
        organism
            .request_hello_at(Duration::ZERO, Some(&owner), &config)
            .unwrap();
        assert!(organism.snapshot(Duration::ZERO).active.is_some());
        Self {
            config,
            session_manager: SessionManager {
                active: 0,
                sessions: vec![session::Session {
                    metadata: session::Metadata {
                        session_id: owner.clone(),
                    },
                    purpose: session::SessionPurpose::Interactive,
                    pending_input: vec![],
                    terminal: session::TerminalLock::default(),
                }],
            },
            organism,
            selection: Some(owner),
            status: None,
            scroll_requests: 0,
        }
    }
    fn assert_unchanged(&self, before: LiveAttentionSnapshot) {
        assert_eq!(self.organism.snapshot(Duration::ZERO), before);
        assert!(self.organism.last_input.is_none());
        assert_eq!(self.organism.activity, LiveActivityKnowledge::KnownIdle);
        assert_eq!(self.organism.watch.resets, 0);
        assert_eq!(self.organism.bridge.resets, 0);
        assert_eq!(self.selection.as_deref(), Some("active-local"));
        assert!(self
            .organism
            .policy(&self.config)
            .body_visible(Duration::ZERO));
        assert!(self.session_manager.sessions[0]
            .terminal
            .lock()
            .noted_input
            .is_empty());
    }
}
const CONTROLS: [(&str, &str, u8); 3] = [
    ("ctrl+c", "terminal:send_sigint", 0x03),
    ("ctrl+d", "terminal:send_eof", 0x04),
    ("ctrl+l", "terminal:clear", 0x0c),
];
const MODES: [TerminalMode; 2] = [TerminalMode::Block, TerminalMode::Chat];

fn configured_controls() -> keybindings::KeyBindings {
    let mut bindings = keybindings::KeyBindings::default_bindings();
    // SIGINT/Clear are configurable commands but are normally delivered by the
    // raw terminal encoder. Install user bindings to exercise their dispatch.
    for (chord, command, _) in CONTROLS {
        bindings.bindings.insert(chord.into(), command.into());
    }
    bindings
}

#[test]
fn default_eof_binding_routes_through_control_admission() {
    let bindings = keybindings::KeyBindings::default_bindings();
    assert_eq!(
        bindings.get_command("ctrl+d"),
        Some(keybindings::Command::TerminalSendEof)
    );
    for mode in MODES {
        let mut app = App::greeting(mode);
        app.press(&bindings, "ctrl+d");
        assert_eq!(app.session_manager.sessions[0].pending_input, [0x04]);
        let now = app.organism.last_input.expect("default EOF must notify");
        assert!(app.organism.snapshot(now).active.is_none());
    }
}

#[test]
fn configurable_controls_cancel_greeting_and_retreat_in_block_and_chat() {
    let bindings = configured_controls();
    for mode in MODES {
        for (chord, _, byte) in CONTROLS {
            let mut app = App::greeting(mode);
            app.press(&bindings, chord);
            let input_at = app
                .organism
                .last_input
                .expect("accepted control must retreat");
            assert_eq!(app.session_manager.sessions[0].pending_input, [byte]);
            assert_eq!(
                app.session_manager.sessions[0].terminal.lock().noted_input,
                [byte]
            );
            assert!(app.organism.snapshot(input_at).active.is_none());
            assert_eq!(app.organism.activity, LiveActivityKnowledge::Unknown);
            assert_eq!(app.organism.watch.resets, 1);
            assert_eq!(app.organism.bridge.resets, 1);
            assert!(app.selection.is_none());
            assert!(app.status.is_none());
            let policy = app.organism.policy(&app.config);
            assert!(!policy.body_visible(input_at));
            assert!(!policy.body_visible(input_at + INPUT_RETREAT - Duration::from_nanos(1)));
            assert!(policy.body_visible(input_at + INPUT_RETREAT));
            // Accepted input cancels feedback, but cannot erase its real cooldown.
            assert_eq!(
                app.organism.request_hello_at(
                    input_at + INPUT_RETREAT,
                    Some("active-local"),
                    &app.config
                ),
                Err("ASCII Organism is cooling down; try again later")
            );
        }
    }
}

#[test]
fn remapped_configurable_controls_preserve_fifo_and_feedback() {
    for mode in MODES {
        for (_, command, byte) in CONTROLS {
            let mut bindings = keybindings::KeyBindings::new();
            bindings
                .bindings
                .insert("ctrl+shift+f12".into(), command.into());
            let mut app = App::greeting(mode);
            app.session_manager.sessions[0].pending_input = b"prior".to_vec();
            app.press(&bindings, "CTRL+SHIFT+F12");
            assert_eq!(
                app.session_manager.sessions[0].pending_input,
                [b"prior".as_slice(), &[byte]].concat()
            );
            let now = app
                .organism
                .last_input
                .expect("remapped control must notify");
            assert!(app.organism.snapshot(now).active.is_none());
            assert!(!app.organism.policy(&app.config).body_visible(now));
        }
    }
}

#[test]
fn retained_read_only_controls_preserve_greeting_and_selection() {
    let bindings = configured_controls();
    for mode in MODES {
        for (chord, _, _) in CONTROLS {
            let mut app = App::greeting(mode);
            app.session_manager.sessions[0].purpose = session::SessionPurpose::RetainedCommand;
            let before = app.organism.snapshot(Duration::ZERO);
            app.press(&bindings, chord);
            app.assert_unchanged(before);
            assert!(app.session_manager.sessions[0].pending_input.is_empty());
            assert_eq!(
                app.status.as_deref(),
                Some("Exited Agent terminals are read-only")
            );
        }
    }
}

#[test]
fn full_retry_queue_preserves_greeting_selection_and_bytes_atomically() {
    let bindings = configured_controls();
    for mode in MODES {
        for (chord, _, _) in CONTROLS {
            let mut app = App::greeting(mode);
            app.session_manager.sessions[0].pending_input =
                vec![0x55; session::PENDING_INPUT_BYTE_CAP];
            let before = app.organism.snapshot(Duration::ZERO);
            app.press(&bindings, chord);
            app.assert_unchanged(before);
            let pending = &app.session_manager.sessions[0].pending_input;
            assert_eq!(pending.len(), session::PENDING_INPUT_BYTE_CAP);
            assert!(pending.iter().all(|byte| *byte == 0x55));
            assert_eq!(
                app.status.as_deref(),
                Some("Terminal input retry buffer is full")
            );
        }
    }
}

#[test]
fn no_effect_bindings_and_scroll_do_not_report_accepted_input() {
    for mode in MODES {
        for command in [None, Some("not:a:command"), Some("terminal:scroll_up")] {
            let mut bindings = keybindings::KeyBindings::new();
            if let Some(command) = command {
                bindings.bindings.insert("f12".into(), command.into());
            }
            let mut app = App::greeting(mode);
            let before = app.organism.snapshot(Duration::ZERO);
            app.press(&bindings, "f12");
            app.assert_unchanged(before);
            assert!(app.session_manager.sessions[0].pending_input.is_empty());
            assert_eq!(
                app.scroll_requests,
                usize::from(command == Some("terminal:scroll_up"))
            );
            assert!(app.status.is_none());
        }
    }
}

#[test]
fn accepted_control_does_not_cancel_another_sessions_greeting() {
    for mode in MODES {
        let mut app = App::greeting(mode);
        app.session_manager.sessions[0].metadata.session_id = "another-session".into();
        let before = app.organism.snapshot(Duration::ZERO);
        app.press(&configured_controls(), "ctrl+c");
        assert_eq!(app.session_manager.sessions[0].pending_input, [0x03]);
        assert_eq!(app.organism.snapshot(Duration::ZERO), before);
        assert!(app.organism.last_input.is_none());
        assert_eq!(app.organism.watch.resets, 0);
    }
}
