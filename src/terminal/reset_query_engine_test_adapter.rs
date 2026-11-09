//! Invocation-only bridges for unchanged pre-extraction helper-level tests.
//! No reset or reply algorithm lives here: all operations use the live dispatcher.
use super::*;
use jterm_engine::semantics::{dispatch_local, Dispatch};

impl TerminalState {
    fn reset_query_test_dispatch(&mut self, event: Event<'_>) {
        let Dispatch::Handled(effects) =
            dispatch_local(&mut self.screen, &mut self.protocol, &event)
        else {
            panic!("engine did not handle reset/query event");
        };
        self.apply_local_semantic_effects(effects);
    }

    pub(super) fn soft_reset(&mut self) {
        self.reset_query_test_dispatch(Event::Csi {
            raw_params: Vec::new(),
            params: Vec::new(),
            colon_flags: Vec::new(),
            private_prefix: None,
            intermediates: vec![b'!'],
            final_byte: b'p',
        });
    }

    pub(super) fn handle_decrqss(&mut self, selector: &[u8]) {
        self.reset_query_test_dispatch(Event::String {
            kind: StringKind::Dcs,
            payload: [b"$q".as_slice(), selector].concat().into(),
            bel_terminated: false,
        });
    }

    pub(super) fn decrqss_sgr_payload(&self) -> String {
        let mut probe = TerminalState::new(1, 1);
        probe.screen.current_flags = self.screen.current_flags;
        probe.screen.current_fg = self.screen.current_fg;
        probe.screen.current_bg = self.screen.current_bg;
        probe.handle_decrqss(b"m");
        let reply = probe.output_buffer;
        let payload = reply
            .strip_prefix(b"\x1bP1$r")
            .unwrap()
            .strip_suffix(b"\x1b\\")
            .unwrap();
        String::from_utf8(payload.to_vec()).unwrap()
    }

    pub(super) fn sgr_color_params(color: Color, foreground: bool) -> Vec<String> {
        let mut probe = TerminalState::new(1, 1);
        if foreground {
            probe.screen.current_fg = color;
        } else {
            probe.screen.current_bg = color;
        }
        let payload = probe.decrqss_sgr_payload();
        // Decode the framing of a color-only SGR reply; no color mapping here.
        if payload == "0m" {
            Vec::new()
        } else {
            payload
                .strip_suffix('m')
                .unwrap()
                .split(';')
                .map(str::to_owned)
                .collect()
        }
    }
}
