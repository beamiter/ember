//! Frozen Ember fallback admission and transaction loops before shared CSI execution.
use super::*;
use std::cell::RefCell;
impl TerminalState {
    pub(super) fn frozen_ember_dispatch(
        &mut self,
        params: &[u16],
        cmd: char,
        private_prefix: Option<u8>,
        intermediates: &[u8],
    ) {
        // xterm semantics: an explicit 0 count means 1 for every movement,
        // insert/delete, erase-character and scroll control below.
        if let Some(operation) = jterm_engine::display_erase::prepare_display_erase(
            jterm_engine::display_erase::DisplayEraseProfile::Ember,
            &self.vt.screen,
            params,
            cmd,
            private_prefix,
            intermediates,
        ) {
            self.frozen_ember_display(operation);
            observe(self, "completed");
            self.observe_chat_native_takeover();
            return;
        }
        if let Some(operation) = jterm_engine::region_ops::prepare_region_operation(
            jterm_engine::region_ops::RegionOpsProfile::Ember,
            &self.vt.screen,
            params,
            cmd,
        ) {
            self.frozen_ember_region(operation);
            observe(self, "completed");
            self.observe_chat_native_takeover();
            return;
        }
        if let Some(command) = jterm_engine::mode_mutation::prepare_native_mode_command(
            jterm_engine::mode_mutation::NativeModeProfile::Ember,
            params,
            cmd,
            private_prefix,
        ) {
            self.frozen_ember_mode(command);
            observe(self, "completed");
            self.observe_chat_native_takeover();
            return;
        }
        if let Some(mut repeat) =
            jterm_engine::graphic_write::RepeatGraphics::prepare(&self.vt.screen, params, cmd)
        {
            while let Some(operation) = repeat.next(
                jterm_engine::graphic_write::GraphicProfile::Ember,
                &self.vt.screen,
                self.vt.protocol.modes().contains(&7),
                crate::char_width::cached_char_width,
            ) {
                self.frozen_ember_graphic(operation);
            }
            observe(self, "completed");
            self.observe_chat_native_takeover();
            return;
        }
        match cmd {
            't' if private_prefix.is_none() && intermediates.is_empty() => {
                // XTWINOPS - window manipulation. Only the reporting and
                // title-stack ops; the mutating ones (resize/move/iconify) are
                // deliberately ignored, as frost ignores them.
                observe(self, "window");
                self.handle_window_ops(params);
            }
            'n' if private_prefix == Some(b'?') && params.first() == Some(&996) => {
                let response = format!("\x1b[?997;{}n", self.color_scheme_report());
                observe(self, "reply");
                self.output_buffer.extend(response.as_bytes());
            }
            'q' => {
                if private_prefix == Some(b'>')
                    && intermediates.is_empty()
                    && params.first().copied().unwrap_or(0) == 0
                {
                    observe(self, "version");
                    crate::debug_log!("[XTVERSION] report terminal version request");
                    self.output_buffer.extend_from_slice(XTERM_VERSION_RESPONSE);
                }
            }
            _ => {}
        }
        observe(self, "completed");
        self.observe_chat_native_takeover();
    }
    fn frozen_ember_display(&mut self, operation: jterm_engine::display_erase::DisplayErase) {
        use jterm_engine::display_erase::{DisplayErase, DisplayEraseStep};
        match operation {
            DisplayErase::Rows(mut operation) => {
                while let Some(step) = operation.step(&mut self.vt.screen) {
                    observe(self, "erase");
                    match step {
                        DisplayEraseStep::ScreenErased => self.inline_cli_screen_erased(),
                        DisplayEraseStep::Mutations(effects) => {
                            for effect in effects {
                                self.apply_screen_mutation(effect);
                            }
                        }
                        DisplayEraseStep::DirtyRows(rows) => {
                            self.mark_rows_dirty(*rows.start(), *rows.end())
                        }
                    }
                }
            }
            DisplayErase::Screen => {
                observe(self, "erase-screen");
                // ED 擦除显示不移动光标(VT 规范)
                if self.vt.sync_output.active {
                    if self.vt.screen.use_alt_buffer {
                        self.archive_visible_screen_to_scrollback_with_options(true, true);
                    } else {
                        self.archive_primary_screen_unless_last_synced_snapshot();
                    }
                } else {
                    self.archive_visible_screen_to_scrollback();
                }
                self.inline_cli_screen_erased();
                self.erase_screen();
            }
            DisplayErase::SavedLines
                if self.vt.screen.use_alt_buffer && self.inline_cli_owner_active() =>
            {
                observe(self, "erase-saved");
                // A temporary CLI screen cannot erase its primary
                // conversation's saved lines.
            }
            DisplayErase::SavedLines => {
                observe(self, "erase-saved");
                // Clear scrollback buffer (xterm extension)
                self.clear_scrollback_with_projection_provenance();
                self.invalidate_scrollback_view_cache();
                self.kitty_graphics.clear_scrollback_placements();
                self.scroll_offset = 0;
            }
            DisplayErase::Ignored => {}
        }
    }
    fn frozen_ember_mode(
        &mut self,
        mut command: jterm_engine::mode_mutation::NativeModeCommand<'_>,
    ) {
        while let Some(effect) = command.step(
            &mut self.vt.screen,
            &mut self.vt.protocol,
            self.host_window_focused,
        ) {
            observe(self, "mode");
            self.apply_mode_effect(effect);
        }
    }
    pub(super) fn frozen_ember_region(
        &mut self,
        mut operation: jterm_engine::region_ops::RegionOperation,
    ) {
        use jterm_engine::region_ops::RegionStep;
        loop {
            let allow_alt_archive = self.vt.sync_output.active && !self.inline_cli_owner_active();
            let next = &mut self.next_raw_row_id;
            let Some(step) = operation.step(&mut self.vt.screen, allow_alt_archive, || {
                allocate_raw_row_id_from(next)
            }) else {
                break;
            };
            observe(self, "region");
            match step {
                RegionStep::Scroll {
                    mutation,
                    transfer_outgoing,
                    allow_alt_scrollback,
                    ..
                } => {
                    self.apply_vertical_region_effects(
                        mutation,
                        transfer_outgoing,
                        allow_alt_scrollback,
                    );
                }
                RegionStep::Lines { mutation, dirty } => {
                    if let Some(effect) = mutation {
                        self.apply_region_identity_effects(&effect);
                        match effect.direction {
                            RegionDirection::Up => self.kitty_graphics.scroll_region_up(
                                effect.top,
                                effect.bottom,
                                effect.count,
                                false,
                            ),
                            RegionDirection::Down => self.kitty_graphics.scroll_region_down(
                                effect.top,
                                effect.bottom,
                                effect.count,
                            ),
                        }
                    }
                    self.mark_rows_dirty(*dirty.start(), *dirty.end());
                }
            }
        }
    }
    pub(super) fn frozen_ember_graphic(
        &mut self,
        mut operation: jterm_engine::graphic_write::GraphicWrite,
    ) {
        use jterm_engine::graphic_write::GraphicStep;
        while let Some(effect) = operation.step(&mut self.vt.screen) {
            observe(self, "graphic");
            match effect {
                GraphicStep::Wrap => {
                    self.wrap_to_next_line();
                }
                GraphicStep::Composed(mutation) => {
                    self.apply_screen_mutation(mutation);
                }
                GraphicStep::Written { mutation, .. } => {
                    self.apply_screen_mutation(mutation);
                }
                GraphicStep::ObserveIdle { .. } => unreachable!("Frost-only idle effect"),
            }
        }
    }
}

type Snapshot = std::collections::BTreeMap<String, String>;
thread_local! { static TRACE: RefCell<Option<Vec<(String,Snapshot)>>> = const { RefCell::new(None) }; }
fn snapshot(t: &TerminalState) -> Snapshot {
    let mut out = super::super::alternate_parity_tests::snapshot(t);
    out.insert(
        "chat_native_takeover_sequence".into(),
        format!("{:?}", t.chat_native_takeover_sequence),
    );
    out.insert(
        "inline_cli_launch_floor".into(),
        format!("{:?}", t.inline_cli_launch_floor),
    );
    out.insert(
        "inline_cli_command_scope".into(),
        format!("{:?}", t.inline_cli_command_scope),
    );
    out
}
pub(in crate::terminal) fn observe(t: &TerminalState, event: &str) {
    if TRACE.with(|trace| trace.borrow().is_none()) {
        return;
    }
    let snapshot = snapshot(t);
    TRACE.with(|trace| {
        trace
            .borrow_mut()
            .as_mut()
            .unwrap()
            .push((event.to_owned(), snapshot))
    });
}
fn capture(f: impl FnOnce()) -> Vec<(String, Snapshot)> {
    TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
    f();
    TRACE.with(|trace| trace.borrow_mut().take().unwrap())
}
fn seed(cols: usize, bits: usize) -> TerminalState {
    let mut t = TerminalState::new(cols, 3);
    t.process_input(b"old\r\nhistory\r\nmore\r\nlast");
    if bits & 1 != 0 {
        t.enter_alt_screen(true, true);
    }
    t.vt.screen.cursor_row = 2;
    t.vt.screen.cursor_col = cols - 1;
    t.vt.screen.pending_wrap = bits & 2 != 0;
    t.vt.screen.insert_mode = bits & 4 != 0;
    t.vt.screen.scroll_region_top = 1;
    t.vt.screen.scroll_region_bottom = 2;
    t.vt.screen.last_printed_char = Some('q');
    t.host_window_focused = true;
    if bits & 8 != 0 {
        t.set_inline_cli_launch(true);
    }
    t
}
#[test]
fn shared_ember_csi_matches_frozen_fallback_state_and_ordered_effects() {
    let cases: &[(&[u16], char)] = &[
        (&[0], 'J'),
        (&[1], 'J'),
        (&[2], 'S'),
        (&[2], 'T'),
        (&[2], 'L'),
        (&[1], 'M'),
        (&[3], 'b'),
        (&[1004, 6, 1049, 2026], 'h'),
        (&[1049, 6], 'l'),
        (&[22, 0], 't'),
        (&[18], 't'),
        (&[23, 0], 't'),
        (&[996], 'n'),
        (&[0], 'q'),
        (&[2026], 'l'),
        (&[2], 'J'),
        (&[3], 'J'),
        (&[5522], 'l'),
    ];
    for cols in [1, 2, 7] {
        for bits in 0..16 {
            for prefix in [None, Some(b'?'), Some(b'>'), Some(b'=')] {
                for intermediate in [&b""[..], b" ", b"!"] {
                    let mut a = seed(cols, bits);
                    let mut e = seed(cols, bits);
                    for &(params, cmd) in cases {
                        let actual =
                            capture(|| a.handle_escape_sequence(params, cmd, prefix, intermediate));
                        let expected =
                            capture(|| e.frozen_ember_dispatch(params, cmd, prefix, intermediate));
                        assert_eq!(actual,expected,"cols={cols} bits={bits} cmd={cmd} params={params:?} prefix={prefix:?} intermediates={intermediate:?}");
                        assert_eq!(snapshot(&a), snapshot(&e));
                    }
                }
            }
        }
    }
}
