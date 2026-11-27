use super::*;

use jterm_engine::parser::RejectReason;
#[cfg(test)]
use jterm_engine::parser::{Event, StringKind};

impl super::TerminalState {
    fn handle_kitty_apc_payload(&mut self, payload: &[u8]) {
        if payload.first() != Some(&b'G') {
            return;
        }
        let Ok(payload) = std::str::from_utf8(payload) else {
            self.kitty_graphics
                .reject_graphics_payload(payload, "Kitty graphics command is not valid UTF-8");
            self.drain_kitty_graphics_responses();
            return;
        };
        let cursor_col = u32::try_from(self.vt.screen.cursor_col).unwrap_or(u32::MAX);
        let cursor_row = u32::try_from(self.vt.screen.cursor_row).unwrap_or(u32::MAX);
        let parsed = self
            .kitty_graphics
            .parse_graphics_payload_at(payload, cursor_col, cursor_row);
        if let Err(_error) = parsed {
            crate::debug_log!("[APC] Kitty graphics error: {}", _error);
        } else if let Some((columns, rows)) = self.kitty_graphics.take_cursor_movement() {
            self.vt.screen.pending_wrap = false;
            let last_col = self.vt.screen.grid.row_len().saturating_sub(1);
            self.vt.screen.cursor_col = self
                .vt.screen
                .cursor_col
                .saturating_add(columns as usize)
                .min(last_col);
            let last_row = if self.vt.screen.cursor_row >= self.vt.screen.scroll_region_top
                && self.vt.screen.cursor_row <= self.vt.screen.scroll_region_bottom
            {
                self.vt.screen.scroll_region_bottom
            } else {
                self.vt.screen.grid.rows().saturating_sub(1)
            };
            self.vt.screen.cursor_row = self
                .vt.screen
                .cursor_row
                .saturating_add(rows as usize)
                .min(last_row);
        }
        self.observe_chat_native_takeover();
        self.drain_kitty_graphics_responses();
    }

    fn drain_kitty_graphics_responses(&mut self) {
        let responses = self.kitty_graphics.take_responses();
        if !responses.is_empty() {
            self.output_buffer.extend_from_slice(&responses);
        }
    }

    fn handle_osc_payload(
        &mut self,
        payload: &[u8],
        bel_terminated: bool,
        packet_end: Option<u64>,
    ) {
        use jterm_engine::osc::{NativeOsc, NativeOscProfile, OscPacket};

        let Some(event) = OscPacket::decode(payload, bel_terminated).native_event(
            NativeOscProfile::Ember,
            &mut self.osc_reply_terminator,
            &mut self.shell_marker_clock,
            packet_end,
        ) else {
            return;
        };
        match event {
            NativeOsc::Title(command, value) => {
                self.titles.set_osc(command, &Self::sanitized_title(value));
            }
            NativeOsc::Directory(value) => {
                self.current_working_dir = self.decode_osc7_cwd(value);
                Self::observe_organism_directory(
                    &mut self.organism_working_dir,
                    if self.vt.screen.use_alt_buffer {
                        None
                    } else {
                        self.current_working_dir.as_deref()
                    },
                );
                crate::debug_log!("[OSC7] cwd set to {:?}", self.current_working_dir);
            }
            NativeOsc::Hyperlink(value) => {
                self.vt.screen.current_hyperlink = self.hyperlinks.apply_osc8(value);
            }
            NativeOsc::Color(command, value) => self.handle_osc_color(command, value),
            NativeOsc::Notification(notification) => notification.enqueue(
                &mut self.pending_notifications,
                || self.host_context.notification_app_name.clone(),
                jterm_core::review_input::is_terminal_visual_spoofing_character,
            ),
            NativeOsc::JshSession(value) => self.announced_jsh_session_id = Some(value.to_owned()),
            NativeOsc::Clipboard(value) => self.handle_osc_52(value),
            NativeOsc::ShellMarker(value, position) => self.handle_osc_133(value, position),
            NativeOsc::ClipboardExtension(metadata, value) => self.handle_osc_5522(metadata, value),
        }
    }

    /// LF / IND (and VT, FF, NEL's second half): move down one row, scrolling
    /// only when exactly on the bottom margin; below the region the cursor
    /// just moves down and never scrolls.
    fn line_feed(&mut self) {
        let prior_total = self.total_lines_scrolled;
        let departure = self
            .apply_vertical_motion(VerticalMotion::LineFeed)
            .unwrap();
        let departed_boundary = BufferAnchor {
            line_id: prior_total.saturating_add(departure.row as u64),
            column: departure.width,
        };
        let next_boundary = BufferAnchor {
            line_id: self
                .total_lines_scrolled
                .saturating_add(self.vt.screen.cursor_row as u64),
            column: 0,
        };
        self.note_output_hard_line_advance(departure.row_id, departed_boundary, next_boundary);
    }

    /// DECALN (`ESC # 8`): fill the screen with 'E', reset the margins and home
    /// the cursor, as xterm does.
    fn screen_alignment_test(&mut self) {
        let cols = self.vt.screen.grid.row_len();
        let rows = self.vt.screen.grid.rows();
        let spans: Vec<_> = (0..rows).map(|row| (row, 0, cols)).collect();
        self.invalidate_grid_mutation_spans(&spans);
        self.vt.screen.screen_alignment_test();
        self.note_output_cursor_reposition(true);
        self.dirty_region.mark_all(rows);
        self.mark_rows_dirty(0, rows.saturating_sub(1));
    }

    pub(super) fn apply_local_semantic_effects(
        &mut self,
        effects: jterm_engine::semantics::Effects,
    ) {
        use jterm_engine::semantics::Effect;
        for effect in effects {
            match effect {
                Effect::Row(mutation) => self.apply_screen_mutation(mutation),
                Effect::CursorReposition { non_linear } => {
                    self.note_output_cursor_reposition(non_linear)
                }
                Effect::Reply(bytes) => self.output_buffer.extend(bytes),
                Effect::Bell => self.pending_bell = true,
                Effect::GlobalBackground(color) => self.global_bg = color,
            }
        }
    }

    pub fn process_input(&mut self, input: &[u8]) {
        if !input.is_empty() {
            self.invalidate_search_source();
        }
        let stream_start = self.shell_marker_clock.begin_feed(input.len());
        jterm_engine::native_terminal::NativeTerminal::feed_ember(self, input, stream_start);
    }



    #[cfg(test)]
    pub(super) fn handle_escape_sequence(
        &mut self,
        params: &[u16],
        cmd: char,
        private_prefix: Option<u8>,
        intermediates: &[u8],
    ) {
        jterm_engine::ember_csi::execute(self, params, cmd, private_prefix, intermediates);
    }

    /// 擦除整个屏幕单元格(保留当前背景色),不移动光标。
    /// 供 ED(`CSI 2J`)使用 —— 按 VT 规范擦除显示不得移动光标。
    pub(super) fn erase_screen(&mut self) {
        self.vt.screen.pending_wrap = false;
        let cols = self.vt.screen.grid.row_len();
        let spans: Vec<_> = (0..self.vt.screen.grid.rows())
            .map(|row| (row, 0, cols))
            .collect();
        self.invalidate_grid_mutation_spans(&spans);
        self.vt.screen.erase_screen();
        // Mark all rows as dirty
        self.dirty_region.mark_all(self.vt.screen.grid.rows());
        self.mark_rows_dirty(0, self.vt.screen.grid.rows().saturating_sub(1));
        self.kitty_graphics.clear_placements();
    }

    pub(super) fn enter_alt_screen(&mut self, save_cursor: bool, clear: bool) {
        let before = self.vt.screen.use_alt_buffer;
        jterm_engine::native_alternate::enter_ember(self, save_cursor, clear);
        if before != self.vt.screen.use_alt_buffer { self.retire_alternate_scroll(); }
    }

    pub(super) fn exit_alt_screen(&mut self, restore_cursor: bool) {
        let before = self.vt.screen.use_alt_buffer;
        jterm_engine::native_alternate::leave_ember(self, restore_cursor);
        if before != self.vt.screen.use_alt_buffer { self.retire_alternate_scroll(); }
    }

    /// Titles are rendered in trusted app chrome (tabs, window manager,
    /// XTWINOPS replies). Drop line/layout controls and replace visual
    /// spoofing so PTY output cannot create multiline labels or reorder them.
    fn sanitized_title(title: &str) -> String {
        title
            .chars()
            .filter_map(|ch| {
                if ch.is_control() {
                    None
                } else if jterm_core::review_input::is_visual_spoofing_character(ch) {
                    Some('\u{fffd}')
                } else {
                    Some(ch)
                }
            })
            .take(MAX_WINDOW_TITLE_CHARS)
            .collect()
    }

    pub(super) fn handle_window_ops(&mut self, params: &[u16]) {
        use jterm_engine::native_window_ops::{
            apply_native_window_op, NativeWindowGeometry, NativeWindowPixels,
        };
        let (width, height) = self.kitty_graphics.cell_size_pixels();
        apply_native_window_op(
            params,
            NativeWindowGeometry {
                columns: self.vt.screen.grid.row_len(),
                rows: self.vt.screen.grid.rows(),
                pixels: NativeWindowPixels::Cell { width, height },
            },
            &mut self.titles,
            jterm_engine::titles::TitleState::reportable_title,
            &mut self.output_buffer,
        );
    }

    pub(super) fn apply_mode_effect(
        &mut self,
        effect: jterm_engine::mode_mutation::NativeModeEffect,
    ) {
        use jterm_engine::mode_mutation::NativeModeEffect;
        match effect {
            NativeModeEffect::None => {}
            NativeModeEffect::Reply(bytes) => self.output_buffer.extend_from_slice(bytes),
            NativeModeEffect::RevokePasteGrant => self.osc5522.revoke(),
            NativeModeEffect::MouseReportingChanged => {
                self.retire_alternate_scroll();
            }
            NativeModeEffect::AlternateScreen { mode, enabled } => {
                if enabled {
                    match mode {
                        47 => self.enter_alt_screen(false, false),
                        1047 => self.enter_alt_screen(false, true),
                        1049 => self.enter_alt_screen(true, true),
                        _ => unreachable!("shared alternate mode"),
                    }
                    self.vt.protocol.modes_mut().insert(mode);
                } else {
                    self.exit_alt_screen(mode == 1049);
                    self.vt.protocol.modes_mut().remove(&mode);
                }
            }
            NativeModeEffect::SynchronizedOutput { enabled } => {
                if enabled {
                    let clock = &self.sync_output_clock;
                    self.vt.sync_output
                        .begin_native(&mut self.vt.protocol, || clock.now());
                } else {
                    self.apply_sync_end(self.vt.sync_output.explicit_end());
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn set_mode(&mut self, mode: u16) {
        let effect = jterm_engine::mode_mutation::apply_native_mode_parameter(
            jterm_engine::mode_mutation::NativeModeProfile::Ember,
            &mut self.vt.screen,
            &mut self.vt.protocol,
            jterm_engine::mode_mutation::ModeNamespace::DecPrivate,
            mode,
            true,
            self.host_window_focused,
        );
        self.apply_mode_effect(effect);
    }

    #[cfg(test)]
    pub(super) fn reset_mode(&mut self, mode: u16) {
        let effect = jterm_engine::mode_mutation::apply_native_mode_parameter(
            jterm_engine::mode_mutation::NativeModeProfile::Ember,
            &mut self.vt.screen,
            &mut self.vt.protocol,
            jterm_engine::mode_mutation::ModeNamespace::DecPrivate,
            mode,
            false,
            self.host_window_focused,
        );
        self.apply_mode_effect(effect);
    }

    #[cfg(test)]
    pub(super) fn set_ansi_mode(&mut self, mode: u16) {
        let effect = jterm_engine::mode_mutation::apply_native_mode_parameter(
            jterm_engine::mode_mutation::NativeModeProfile::Ember,
            &mut self.vt.screen,
            &mut self.vt.protocol,
            jterm_engine::mode_mutation::ModeNamespace::Ansi,
            mode,
            true,
            self.host_window_focused,
        );
        self.apply_mode_effect(effect);
    }

    #[cfg(test)]
    pub(super) fn reset_ansi_mode(&mut self, mode: u16) {
        let effect = jterm_engine::mode_mutation::apply_native_mode_parameter(
            jterm_engine::mode_mutation::NativeModeProfile::Ember,
            &mut self.vt.screen,
            &mut self.vt.protocol,
            jterm_engine::mode_mutation::ModeNamespace::Ansi,
            mode,
            false,
            self.host_window_focused,
        );
        self.apply_mode_effect(effect);
    }
}

#[cfg(test)]
#[path = "reset_query_oracles.rs"]
mod reset_query_oracles;

#[cfg(test)]
#[path = "reset_query_engine_test_adapter.rs"]
mod reset_query_engine_test_adapter;

#[cfg(test)]
#[path = "decrqm_oracles.rs"]
mod decrqm_oracles;

#[cfg(test)]
#[path = "decrqm_engine_test_adapter.rs"]
mod decrqm_engine_test_adapter;

#[cfg(test)]
#[path = "mode_mutation_oracles.rs"]
mod mode_mutation_oracles;

#[cfg(test)]
#[path = "fullscreen_fill_oracles.rs"]
mod fullscreen_fill_oracles;

#[cfg(test)]
#[path = "vertical_motion_oracles.rs"]
pub(super) mod vertical_motion_oracles;

#[cfg(test)]
#[path = "color_oracles.rs"]
mod color_oracles;

#[cfg(test)]
#[path = "title_oracles.rs"]
mod title_oracles;

#[cfg(test)]
#[path = "ascii_oracles.rs"]
mod ascii_oracles;

#[cfg(test)]
#[path = "display_erase_dispatch_oracles.rs"]
pub(super) mod display_erase_dispatch_oracles;

#[cfg(test)]
#[path = "mode_command_oracles.rs"]
mod mode_command_oracles;

impl jterm_engine::ember_csi::EmberCsiHost for TerminalState {
    fn track_alternate_scroll_changes(&self) -> bool { true }

    fn csi_state(&mut self) -> (&mut ScreenState, &mut ProtocolState, &mut u64) {
        (
            &mut self.vt.screen,
            &mut self.vt.protocol,
            &mut self.next_raw_row_id,
        )
    }
    fn focused(&self) -> bool {
        self.host_window_focused
    }
    fn allow_alt_archive(&self) -> bool {
        self.vt.sync_output.active && !self.inline_cli_owner_active()
    }
    fn color_scheme_report(&self) -> u8 {
        TerminalState::color_scheme_report(self)
    }
    fn graphic_width(ch: char) -> usize {
        crate::char_width::cached_char_width(ch)
    }
    fn allocate_row_id(next: &mut u64) -> RawRowId {
        super::state::allocate_raw_row_id_from(next)
    }
    fn csi_effect(&mut self, effect: jterm_engine::ember_csi::EmberCsiEffect<'_>) {
        use jterm_engine::csi_transaction::CsiTransactionEffect;
        use jterm_engine::display_erase::DisplayEraseStep;
        use jterm_engine::ember_csi::EmberCsiEffect;
        #[cfg(test)]
        super::state::ember_csi_oracles::observe(
            self,
            match &effect {
                EmberCsiEffect::Transaction(step) => match step {
                    CsiTransactionEffect::Erase(_) => "erase",
                    CsiTransactionEffect::EraseScreen => "erase-screen",
                    CsiTransactionEffect::EraseSavedLines => "erase-saved",
                    CsiTransactionEffect::Region(_) => "region",
                    CsiTransactionEffect::Mode(_) => "mode",
                    CsiTransactionEffect::Graphic(_) => "graphic",
                },
                EmberCsiEffect::Window(_) => "window",
                EmberCsiEffect::Reply(_) => "reply",
                EmberCsiEffect::VersionReply => "version",
                EmberCsiEffect::Completed => "completed",
            },
        );
        match effect {
            EmberCsiEffect::Transaction(step) => match step {
                CsiTransactionEffect::Erase(step) => match step {
                    DisplayEraseStep::ScreenErased => self.inline_cli_screen_erased(),
                    DisplayEraseStep::Mutations(effects) => {
                        for effect in effects {
                            self.apply_screen_mutation(effect);
                        }
                    }
                    DisplayEraseStep::DirtyRows(rows) => {
                        self.mark_rows_dirty(*rows.start(), *rows.end())
                    }
                },
                CsiTransactionEffect::EraseScreen => {
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
                CsiTransactionEffect::EraseSavedLines
                    if self.vt.screen.use_alt_buffer && self.inline_cli_owner_active() =>
                {
                    // A temporary CLI screen cannot erase its primary
                    // conversation's saved lines.
                }
                CsiTransactionEffect::EraseSavedLines => {
                    // Clear scrollback buffer (xterm extension)
                    self.clear_scrollback_with_projection_provenance();
                    self.invalidate_scrollback_view_cache();
                    self.kitty_graphics.clear_scrollback_placements();
                    self.scroll_offset = 0;
                }

                CsiTransactionEffect::Region(step) => self.apply_csi_region_step(step),
                CsiTransactionEffect::Mode(effect) => {
                    self.apply_mode_effect(effect);
                    #[cfg(test)]
                    mode_command_oracles::observe(self);
                }
                CsiTransactionEffect::Graphic(effect) => self.apply_graphic_step(effect),
            },
            EmberCsiEffect::Window(params) => self.handle_window_ops(params),
            EmberCsiEffect::Reply(bytes) => self.output_buffer.extend(bytes),
            EmberCsiEffect::VersionReply => {
                crate::debug_log!("[XTVERSION] report terminal version request");
                self.output_buffer.extend_from_slice(XTERM_VERSION_RESPONSE);
            }
            EmberCsiEffect::Completed => self.observe_chat_native_takeover(),
        }
    }
}

impl jterm_engine::native_terminal::NativeTerminalHost for TerminalState {
    fn native_terminal(&mut self) -> &mut jterm_engine::native_terminal::NativeTerminal {
        &mut self.vt
    }
    fn native_effect(&mut self, effect: jterm_engine::native_terminal::NativeEffect<'_>) {
        use jterm_engine::native_terminal::NativeEffect;
        match effect {
            NativeEffect::AbandonApc => self.kitty_graphics.abandon_pending_transfer(),
            NativeEffect::Local(effects) => self.apply_local_semantic_effects(effects),
            NativeEffect::Graphic(effect) => self.apply_graphic_step(effect),
            NativeEffect::Ascii(effect) => self.apply_ascii_step(effect),
            NativeEffect::Vertical(VerticalMotion::LineFeed) => self.line_feed(),
            NativeEffect::Vertical(motion) => {
                self.apply_vertical_motion(motion);
            }
            NativeEffect::CursorReposition { non_linear } => {
                self.note_output_cursor_reposition(non_linear)
            }
            NativeEffect::Reset => self.hard_reset(),
            NativeEffect::Alignment => self.screen_alignment_test(),
            NativeEffect::Osc {
                payload,
                bel_terminated,
                packet_end,
            } => self.handle_osc_payload(payload, bel_terminated, packet_end),
            NativeEffect::KittyApc(payload) => self.handle_kitty_apc_payload(payload),
            NativeEffect::RejectApc { recovery, reason } => {
                let message = match reason {
                    RejectReason::Cancelled => "Kitty graphics APC was cancelled",
                    RejectReason::TooLarge => "Kitty graphics APC exceeded the parser size limit",
                };
                self.kitty_graphics
                    .reject_graphics_payload(recovery, message);
                self.drain_kitty_graphics_responses();
            }
            NativeEffect::ObserveTakeover => self.observe_chat_native_takeover(),
            NativeEffect::FrostState(_) | NativeEffect::CaptureBytes(_) => {
                unreachable!("Frost-only native effect")
            }
        }
    }
}
