use super::*;

use jterm_engine::mode_mutation::{apply_ember_mode, ModeNamespace};
use jterm_engine::parser::{Event, RejectReason, StringKind};

impl super::TerminalState {
    /// Sanitise one OSC 9 / OSC 777 notification field.
    ///
    /// These two strings are the only PTY-authored text ember hands to a
    /// process outside itself: they go to `notify-send` and are drawn by the
    /// desktop's notification server, which applies none of the terminal's
    /// own display rules. They arrived with controls and bidi overrides
    /// intact, so `printf '\e]777;notify;<RLO>Security Update;<RLO>approve\a'`
    /// put attacker-reordered text into a system toast, outside ember's
    /// sanitisation boundary and wearing the desktop's chrome rather than the
    /// terminal's.
    ///
    /// Offending scalars become U+FFFD rather than disappearing, so a rewritten
    /// field reads as rewritten instead of as a shorter honest one. This is the
    /// shared parser's `bounded_notification_field` rule, on the terminal-strict
    /// spoofing class, since a toast is a terminal-adjacent rendered surface.
    fn safe_notification_field(raw: &str) -> String {
        raw.chars()
            .map(|ch| {
                if ch.is_control()
                    || jterm_core::review_input::is_terminal_visual_spoofing_character(ch)
                {
                    '\u{fffd}'
                } else {
                    ch
                }
            })
            .take(jterm_core::parser::MAX_NOTIFICATION_CHARS)
            .collect::<String>()
            .trim()
            .to_owned()
    }

    fn notification_field_is_rewritten(field: &str) -> bool {
        field.contains('\u{fffd}')
    }

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
        let cursor_col = u32::try_from(self.screen.cursor_col).unwrap_or(u32::MAX);
        let cursor_row = u32::try_from(self.screen.cursor_row).unwrap_or(u32::MAX);
        let parsed = self
            .kitty_graphics
            .parse_graphics_payload_at(payload, cursor_col, cursor_row);
        if let Err(_error) = parsed {
            crate::debug_log!("[APC] Kitty graphics error: {}", _error);
        } else if let Some((columns, rows)) = self.kitty_graphics.take_cursor_movement() {
            self.screen.pending_wrap = false;
            let last_col = self.screen.grid.row_len().saturating_sub(1);
            self.screen.cursor_col = self
                .screen
                .cursor_col
                .saturating_add(columns as usize)
                .min(last_col);
            let last_row = if self.screen.cursor_row >= self.screen.scroll_region_top
                && self.screen.cursor_row <= self.screen.scroll_region_bottom
            {
                self.screen.scroll_region_bottom
            } else {
                self.screen.grid.rows().saturating_sub(1)
            };
            self.screen.cursor_row = self
                .screen
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

    /// Dispatch a complete OSC payload (the bytes between `ESC ]` and the
    /// BEL/ST terminator). Shared by the single-batch parser and the streaming
    /// resume path so fragmented OSCs behave exactly like unfragmented ones.
    fn handle_osc_payload(&mut self, payload: &[u8], bel_terminated: bool) {
        self.osc_reply_terminator = Self::osc_reply_terminator_for(bel_terminated);
        if let Ok(payload) = std::str::from_utf8(payload) {
            // OSC 104/110/111/112 are valid without a
            // `;value` part — treat those as empty.
            if let Some((command, value)) = payload.split_once(';').or(Some((payload, ""))) {
                if command == "0" || command == "1" || command == "2" {
                    // Bounded at ingest. The payload arrives from the pending
                    // escape buffer, which tolerates megabytes before it gives
                    // up, and the title is read back once per frame; an
                    // unbounded copy parked in terminal state is charged twice.
                    // The window-manager sanitiser downstream still caps the
                    // *displayed* title, but it can only cap what it is handed.
                    //
                    // Per xterm: 0 sets both titles, 1 the icon title only,
                    // 2 the window title only. The icon title is separate
                    // because XTWINOPS reports and saves the two independently.
                    if command != "1" {
                        self.window_title = Self::sanitized_title(value);
                    }
                    if command != "2" {
                        self.icon_title = Self::sanitized_title(value);
                    }
                } else if command == "7" {
                    // OSC 7 — current working directory.
                    // Format: file://hostname/path (path is %-encoded).
                    // We accept either the full URL or a bare path.
                    self.current_working_dir = self.decode_osc7_cwd(value);
                    Self::observe_organism_directory(
                        &mut self.organism_working_dir,
                        if self.screen.use_alt_buffer {
                            None
                        } else {
                            self.current_working_dir.as_deref()
                        },
                    );
                    crate::debug_log!("[OSC7] cwd set to {:?}", self.current_working_dir);
                } else if command == "8" {
                    // OSC 8 - Hyperlinks
                    // Format: ESC ] 8 ; params ; URI ST
                    // Empty URI closes the current hyperlink. Invalid,
                    // oversized, or unsafe openings also clear it so a
                    // rejected sequence cannot accidentally extend the
                    // preceding safe link over later text.
                    if let Some((params, uri)) = value.split_once(';') {
                        if uri.is_empty() {
                            self.screen.current_hyperlink = HyperlinkId::NONE;
                        } else {
                            self.screen.current_hyperlink = self
                                .hyperlinks
                                .intern(params, uri)
                                .unwrap_or(HyperlinkId::NONE);
                        }
                    } else {
                        self.screen.current_hyperlink = HyperlinkId::NONE;
                    }
                } else if command == "4" {
                    self.handle_osc_palette(value);
                } else if command == "104" {
                    self.reset_osc_palette(value);
                } else if command == "10" || command == "11" || command == "12" {
                    self.handle_osc_color(command, value);
                } else if command == "110" || command == "111" || command == "112" {
                    self.reset_osc_color(command);
                } else if command == "9" {
                    // Desktop notification (iTerm2/ConEmu)
                    if self.pending_notifications.len() < 8 {
                        let title = self.host_context.notification_app_name.clone();
                        let body = Self::safe_notification_field(value);
                        if !Self::notification_field_is_rewritten(&body) {
                            self.pending_notifications.push((title, body));
                        }
                    }
                } else if command == "777" {
                    // rxvt notification: 777;notify;title;body
                    let parts: Vec<&str> = value.splitn(3, ';').collect();
                    if parts.len() >= 2 && parts[0] == "notify" {
                        let title = Self::safe_notification_field(parts.get(1).unwrap_or(&""));
                        let title = if title.is_empty() {
                            self.host_context.notification_app_name.clone()
                        } else {
                            title
                        };
                        let body = Self::safe_notification_field(parts.get(2).unwrap_or(&""));
                        if self.pending_notifications.len() < 8
                            && !Self::notification_field_is_rewritten(&title)
                            && !Self::notification_field_is_rewritten(&body)
                        {
                            self.pending_notifications.push((title, body));
                        }
                    }
                } else if command == "7770" {
                    // OSC 7770 ; <session-id> — jsh announces the session that
                    // owns this pane's execution journal. Without it a pane can
                    // only know a session id it passed in itself as
                    // `--session`, so a jsh the user started by hand inside
                    // some other shell writes a journal the Commands sidebar
                    // then reads under the wrong key and shows as empty.
                    //
                    // Not trimmed, not repaired: jsh's grammar is an exact
                    // 1-128 byte ASCII token, and salvaging an invalid payload
                    // into a valid one would name a *different* session's
                    // journal than the shell meant.
                    if jterm_core::execution_journal::is_valid_jsh_session_id(value) {
                        self.announced_jsh_session_id = Some(value.to_owned());
                    }
                } else if command == "52" {
                    self.handle_osc_52(value);
                } else if command == "133" {
                    // OSC 133 (FinalTerm) shell integration:
                    //   A           prompt start
                    //   B           prompt end / command line begins
                    //   C           command output begins
                    //   D[;<exit>]  command finished, optional exit code
                    // Parameters are parsed centrally so C can carry
                    // Kitty's `cmdline_url` and jsh can correlate all
                    // phases with `jsh_id`/`id`.
                    self.handle_osc_133(value);
                } else if command == "5522" {
                    let (metadata, osc_payload) =
                        if let Some((metadata, osc_payload)) = value.split_once(';') {
                            (metadata, Some(osc_payload))
                        } else {
                            (value, None)
                        };
                    self.handle_osc_5522(metadata, osc_payload);
                }
            }
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
                .saturating_add(self.screen.cursor_row as u64),
            column: 0,
        };
        self.note_output_hard_line_advance(departure.row_id, departed_boundary, next_boundary);
    }

    /// Two-byte-and-longer ESC sequences without a dedicated arm in
    /// `process_input`. Only DECALN has an effect; the rest (SS2/SS3, stray ST,
    /// DOCS `ESC % G`, DECDHL/DECSWL `ESC # 3..6`, G2/G3 designation, ...) are
    /// recognised so their bytes are swallowed instead of printed.
    fn handle_esc_dispatch(&mut self, intermediates: &[u8], final_byte: u8) {
        if intermediates == b"#" && final_byte == b'8' {
            self.screen_alignment_test();
        }
    }

    /// DECALN (`ESC # 8`): fill the screen with 'E', reset the margins and home
    /// the cursor, as xterm does.
    fn screen_alignment_test(&mut self) {
        let cols = self.screen.grid.row_len();
        let rows = self.screen.grid.rows();
        let spans: Vec<_> = (0..rows).map(|row| (row, 0, cols)).collect();
        self.invalidate_grid_mutation_spans(&spans);
        self.screen.screen_alignment_test();
        self.note_output_cursor_reposition(true);
        self.dirty_region.mark_all(rows);
        self.mark_rows_dirty(0, rows.saturating_sub(1));
    }

    fn apply_local_semantic_effects(&mut self, effects: jterm_engine::semantics::Effects) {
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
        let mut at = 0;
        loop {
            let step = self.parser.next(&input[at..]);
            at += step.consumed;
            let Some(event) = step.event else {
                break;
            };
            if let jterm_engine::semantics::Dispatch::Handled(effects) =
                jterm_engine::semantics::dispatch_local(
                    &mut self.screen,
                    &mut self.protocol,
                    &event,
                )
            {
                self.apply_local_semantic_effects(effects);
                self.observe_chat_native_takeover();
                continue;
            }
            // The parser borrow ends before semantic dispatch. RIS may replace
            // this entire terminal, including its parser, without restoring stale carry.
            match event {
                Event::Print(ch) => self.put_char(ch),
                Event::Ascii(bytes) => {
                    if self.screen.active_charset == Charset::Ascii {
                        self.put_ascii_run(bytes);
                    } else {
                        for &byte in bytes {
                            self.put_char(byte as char);
                            self.observe_chat_native_takeover();
                        }
                    }
                }
                Event::Control(10..=12) => self.line_feed(),
                // Other controls and charset designation are consumed by the
                // local engine dispatcher before reaching this fallback.
                Event::Control(_) | Event::Charset { .. } => {}
                Event::Escape {
                    intermediates,
                    final_byte,
                } => {
                    if intermediates.is_empty() {
                        match final_byte {
                            b'c' => self.hard_reset(),
                            b'M' => {
                                self.apply_vertical_motion(VerticalMotion::ReverseIndex);
                            }
                            b'D' => self.line_feed(),
                            b'E' => {
                                self.screen.begin_next_line();
                                self.note_output_cursor_reposition(false);
                                self.line_feed();
                            }
                            _ => self.handle_esc_dispatch(&intermediates, final_byte),
                        }
                    } else {
                        self.handle_esc_dispatch(&intermediates, final_byte);
                    }
                }
                Event::Csi {
                    params,
                    private_prefix,
                    intermediates,
                    final_byte,
                    ..
                } => self.handle_escape_sequence(
                    &params,
                    final_byte as char,
                    private_prefix,
                    &intermediates,
                ),
                Event::String {
                    kind,
                    payload,
                    bel_terminated,
                } => match kind {
                    StringKind::Osc => self.handle_osc_payload(&payload, bel_terminated),
                    StringKind::Dcs => {} // Non-DECRQSS DCS remains opaque.
                    StringKind::Apc => self.handle_kitty_apc_payload(&payload),
                    StringKind::Sos | StringKind::Pm => {}
                },
                Event::Boundary => {}
                Event::RejectApc { recovery, reason } => {
                    let message = match reason {
                        RejectReason::Cancelled => "Kitty graphics APC was cancelled",
                        RejectReason::TooLarge => {
                            "Kitty graphics APC exceeded the parser size limit"
                        }
                    };
                    self.kitty_graphics
                        .reject_graphics_payload(&recovery, message);
                    self.drain_kitty_graphics_responses();
                }
            }
            self.observe_chat_native_takeover();
        }
    }

    pub(super) fn handle_escape_sequence(
        &mut self,
        params: &[u16],
        cmd: char,
        private_prefix: Option<u8>,
        intermediates: &[u8],
    ) {
        // xterm semantics: an explicit 0 count means 1 for every movement,
        // insert/delete, erase-character and scroll control below.
        let count = params.first().copied().unwrap_or(1).max(1) as usize;
        match cmd {
            'J' if intermediates.is_empty() && matches!(private_prefix, None | Some(b'?')) => {
                let selective = private_prefix == Some(b'?');
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        if !selective && self.screen.cursor_row == 0 && self.screen.cursor_col == 0
                        {
                            self.inline_cli_screen_erased();
                        }
                        // Clear from cursor to end of display
                        // ClearRight cancels wrap even if the tail is protected.
                        self.screen.pending_wrap = false;
                        let cols = self.screen.grid.row_len();
                        self.erase_grid_span(
                            self.screen.cursor_row,
                            self.screen.cursor_col,
                            cols,
                            selective,
                        );
                        for row in (self.screen.cursor_row + 1)..self.screen.grid.rows() {
                            self.erase_grid_span(row, 0, cols, selective);
                        }
                    }
                    1 => {
                        // Clear from start to cursor
                        let cols = self.screen.grid.row_len();
                        for row in 0..=self.screen.cursor_row {
                            let end_col = if row == self.screen.cursor_row {
                                self.screen.cursor_col + 1
                            } else {
                                cols
                            };
                            self.erase_grid_span(row, 0, end_col, selective);
                        }
                    }
                    2 if selective => {
                        // DECSED preserves protected text and does not archive
                        // a partially retained screen or delete Kitty images.
                        for row in 0..self.screen.grid.rows() {
                            self.erase_grid_span(row, 0, self.screen.grid.row_len(), true);
                        }
                    }
                    2 => {
                        // ED 擦除显示不移动光标(VT 规范)
                        if self.sync_output_active {
                            if self.screen.use_alt_buffer {
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
                    3 if self.screen.use_alt_buffer && self.inline_cli_owner_active() => {
                        // A temporary CLI screen cannot erase its primary
                        // conversation's saved lines.
                    }
                    3 => {
                        // Clear scrollback buffer (xterm extension)
                        self.clear_scrollback_with_projection_provenance();
                        self.invalidate_scrollback_view_cache();
                        self.kitty_graphics.clear_scrollback_placements();
                        self.scroll_offset = 0;
                    }
                    _ => {}
                }
            }
            'L' => {
                self.edit_grid_lines(RegionDirection::Down, count);
                self.mark_rows_dirty(self.screen.cursor_row, self.screen.scroll_region_bottom);
            }
            'M' => {
                self.edit_grid_lines(RegionDirection::Up, count);
                self.mark_rows_dirty(self.screen.cursor_row, self.screen.scroll_region_bottom);
            }
            'S' => {
                // SU — Scroll Up. Cap to region height: more would just blank
                // an already-blank region while doing O(rows) work each step.
                let n = count;
                let region_height = self
                    .screen
                    .scroll_region_bottom
                    .saturating_sub(self.screen.scroll_region_top)
                    + 1;
                let n = n.min(region_height);
                for _ in 0..n {
                    self.scroll_region_up(
                        self.screen.scroll_region_top,
                        self.screen.scroll_region_bottom,
                    );
                }
            }
            'T' => {
                // SD — Scroll Down. Same cap as SU.
                let n = count;
                let region_height = self
                    .screen
                    .scroll_region_bottom
                    .saturating_sub(self.screen.scroll_region_top)
                    + 1;
                let n = n.min(region_height);
                for _ in 0..n {
                    self.scroll_region_down(
                        self.screen.scroll_region_top,
                        self.screen.scroll_region_bottom,
                    );
                }
            }
            't' if private_prefix.is_none() && intermediates.is_empty() => {
                // XTWINOPS - window manipulation. Only the reporting and
                // title-stack ops; the mutating ones (resize/move/iconify) are
                // deliberately ignored, as frost ignores them.
                self.handle_window_ops(params);
            }
            'n' if private_prefix == Some(b'?') && params.first() == Some(&996) => {
                let response = format!("\x1b[?997;{}n", self.color_scheme_report());
                self.output_buffer.extend(response.as_bytes());
            }
            'c' => {
                if intermediates.is_empty() {
                    match private_prefix {
                        None => {
                            crate::debug_log!("[DA] primary device attributes request");
                            self.output_buffer
                                .extend_from_slice(PRIMARY_DEVICE_ATTRIBUTES_RESPONSE);
                        }
                        Some(b'>') => {
                            crate::debug_log!("[DA] secondary device attributes request");
                            self.output_buffer
                                .extend_from_slice(SECONDARY_DEVICE_ATTRIBUTES_RESPONSE);
                        }
                        _ => {}
                    }
                }
            }
            'h' => {
                // 区分 DEC 私有模式(CSI ? Pn h)与 ANSI 模式(CSI Pn h)。
                // 否则 CSI 7h 会被误当作 DECAWM(?7),CSI 4h(IRM)也会落空。
                match private_prefix {
                    Some(b'?') => {
                        for &mode in params {
                            self.set_mode(mode);
                        }
                    }
                    None => {
                        for &mode in params {
                            self.set_ansi_mode(mode);
                        }
                    }
                    _ => {}
                }
            }
            'l' => match private_prefix {
                Some(b'?') => {
                    for &mode in params {
                        self.reset_mode(mode);
                    }
                }
                None => {
                    for &mode in params {
                        self.reset_ansi_mode(mode);
                    }
                }
                _ => {}
            },
            'q' => {
                if private_prefix == Some(b'>')
                    && intermediates.is_empty()
                    && params.first().copied().unwrap_or(0) == 0
                {
                    crate::debug_log!("[XTVERSION] report terminal version request");
                    self.output_buffer.extend_from_slice(XTERM_VERSION_RESPONSE);
                }
            }
            'b' => {
                // REP - repeat the last graphic character n times. terminfo's
                // rep=%p1%c\E[%p2%{1}%-%db writes the glyph once and asks for
                // the remaining n-1 here, so dropping this arm renders a run of
                // identical cells — a rule, a bar, a box border — as one glyph
                // followed by blanks.
                let n = count;
                if let Some(ch) = self.screen.last_printed_char {
                    for _ in 0..n {
                        self.put_char(ch);
                    }
                }
            }
            _ => {}
        }
        self.observe_chat_native_takeover();
    }

    /// 擦除整个屏幕单元格(保留当前背景色),不移动光标。
    /// 供 ED(`CSI 2J`)使用 —— 按 VT 规范擦除显示不得移动光标。
    pub(super) fn erase_screen(&mut self) {
        self.screen.pending_wrap = false;
        let cols = self.screen.grid.row_len();
        let spans: Vec<_> = (0..self.screen.grid.rows())
            .map(|row| (row, 0, cols))
            .collect();
        self.invalidate_grid_mutation_spans(&spans);
        self.screen.erase_screen();
        // Mark all rows as dirty
        self.dirty_region.mark_all(self.screen.grid.rows());
        self.mark_rows_dirty(0, self.screen.grid.rows().saturating_sub(1));
        self.kitty_graphics.clear_placements();
    }

    /// Host policy around the shared engine's physical alternate transition.
    pub(super) fn enter_alt_screen(&mut self, save_cursor: bool, clear: bool) {
        if self.screen.use_alt_buffer {
            return;
        }
        self.selection = None;
        self.scroll_offset = 0;
        let effect = self
            .screen
            .enter_alternate_screen(save_cursor, clear, &self.protocol)
            .expect("inactive alternate screen checked above");
        self.mark_row_identity_changed();
        self.kitty_graphics.switch_screen();
        self.protocol.switch_screen();
        self.watch.boundary();
        self.invalidate_search_scrollback_seals();
        self.organism_working_dir = None;
        if let Some(rows) = effect.cleared_rows {
            // Clearing is a BCE fill, not a row replacement. Consume the host
            // effects after switching graphics/keyboard/watch ownership, as
            // before extraction. Alternate cells have no primary provenance.
            let cols = self.screen.grid.row_len();
            let spans: Vec<_> = rows.clone().map(|row| (row, 0, cols)).collect();
            self.invalidate_grid_mutation_spans(&spans);
            self.dirty_region.mark_all(self.screen.grid.rows());
            self.mark_rows_dirty(rows.start, rows.end.saturating_sub(1));
            self.kitty_graphics.clear_placements();
            self.kitty_graphics.clear_current_screen_placements();
        }
    }

    pub(super) fn exit_alt_screen(&mut self, restore_cursor: bool) {
        if !self.screen.use_alt_buffer {
            return;
        }
        self.selection = None;
        let effect = self
            .screen
            .leave_alternate_screen(restore_cursor)
            .expect("active alternate screen checked above");
        self.mark_row_identity_changed();
        self.kitty_graphics.switch_screen();
        self.protocol.switch_screen();
        self.watch.boundary();
        self.invalidate_search_scrollback_seals();
        self.global_bg = effect.global_background;
        if let Some(autowrap) = effect.restore_autowrap {
            if autowrap {
                self.protocol.modes_mut().insert(7);
            } else {
                self.protocol.modes_mut().remove(&7);
            }
        }
        self.grid_version += effect.dirty_rows.len() as u64 + 1;
        self.row_versions.fill(self.grid_version);
        self.dirty_region.mark_all(self.screen.grid.rows());
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

    /// A title on its way back out to the PTY, with anything that could break
    /// the reply's own framing or reorder it removed. The ingest path caps
    /// length; this caps what the bytes can *do*.
    fn reportable_title(title: &str) -> String {
        title
            .chars()
            .filter(|&ch| {
                !ch.is_control()
                    && !matches!(
                        ch,
                        '\u{061c}'
                            | '\u{200e}'
                            | '\u{200f}'
                            | '\u{202a}'..='\u{202e}'
                            | '\u{2066}'..='\u{2069}'
                    )
            })
            .take(MAX_WINDOW_TITLE_CHARS)
            .collect()
    }

    /// XTWINOPS (`CSI Ps ; … t`), reporting ops and the title stack only.
    ///
    /// Dropping these entirely left two holes: a program probing cell geometry
    /// with `CSI 18 t` — which is how several TUIs and image protocols size
    /// themselves when they cannot ioctl the pty — got no reply, and the
    /// `CSI 22 t` / `CSI 23 t` push-pop that shells and tmux use around a
    /// command was a no-op, so the title stayed on whatever the last command
    /// set it to.
    fn handle_window_ops(&mut self, params: &[u16]) {
        let op = params.first().copied().unwrap_or(0);
        let rows = self.screen.grid.rows();
        let cols = self.screen.grid.row_len();
        match op {
            // Report window state: normal/non-iconified.
            11 => self.output_buffer.extend_from_slice(b"\x1b[1t"),
            // Report window position. We do not track compositor coordinates,
            // so report a stable origin, as frost does.
            13 => self.output_buffer.extend_from_slice(b"\x1b[3;0;0t"),
            // Report text area size in pixels, derived from the cell size the
            // renderer keeps on the graphics state.
            14 => {
                let (cell_w, cell_h) = self.kitty_graphics.cell_size_pixels();
                let response = format!(
                    "\x1b[4;{};{}t",
                    cell_h.saturating_mul(rows as u32),
                    cell_w.saturating_mul(cols as u32)
                );
                self.output_buffer.extend(response.as_bytes());
            }
            // Report character cell size in pixels, from the same metrics.
            16 => {
                let (cell_w, cell_h) = self.kitty_graphics.cell_size_pixels();
                let response = format!("\x1b[6;{cell_h};{cell_w}t");
                self.output_buffer.extend(response.as_bytes());
            }
            // Report text area size in characters.
            18 => {
                let response = format!("\x1b[8;{rows};{cols}t");
                self.output_buffer.extend(response.as_bytes());
            }
            // Report screen size in characters. Monitor geometry is not known
            // here, so mirror the grid rather than invent numbers.
            19 => {
                let response = format!("\x1b[9;{rows};{cols}t");
                self.output_buffer.extend(response.as_bytes());
            }
            // Report icon label / window title.
            20 => {
                let response = format!("\x1b]L{}\x1b\\", Self::reportable_title(&self.icon_title));
                self.output_buffer.extend(response.as_bytes());
            }
            21 => {
                let response =
                    format!("\x1b]l{}\x1b\\", Self::reportable_title(&self.window_title));
                self.output_buffer.extend(response.as_bytes());
            }
            // Save/restore titles. 1 = icon, 2 = window, anything else = both.
            22 => self.save_titles(params.get(1).copied().unwrap_or(0)),
            23 => self.restore_titles(params.get(1).copied().unwrap_or(0)),
            _ => {}
        }
    }

    fn save_titles(&mut self, target: u16) {
        if self.title_stack.len() >= MAX_TITLE_STACK_DEPTH {
            self.title_stack.remove(0);
        }
        match target {
            1 => self.title_stack.push((Some(self.icon_title.clone()), None)),
            2 => self
                .title_stack
                .push((None, Some(self.window_title.clone()))),
            _ => self.title_stack.push((
                Some(self.icon_title.clone()),
                Some(self.window_title.clone()),
            )),
        }
    }

    fn restore_titles(&mut self, target: u16) {
        let Some((icon_title, window_title)) = self.title_stack.pop() else {
            return;
        };
        if target != 2 {
            if let Some(icon_title) = icon_title {
                self.icon_title = icon_title;
            }
        }
        if target != 1 {
            if let Some(window_title) = window_title {
                self.window_title = window_title;
            }
        }
    }

    pub(super) fn set_mode(&mut self, mode: u16) {
        if apply_ember_mode(
            &mut self.screen,
            &mut self.protocol,
            ModeNamespace::DecPrivate,
            mode,
            true,
        ) {
            return;
        }
        match mode {
            1004 => {
                // Focus event reporting (CSI I / CSI O). If the window is
                // already focused, report that immediately so a client that
                // probes then waits for the first I is not stuck until Alt-Tab.
                let newly = !self.protocol.modes().contains(&1004);
                self.protocol.modes_mut().insert(1004);
                if newly && self.host_window_focused {
                    self.output_buffer.extend_from_slice(b"\x1b[I");
                }
            }
            47 => {
                // 备用屏(无保存光标、无清屏)
                self.enter_alt_screen(false, false);
                self.protocol.modes_mut().insert(47);
            }
            1047 => {
                // 备用屏(切入时清屏)
                self.enter_alt_screen(false, true);
                self.protocol.modes_mut().insert(1047);
            }
            1048 => {
                // 仅保存光标(等价 DECSC). It shares DECSC's slot, as in xterm,
                // not the one 1049 keeps for the primary screen, so a 1048
                // inside a 1049 session cannot move the cursor 1049l restores.
                self.save_cursor_state();
                self.protocol.modes_mut().insert(1048);
            }
            1049 => {
                // 备用屏:保存光标 + 切入 + 清屏
                self.enter_alt_screen(true, true);
                self.protocol.modes_mut().insert(1049);
            }
            2026 => {
                // Synchronized output: suppress rendering until cleared
                self.protocol.modes_mut().insert(2026);
                self.sync_output_active = true;
                self.sync_output_start = Some(self.sync_output_clock.now());
            }
            5522 => {
                self.protocol.modes_mut().insert(5522);
            }
            _ => unreachable!("engine returned an unknown host mode"),
        }
    }
    /// ANSI modes have no host lifecycle hooks.
    pub(super) fn set_ansi_mode(&mut self, mode: u16) {
        apply_ember_mode(
            &mut self.screen,
            &mut self.protocol,
            ModeNamespace::Ansi,
            mode,
            true,
        );
    }

    pub(super) fn reset_ansi_mode(&mut self, mode: u16) {
        apply_ember_mode(
            &mut self.screen,
            &mut self.protocol,
            ModeNamespace::Ansi,
            mode,
            false,
        );
    }

    pub(super) fn reset_mode(&mut self, mode: u16) {
        if apply_ember_mode(
            &mut self.screen,
            &mut self.protocol,
            ModeNamespace::DecPrivate,
            mode,
            false,
        ) {
            return;
        }
        match mode {
            1004 => {
                // Disable focus event reporting
                self.protocol.modes_mut().remove(&1004);
            }
            5522 => {
                // A paste capability is valid only while the application keeps
                // paste-event mode enabled. Revocation must be immediate.
                self.protocol.modes_mut().remove(&5522);
                self.pending_paste_grant = None;
            }
            47 => {
                // 退出备用屏(不恢复光标)
                self.exit_alt_screen(false);
                self.protocol.modes_mut().remove(&47);
            }
            1047 => {
                // 退出备用屏(不恢复光标)
                self.exit_alt_screen(false);
                self.protocol.modes_mut().remove(&1047);
            }
            1048 => {
                // 仅恢复光标(等价 DECRC)
                self.restore_cursor_state();
                self.protocol.modes_mut().remove(&1048);
            }
            1049 => {
                // 退出备用屏并恢复进入前保存的光标
                self.exit_alt_screen(true);
                self.protocol.modes_mut().remove(&1049);
            }
            2026 => {
                // End synchronized output: force full render
                if self.screen.use_alt_buffer {
                    self.archive_visible_screen_to_scrollback_with_options(true, true);
                } else {
                    self.last_synced_primary_screen_snapshot =
                        self.visible_screen_snapshot().unwrap_or_default();
                }
                self.protocol.modes_mut().remove(&2026);
                self.sync_output_active = false;
                self.sync_output_start = None;
                self.dirty_region.mark_all(self.screen.grid.rows());
                self.mark_rows_dirty(0, self.screen.grid.rows().saturating_sub(1));
            }
            _ => unreachable!("engine returned an unknown host mode"),
        }
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
