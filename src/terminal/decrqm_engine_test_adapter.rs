//! Test-only invocation bridge; mode semantics and reply framing stay in engine.
use super::*;
use jterm_engine::{
    screen::ScreenState,
    semantics::{dispatch_local, Dispatch, Effect},
    TerminalGrid,
};

impl TerminalState {
    fn decrqm_test_mode_state(&self, mode: u16, private: bool) -> u8 {
        let mut screen = ScreenState::with_grids(TerminalGrid::new(1, 1), TerminalGrid::new(1, 1));
        screen.origin_mode = self.screen.origin_mode;
        screen.insert_mode = self.screen.insert_mode;
        let mut protocol = self.protocol.clone();
        let event = Event::Csi {
            raw_params: mode.to_string().into_bytes(),
            params: vec![mode],
            colon_flags: vec![false],
            private_prefix: private.then_some(b'?'),
            intermediates: vec![b'$'],
            final_byte: b'p',
        };
        let Dispatch::Handled(effects) = dispatch_local(&mut screen, &mut protocol, &event) else {
            panic!("engine did not handle DECRQM");
        };
        assert_eq!(effects.len(), 1);
        let Effect::Reply(reply) = &effects[0] else {
            panic!("expected reply effect")
        };
        let prefix = format!("\x1b[{}{mode};", if private { "?" } else { "" });
        let payload = reply
            .strip_prefix(prefix.as_bytes())
            .unwrap()
            .strip_suffix(b"$y")
            .unwrap();
        std::str::from_utf8(payload).unwrap().parse().unwrap()
    }
    pub(super) fn decrqm_private_mode_state(&self, mode: u16) -> u8 {
        self.decrqm_test_mode_state(mode, true)
    }
    pub(super) fn decrqm_ansi_mode_state(&self, mode: u16) -> u8 {
        self.decrqm_test_mode_state(mode, false)
    }
}
