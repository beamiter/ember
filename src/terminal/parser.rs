use super::*;

impl super::TerminalState {
    /// Carry an unfinished escape across PTY read batches, but cap total size
    /// to avoid unbounded growth on malformed/binary streams that never send a
    /// terminator. On overflow the buffer is dropped (parser drops back to a
    /// clean state) — the alternative would be to keep partial data that we
    /// already know is too big to ever match.
    fn stash_pending_escape(&mut self, tail: &[u8]) {
        let new_len = self.pending_escape.len().saturating_add(tail.len());
        if new_len > MAX_PENDING_ESCAPE {
            crate::debug_log!(
                "[PARSER] pending_escape exceeded {} bytes (have {}, +{}); discarding",
                MAX_PENDING_ESCAPE,
                self.pending_escape.len(),
                tail.len()
            );
            self.pending_escape.clear();
            return;
        }
        self.pending_escape.extend_from_slice(tail);
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
        let cursor_col = u32::try_from(self.cursor_col).unwrap_or(u32::MAX);
        let cursor_row = u32::try_from(self.cursor_row).unwrap_or(u32::MAX);
        let parsed = self
            .kitty_graphics
            .parse_graphics_payload_at(payload, cursor_col, cursor_row);
        if let Err(_error) = parsed {
            crate::debug_log!("[APC] Kitty graphics error: {}", _error);
        } else if let Some((columns, rows)) = self.kitty_graphics.take_cursor_movement() {
            self.pending_wrap = false;
            let last_col = self.grid.row_len().saturating_sub(1);
            self.cursor_col = self
                .cursor_col
                .saturating_add(columns as usize)
                .min(last_col);
            let last_row = if self.cursor_row >= self.scroll_region_top
                && self.cursor_row <= self.scroll_region_bottom
            {
                self.scroll_region_bottom
            } else {
                self.grid.rows().saturating_sub(1)
            };
            self.cursor_row = self.cursor_row.saturating_add(rows as usize).min(last_row);
        }
        self.drain_kitty_graphics_responses();
    }

    fn drain_kitty_graphics_responses(&mut self) {
        let responses = self.kitty_graphics.take_responses();
        if !responses.is_empty() {
            self.output_buffer.extend_from_slice(&responses);
        }
    }

    fn reject_buffered_kitty_apc_with_suffix(&mut self, suffix: &[u8], error: &str) {
        let Some(payload) = self.pending_apc.strip_prefix(b"\x1b_") else {
            return;
        };
        if payload.first() != Some(&b'G') {
            return;
        }

        // Usually i/I/q are already buffered. If fragmentation happened
        // unusually early, copy only a bounded control prefix from the new
        // fragment so an oversized rejection can still echo the identifier and
        // honor q without retaining the oversized packet.
        let limit = crate::kitty_graphics::MAX_KITTY_CONTROL_BYTES;
        let mut recovery =
            Vec::with_capacity(limit.min(payload.len().saturating_add(suffix.len())));
        recovery.extend_from_slice(&payload[..payload.len().min(limit)]);
        if recovery.len() < limit && !recovery.contains(&b';') {
            recovery.extend_from_slice(&suffix[..suffix.len().min(limit - recovery.len())]);
        }
        self.kitty_graphics
            .reject_graphics_payload(&recovery, error);
        self.drain_kitty_graphics_responses();
    }

    fn begin_pending_apc(&mut self, tail: &[u8]) {
        if tail.len() > MAX_PENDING_ESCAPE {
            if let Some(payload) = tail.strip_prefix(b"\x1b_") {
                if payload.first() == Some(&b'G') {
                    self.kitty_graphics.reject_graphics_payload(
                        payload,
                        "Kitty graphics APC exceeded the parser size limit",
                    );
                    self.drain_kitty_graphics_responses();
                }
            }
            self.pending_apc.clear();
            self.discarding_oversized_apc = true;
            self.discarding_apc_prev_escape = tail.last() == Some(&0x1b);
            return;
        }
        self.pending_apc.clear();
        self.pending_apc.extend_from_slice(tail);
        self.pending_apc_scan_from = self.pending_apc.len().saturating_sub(1);
    }

    /// Resume a fragmented Kitty APC. Returns true when this function consumed
    /// the input (including any recursively processed bytes after the ST).
    fn resume_pending_apc(&mut self, input: &[u8]) -> bool {
        if self.discarding_oversized_apc {
            let mut previous_escape = self.discarding_apc_prev_escape;
            for (index, byte) in input.iter().copied().enumerate() {
                if previous_escape && byte == b'\\' {
                    self.discarding_oversized_apc = false;
                    self.discarding_apc_prev_escape = false;
                    if index + 1 < input.len() {
                        self.process_input(&input[index + 1..]);
                    }
                    return true;
                }
                previous_escape = byte == 0x1b;
            }
            self.discarding_apc_prev_escape = previous_escape;
            return true;
        }

        if self.pending_apc.is_empty() {
            return false;
        }

        // Everything before pending_apc_scan_from was proved not to contain
        // ST in the previous call. A new terminator can therefore only straddle
        // the old/new boundary or live entirely in input. Search the new bytes
        // once before doing the capacity check: bytes after ST are normal
        // terminal input and must not be charged to the APC size limit.
        let scan_from = self
            .pending_apc_scan_from
            .min(self.pending_apc.len().saturating_sub(1));
        let terminator = if scan_from + 1 == self.pending_apc.len()
            && self.pending_apc[scan_from] == 0x1b
            && input.first() == Some(&b'\\')
        {
            Some((scan_from, 1))
        } else {
            input
                .windows(2)
                .position(|window| window == b"\x1b\\")
                .map(|offset| (self.pending_apc.len() + offset, offset + 2))
        };

        if let Some((terminator, consumed)) = terminator {
            let packet_len = self.pending_apc.len().saturating_add(consumed);
            if packet_len > MAX_PENDING_ESCAPE {
                self.reject_buffered_kitty_apc_with_suffix(
                    &input[..consumed],
                    "Kitty graphics APC exceeded the parser size limit",
                );
                self.pending_apc.clear();
                self.pending_apc_scan_from = 0;
            } else {
                self.pending_apc.extend_from_slice(&input[..consumed]);
                let packet = std::mem::take(&mut self.pending_apc);
                self.pending_apc_scan_from = 0;
                if packet.starts_with(b"\x1b_") {
                    self.handle_kitty_apc_payload(&packet[2..terminator]);
                }
            }
            if consumed < input.len() {
                self.process_input(&input[consumed..]);
            }
            return true;
        }

        if self.pending_apc.len().saturating_add(input.len()) > MAX_PENDING_ESCAPE {
            let previous_escape = input
                .last()
                .copied()
                .or_else(|| self.pending_apc.last().copied())
                == Some(0x1b);
            self.reject_buffered_kitty_apc_with_suffix(
                input,
                "Kitty graphics APC exceeded the parser size limit",
            );
            self.pending_apc.clear();
            self.pending_apc_scan_from = 0;
            self.discarding_oversized_apc = true;
            self.discarding_apc_prev_escape = previous_escape;
            return true;
        }

        self.pending_apc.extend_from_slice(input);
        self.pending_apc_scan_from = self.pending_apc.len().saturating_sub(1);
        true
    }

    pub fn process_input(&mut self, input: &[u8]) {
        if self.resume_pending_apc(input) {
            return;
        }
        // Fast path: if no pending escape, process input directly without allocation
        let data;
        let data_slice: &[u8] = if self.pending_escape.is_empty() {
            input
        } else {
            // Slow path: merge pending escape with new input
            let mut combined = std::mem::take(&mut self.pending_escape);
            combined.extend_from_slice(input);
            data = combined;
            &data
        };

        let mut i = 0;

        while i < data_slice.len() {
            let byte = data_slice[i];

            // 存在未完成的多字节 UTF-8 序列,而当前字节不是续接字节(10xxxxxx):
            // 说明前一个序列残缺,按 Unicode 建议输出替换字符 U+FFFD 并复位,
            // 然后正常处理当前字节,避免残缺序列被静默吞掉或污染后续解析。
            if self.utf8_len > 0 && (byte & 0xC0) != 0x80 {
                self.put_char('\u{FFFD}');
                self.utf8_len = 0;
            }

            match byte {
                b'\x08' => {
                    // Backspace (0x08) - move cursor left.
                    // Shell handles actual deletion and sends back updated display.
                    self.pending_wrap = false;
                    if self.cursor_col > 0 {
                        self.cursor_col -= 1;
                    }
                    i += 1;
                }
                b'\x7f' => {
                    // DEL (0x7f) 在输出流中按 ECMA-48 应被忽略,不能当作退格移动光标。
                    i += 1;
                }
                b'\n' => {
                    // Linefeed - move cursor down or scroll
                    self.pending_wrap = false;
                    if self.cursor_row == self.scroll_region_bottom {
                        // 恰在滚动区底边距:向上滚动区域,光标保持在底行
                        self.scroll_region_up(self.scroll_region_top, self.scroll_region_bottom);
                    } else if self.cursor_row + 1 < self.grid.rows() {
                        // 区内或区外(底边距下方)正常下移,不滚动
                        self.cursor_row += 1;
                    }
                    i += 1;
                }
                b'\r' => {
                    self.pending_wrap = false;
                    self.cursor_col = 0;
                    i += 1;
                }
                b'\x0e' => {
                    self.active_charset = self.g1_charset;
                    i += 1;
                }
                b'\x0f' => {
                    self.active_charset = self.g0_charset;
                    i += 1;
                }
                b'\x07' => {
                    // Bell - ignore
                    i += 1;
                }
                b'\t' => {
                    // Tab - 前进到下一个制表位(支持自定义 HTS/TBC 制表位)
                    self.pending_wrap = false;
                    self.cursor_col = self.next_tab_stop(self.cursor_col);
                    i += 1;
                }
                b'\x1b' => {
                    let esc_start = i;

                    if i + 1 >= data_slice.len() {
                        self.stash_pending_escape(&data_slice[esc_start..]);
                        break;
                    }

                    match data_slice[i + 1] {
                        b'c' => {
                            // RIS — full terminal reset. Reinitialize graphics
                            // state as well; the final byte is control syntax,
                            // never printable text.
                            self.hard_reset();
                            i += 2;
                        }
                        b'7' => {
                            // DECSC - 保存光标(含 SGR/字符集/模式)
                            self.save_cursor_state();
                            i += 2;
                        }
                        b'8' => {
                            // DECRC - 恢复光标(含 SGR/字符集/模式)
                            self.restore_cursor_state();
                            i += 2;
                        }
                        b'H' => {
                            // HTS - 在当前光标列设置制表位
                            if let Some(stop) = self.tab_stops.get_mut(self.cursor_col) {
                                *stop = true;
                            }
                            i += 2;
                        }
                        b']' => {
                            i += 2;

                            let payload_start = i;

                            let mut terminated = false;
                            while i < data_slice.len() {
                                if data_slice[i] == 0x07 {
                                    i += 1;
                                    terminated = true;
                                    break;
                                } else if i + 1 < data_slice.len()
                                    && data_slice[i] == 0x1b
                                    && data_slice[i + 1] == 0x5c
                                {
                                    i += 2;
                                    terminated = true;
                                    break;
                                } else {
                                    i += 1;
                                }
                            }

                            if !terminated {
                                self.stash_pending_escape(&data_slice[esc_start..]);
                                break;
                            }

                            let payload_end = if data_slice[i - 1] == 0x07 {
                                i - 1
                            } else {
                                i - 2
                            };
                            if payload_end >= payload_start {
                                if let Ok(payload) =
                                    std::str::from_utf8(&data_slice[payload_start..payload_end])
                                {
                                    // OSC 104/110/111/112 are valid without a
                                    // `;value` part — treat those as empty.
                                    if let Some((command, value)) =
                                        payload.split_once(';').or(Some((payload, "")))
                                    {
                                        if command == "0" || command == "2" {
                                            self.window_title.clear();
                                            self.window_title.push_str(value);
                                        } else if command == "7" {
                                            // OSC 7 — current working directory.
                                            // Format: file://hostname/path (path is %-encoded).
                                            // We accept either the full URL or a bare path.
                                            self.current_working_dir = Self::decode_osc7_cwd(value);
                                            crate::debug_log!(
                                                "[OSC7] cwd set to {:?}",
                                                self.current_working_dir
                                            );
                                        } else if command == "8" {
                                            // OSC 8 - Hyperlinks
                                            // Format: ESC ] 8 ; params ; URI ST
                                            // params can include id=<identifier>
                                            // Empty URI = close hyperlink
                                            if let Some((params, uri)) = value.split_once(';') {
                                                if uri.is_empty() {
                                                    // Close hyperlink
                                                    self.current_hyperlink = None;
                                                } else {
                                                    // Open hyperlink
                                                    let id = params
                                                        .split(':')
                                                        .find_map(|p| p.strip_prefix("id="))
                                                        .map(|s| s.to_string());
                                                    self.current_hyperlink =
                                                        Some((uri.to_string(), id));
                                                }
                                            } else if value.is_empty() {
                                                // OSC 8 ; ; (close hyperlink)
                                                self.current_hyperlink = None;
                                            }
                                        } else if command == "4" {
                                            self.handle_osc_palette(value);
                                        } else if command == "104" {
                                            self.reset_osc_palette(value);
                                        } else if command == "10"
                                            || command == "11"
                                            || command == "12"
                                        {
                                            self.handle_osc_color(command, value);
                                        } else if command == "110"
                                            || command == "111"
                                            || command == "112"
                                        {
                                            self.reset_osc_color(command);
                                        } else if command == "9" {
                                            // Desktop notification (iTerm2/ConEmu)
                                            if self.pending_notifications.len() < 8 {
                                                let title = "jterm2".to_string();
                                                let body = value.chars().take(256).collect();
                                                self.pending_notifications.push((title, body));
                                            }
                                        } else if command == "777" {
                                            // rxvt notification: 777;notify;title;body
                                            let parts: Vec<&str> = value.splitn(3, ';').collect();
                                            if parts.len() >= 2 && parts[0] == "notify" {
                                                let title = parts
                                                    .get(1)
                                                    .unwrap_or(&"")
                                                    .chars()
                                                    .take(256)
                                                    .collect();
                                                let body = parts
                                                    .get(2)
                                                    .unwrap_or(&"")
                                                    .chars()
                                                    .take(256)
                                                    .collect();
                                                if self.pending_notifications.len() < 8 {
                                                    self.pending_notifications.push((title, body));
                                                }
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
                                            // Kitty's `cmdline_url` and rsh can correlate all
                                            // phases with `rsh_id`/`id`.
                                            self.handle_osc_133(value);
                                        } else if command == "5522" {
                                            let (metadata, osc_payload) =
                                                if let Some((metadata, osc_payload)) =
                                                    value.split_once(';')
                                                {
                                                    (metadata, Some(osc_payload))
                                                } else {
                                                    (value, None)
                                                };
                                            self.handle_osc_5522(metadata, osc_payload);
                                        }
                                    }
                                }
                            }
                        }
                        b'P' | b'X' | b'^' | b'_' => {
                            // ECMA-48 string introducers share the same ST
                            // terminator, but Kitty graphics is specifically an
                            // APC (`ESC _`) whose body starts with the literal
                            // protocol discriminator `G`. DCS/SOS/PM contents
                            // must remain opaque even when they happen to contain
                            // strings such as `a=`.
                            let is_apc = data_slice[i + 1] == b'_';
                            i += 2;

                            let mut terminated = false;
                            let string_start = i;
                            while i < data_slice.len() {
                                if i + 1 < data_slice.len()
                                    && data_slice[i] == 0x1b
                                    && data_slice[i + 1] == 0x5c
                                {
                                    let payload = &data_slice[string_start..i];

                                    if is_apc {
                                        self.handle_kitty_apc_payload(payload);
                                    }

                                    i += 2;
                                    terminated = true;
                                    break;
                                }
                                i += 1;
                            }

                            if !terminated {
                                if is_apc {
                                    self.begin_pending_apc(&data_slice[esc_start..]);
                                } else {
                                    self.stash_pending_escape(&data_slice[esc_start..]);
                                }
                                break;
                            }
                        }
                        b'>' => {
                            // ESC > - DECKPNM (Keypad Numeric Mode) or other private sequence
                            // Just skip it and any following bytes that are part of it
                            i += 2;
                        }
                        b'<' => {
                            // ESC < - DECKPM (Keypad Application Mode) or other private sequence
                            // Just skip it
                            i += 2;
                        }
                        b'=' => {
                            // ESC = - DECKPAM (Keypad Application Mode)
                            // Just skip it
                            i += 2;
                        }
                        b'(' | b')' => {
                            if i + 2 >= data_slice.len() {
                                self.stash_pending_escape(&data_slice[esc_start..]);
                                break;
                            }

                            // Character set selection: ESC ( X or ESC ) X
                            // data_slice[i] = ESC, data_slice[i+1] = '(' or ')', data_slice[i+2] = designator
                            let is_g0 = data_slice[i + 1] == b'(';
                            let designator = data_slice[i + 2];
                            let charset = Self::charset_from_designator(designator);

                            crate::debug_log!(
                                "[CHARSET] ESC {} designator={} (0x{:02x}) charset={:?}",
                                if is_g0 { '(' } else { ')' },
                                designator as char,
                                designator,
                                charset
                            );

                            if is_g0 {
                                self.g0_charset = charset;
                                self.active_charset = self.g0_charset;
                            } else {
                                self.g1_charset = charset;
                            }

                            i += 3;
                        }
                        b'M' => {
                            // RI - Reverse Index:仅在恰好位于上边距时反向滚动,
                            // 否则正常上移(在区域上方时不应滚动)。
                            i += 2;

                            if self.cursor_row == self.scroll_region_top {
                                if self.scroll_region_bottom < self.grid.rows()
                                    && self.scroll_region_top <= self.scroll_region_bottom
                                {
                                    self.scroll_region_down(
                                        self.scroll_region_top,
                                        self.scroll_region_bottom,
                                    );
                                }
                            } else {
                                self.cursor_row = self.cursor_row.saturating_sub(1);
                            }
                        }
                        b'D' => {
                            // IND - Index:仅在恰好位于底边距时向上滚动,
                            // 否则正常下移(在区域下方时不应滚动)。
                            i += 2;

                            if self.cursor_row == self.scroll_region_bottom {
                                self.scroll_region_up(
                                    self.scroll_region_top,
                                    self.scroll_region_bottom,
                                );
                            } else if self.cursor_row + 1 < self.grid.rows() {
                                self.cursor_row += 1;
                            }
                        }
                        b'[' => {
                            i += 2;

                            // Use stack arrays for CSI params. 256 字节足以容纳组合真彩色
                            // SGR(如 0;1;38;2;...;48;2;... 仅 ~40 字节),避免截断丢色。
                            let mut param_bytes = [0u8; 256];
                            let mut param_len = 0;
                            let mut intermediates = [0u8; 8];
                            let mut inter_len = 0;
                            let mut final_byte = None;

                            while i < data_slice.len() {
                                match data_slice[i] {
                                    0x30..=0x3f => {
                                        if param_len < param_bytes.len() {
                                            param_bytes[param_len] = data_slice[i];
                                            param_len += 1;
                                        }
                                    }
                                    0x20..=0x2f => {
                                        if inter_len < intermediates.len() {
                                            intermediates[inter_len] = data_slice[i];
                                            inter_len += 1;
                                        }
                                    }
                                    0x40..=0x7e => {
                                        final_byte = Some(data_slice[i]);
                                        break;
                                    }
                                    _ => break,
                                }
                                i += 1;
                            }

                            let Some(final_byte) = final_byte else {
                                self.stash_pending_escape(&data_slice[esc_start..]);
                                break;
                            };

                            let private_prefix = match param_bytes.first().copied() {
                                Some(prefix @ (b'<' | b'=' | b'>' | b'?')) => {
                                    // Shift remaining params left
                                    for j in 0..param_len - 1 {
                                        param_bytes[j] = param_bytes[j + 1];
                                    }
                                    param_len -= 1;
                                    Some(prefix)
                                }
                                _ => None,
                            };
                            let (params, colon_flags) =
                                Self::parse_csi_params(&param_bytes[..param_len]);
                            let cmd = final_byte as char;

                            self.handle_escape_sequence(
                                &params,
                                &colon_flags,
                                cmd,
                                private_prefix,
                                &intermediates[..inter_len],
                            );
                            i += 1;
                        }
                        _ => {
                            i += 1;
                        }
                    }
                }
                32..=126 => {
                    // ASCII fast path: scan for run of printable ASCII and process in bulk
                    if self.utf8_len == 0 && self.active_charset == Charset::Ascii {
                        let run_start = i;
                        i += 1;
                        while i < data_slice.len() {
                            let b = data_slice[i];
                            if !(32..=126).contains(&b) {
                                break;
                            }
                            i += 1;
                        }
                        self.put_ascii_run(&data_slice[run_start..i]);
                    } else {
                        self.put_char(byte as char);
                        i += 1;
                    }
                }
                // UTF-8 multi-byte sequences: try to consume all bytes eagerly
                0xC2..=0xDF => {
                    self.consume_utf8_lead(byte, 2, data_slice, &mut i);
                }
                0xE0..=0xEF => {
                    self.consume_utf8_lead(byte, 3, data_slice, &mut i);
                }
                0xF0..=0xF4 => {
                    self.consume_utf8_lead(byte, 4, data_slice, &mut i);
                }
                _ => {
                    // 到这里 byte 要么是续接字节(由上方的残缺检测保证此时确有进行中的序列),
                    // 要么是非法的 UTF-8 引导字节(0xC0/0xC1/0xF5..=0xFF)。
                    if self.utf8_len > 0 && (byte & 0xC0) == 0x80 {
                        self.utf8_buf[self.utf8_len as usize] = byte;
                        self.utf8_len += 1;
                        if self.utf8_len == self.utf8_expected {
                            match std::str::from_utf8(&self.utf8_buf[..self.utf8_len as usize]) {
                                Ok(s) => {
                                    if let Some(ch) = s.chars().next() {
                                        self.put_char(ch);
                                    }
                                }
                                // 长度够但内容非法(如过长编码/代理区):输出替换字符。
                                Err(_) => self.put_char('\u{FFFD}'),
                            }
                            self.utf8_len = 0;
                        }
                    } else {
                        // 孤立的续接字节或非法引导字节。
                        self.put_char('\u{FFFD}');
                        self.utf8_len = 0;
                    }
                    i += 1;
                }
            }
        }
    }

    pub(super) fn handle_escape_sequence(
        &mut self,
        params: &[u16],
        colon_flags: &[bool],
        cmd: char,
        private_prefix: Option<u8>,
        intermediates: &[u8],
    ) {
        // 显式光标定位会取消延迟换行标志(DEC 末列标志)。
        if matches!(
            cmd,
            'A' | 'B' | 'C' | 'D' | 'E' | 'F' | 'G' | 'H' | 'f' | 'd' | '`'
        ) {
            self.pending_wrap = false;
        }
        match cmd {
            'A' => {
                // CUU - Cursor Up:仅移动光标,绝不滚动。
                // 区内止于上边距,区外(上边距上方)止于屏幕顶部。
                let n = params.first().copied().unwrap_or(1) as usize;
                let floor = if self.cursor_row >= self.scroll_region_top {
                    self.scroll_region_top
                } else {
                    0
                };
                self.cursor_row = self.cursor_row.saturating_sub(n).max(floor);
            }
            'B' => {
                // CUD - Cursor Down:区内止于底边距,区外止于屏幕底部;不滚动。
                let n = params.first().copied().unwrap_or(1) as usize;
                let ceil = if self.cursor_row <= self.scroll_region_bottom {
                    self.scroll_region_bottom
                } else {
                    self.grid.rows().saturating_sub(1)
                };
                self.cursor_row = (self.cursor_row + n).min(ceil);
            }
            'C' => {
                let n = params.first().copied().unwrap_or(1) as usize;
                self.cursor_col = (self.cursor_col + n).min(self.grid.row_len() - 1);
            }
            'D' => {
                let n = params.first().copied().unwrap_or(1) as usize;
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            'E' => {
                // CNL - Cursor Next Line:下移并到行首,受底边距约束;不滚动。
                let n = params.first().copied().unwrap_or(1) as usize;
                let ceil = if self.cursor_row <= self.scroll_region_bottom {
                    self.scroll_region_bottom
                } else {
                    self.grid.rows().saturating_sub(1)
                };
                self.cursor_row = (self.cursor_row + n).min(ceil);
                self.cursor_col = 0;
            }
            'F' => {
                // CPL - Cursor Previous Line:上移并到行首,受上边距约束;不滚动。
                let n = params.first().copied().unwrap_or(1) as usize;
                let floor = if self.cursor_row >= self.scroll_region_top {
                    self.scroll_region_top
                } else {
                    0
                };
                self.cursor_row = self.cursor_row.saturating_sub(n).max(floor);
                self.cursor_col = 0;
            }
            'G' => {
                // CHA - Move cursor to column (1-based)
                let col = params.first().copied().unwrap_or(1) as usize;
                self.cursor_col = col.saturating_sub(1).min(self.grid.row_len() - 1);
            }
            '`' => {
                // HPA - Horizontal Position Absolute(列绝对,等价 CHA)
                let col = params.first().copied().unwrap_or(1) as usize;
                self.cursor_col = col
                    .saturating_sub(1)
                    .min(self.grid.row_len().saturating_sub(1));
            }
            'd' => {
                // VPA - Vertical Position Absolute(行绝对,1 基)。
                // 原点模式下相对滚动区域顶端并限制在区域内。
                let row = params.first().copied().unwrap_or(1) as usize;
                let row0 = row.saturating_sub(1);
                self.cursor_row = if self.origin_mode {
                    (self.scroll_region_top + row0).min(self.scroll_region_bottom)
                } else {
                    row0.min(self.grid.rows().saturating_sub(1))
                };
            }
            'H' => {
                let row = params.first().copied().unwrap_or(1) as usize;
                let col = params.get(1).copied().unwrap_or(1) as usize;
                self.set_cursor_position(row, col);
            }
            'f' => {
                if private_prefix == Some(b'>') && intermediates.is_empty() {
                    let resource = params.first().copied().unwrap_or(0);
                    let value = params.get(1).copied().unwrap_or(0);
                    if resource == 4 {
                        crate::debug_log!(
                            "[XTFMTKEYS] formatOtherKeys={} previous={}",
                            value,
                            self.xterm_format_other_keys
                        );
                        self.xterm_format_other_keys = value;
                    }
                } else {
                    let row = params.first().copied().unwrap_or(1) as usize;
                    let col = params.get(1).copied().unwrap_or(1) as usize;
                    self.set_cursor_position(row, col);
                }
            }
            'J' => {
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        // Clear from cursor to end of display
                        for col in self.cursor_col..self.grid.row_len() {
                            self.clear_cell(self.cursor_row, col);
                        }
                        for row in (self.cursor_row + 1)..self.grid.rows() {
                            for col in 0..self.grid.row_len() {
                                self.clear_cell(row, col);
                            }
                        }
                        // Mark affected rows as dirty
                        self.dirty_region
                            .mark_rows(self.cursor_row, self.grid.rows().saturating_sub(1));
                        self.mark_rows_dirty(self.cursor_row, self.grid.rows().saturating_sub(1));
                    }
                    1 => {
                        // Clear from start to cursor
                        for row in 0..=self.cursor_row {
                            let end_col = if row == self.cursor_row {
                                self.cursor_col + 1
                            } else {
                                self.grid.row_len()
                            };
                            for col in 0..end_col {
                                self.clear_cell(row, col);
                            }
                        }
                        // Mark affected rows as dirty
                        self.dirty_region.mark_rows(0, self.cursor_row);
                        self.mark_rows_dirty(0, self.cursor_row);
                    }
                    2 => {
                        // ED 擦除显示不移动光标(VT 规范)
                        if self.sync_output_active {
                            if self.use_alt_buffer {
                                self.archive_visible_screen_to_scrollback_with_options(true, true);
                            } else {
                                self.archive_primary_screen_unless_last_synced_snapshot();
                            }
                        } else {
                            self.archive_visible_screen_to_scrollback();
                        }
                        self.erase_screen();
                    }
                    3 => {
                        // Clear scrollback buffer (xterm extension)
                        self.scrollback.clear();
                        self.invalidate_scrollback_view_cache();
                        self.kitty_graphics.clear_scrollback_placements();
                        self.scroll_offset = 0;
                    }
                    _ => {}
                }
            }
            'K' => {
                // Clear line
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        // Clear from cursor to end of line
                        for col in self.cursor_col..self.grid.row_len() {
                            self.clear_cell(self.cursor_row, col);
                        }
                        // Mark the line as dirty
                        self.dirty_region.mark_row(self.cursor_row);
                        self.mark_row_dirty(self.cursor_row);
                    }
                    1 => {
                        // Clear from start of line to cursor
                        for col in 0..=self.cursor_col {
                            self.clear_cell(self.cursor_row, col);
                        }
                        // Mark the line as dirty
                        self.dirty_region.mark_row(self.cursor_row);
                        self.mark_row_dirty(self.cursor_row);
                    }
                    2 => {
                        // Clear entire line
                        for col in 0..self.grid.row_len() {
                            self.clear_cell(self.cursor_row, col);
                        }
                        // Mark the line as dirty
                        self.dirty_region.mark_row(self.cursor_row);
                        self.mark_row_dirty(self.cursor_row);
                    }
                    _ => {}
                }
            }
            'L' => {
                // IL — insert N blank lines at cursor. After (region_height)
                // iterations the entire region is blank, so cap N there to
                // avoid O(N · region · cols) work for adversarial N=65535.
                let n = params.first().copied().unwrap_or(1) as usize;
                if self.cursor_row >= self.scroll_region_top
                    && self.cursor_row <= self.scroll_region_bottom
                {
                    let region_height = self.scroll_region_bottom - self.cursor_row + 1;
                    let n = n.min(region_height);
                    let blank = self.create_blank_cell();
                    let cols = self.grid.row_len();
                    for _ in 0..n {
                        let src_start = self.cursor_row * cols;
                        let src_end = self.scroll_region_bottom * cols;
                        let dst = (self.cursor_row + 1) * cols;
                        self.grid.cells.copy_within(src_start..src_end, dst);
                        self.grid.cells[src_start..src_start + cols].fill(blank);
                    }
                    self.kitty_graphics.scroll_region_down(
                        self.cursor_row,
                        self.scroll_region_bottom,
                        n,
                    );
                }
                self.mark_rows_dirty(self.cursor_row, self.scroll_region_bottom);
            }
            'M' => {
                // DL — delete N lines at cursor. Same cap logic as IL.
                let n = params.first().copied().unwrap_or(1) as usize;
                if self.cursor_row >= self.scroll_region_top
                    && self.cursor_row <= self.scroll_region_bottom
                {
                    let region_height = self.scroll_region_bottom - self.cursor_row + 1;
                    let n = n.min(region_height);
                    let blank = self.create_blank_cell();
                    let cols = self.grid.row_len();
                    for _ in 0..n {
                        let src_start = (self.cursor_row + 1) * cols;
                        let src_end = (self.scroll_region_bottom + 1) * cols;
                        let dst = self.cursor_row * cols;
                        self.grid.cells.copy_within(src_start..src_end, dst);
                        let blank_start = self.scroll_region_bottom * cols;
                        self.grid.cells[blank_start..blank_start + cols].fill(blank);
                    }
                    self.kitty_graphics.scroll_region_up(
                        self.cursor_row,
                        self.scroll_region_bottom,
                        n,
                        false,
                    );
                }
                self.mark_rows_dirty(self.cursor_row, self.scroll_region_bottom);
            }
            'm' => {
                if private_prefix == Some(b'>') && intermediates.is_empty() {
                    let resource = params.first().copied().unwrap_or(0);
                    let value = params.get(1).copied().unwrap_or(0);
                    if resource == 4 {
                        crate::debug_log!(
                            "[XTMODKEYS] modifyOtherKeys={} previous={}",
                            value,
                            self.xterm_modify_other_keys
                        );
                        self.xterm_modify_other_keys = value;
                    }
                } else {
                    // SGR - Select Graphic Rendition
                    self.handle_sgr(params, colon_flags);
                }
            }
            's' => {
                if private_prefix.is_none() && intermediates.is_empty() {
                    self.save_cursor_state();
                }
            }
            'u' => {
                if intermediates.is_empty() {
                    match private_prefix {
                        None => {
                            self.restore_cursor_state();
                        }
                        Some(b'?') => {
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] query current kitty flags -> {}",
                                self.keyboard_enhancement_flags
                            );
                            let response = format!("\x1b[?{}u", self.keyboard_enhancement_flags);
                            self.output_buffer.extend_from_slice(response.as_bytes());
                        }
                        Some(b'=') => {
                            let flags = params.first().copied().unwrap_or(0);
                            let mode = params.get(1).copied().unwrap_or(1);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] set kitty flags flags={} mode={} previous={}",
                                flags,
                                mode,
                                self.keyboard_enhancement_flags
                            );
                            self.set_keyboard_enhancement_flags(flags, mode);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] new kitty flags={}",
                                self.keyboard_enhancement_flags
                            );
                        }
                        Some(b'>') => {
                            let flags = params.first().copied().unwrap_or(0);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] push kitty flags current={} new={}",
                                self.keyboard_enhancement_flags,
                                flags
                            );
                            self.push_keyboard_enhancement_flags(flags);
                        }
                        Some(b'<') => {
                            let count = params.first().copied().unwrap_or(1) as usize;
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] pop kitty flags count={} current={} stack_depth={}",
                                count,
                                self.keyboard_enhancement_flags,
                                self.keyboard_enhancement_stack.len()
                            );
                            self.pop_keyboard_enhancement_flags(count);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] new kitty flags={}",
                                self.keyboard_enhancement_flags
                            );
                        }
                        _ => {}
                    }
                }
            }
            'S' => {
                // SU — Scroll Up. Cap to region height: more would just blank
                // an already-blank region while doing O(rows) work each step.
                let n = params.first().copied().unwrap_or(1) as usize;
                let region_height = self
                    .scroll_region_bottom
                    .saturating_sub(self.scroll_region_top)
                    + 1;
                let n = n.min(region_height);
                for _ in 0..n {
                    self.scroll_region_up(self.scroll_region_top, self.scroll_region_bottom);
                }
            }
            'T' => {
                // SD — Scroll Down. Same cap as SU.
                let n = params.first().copied().unwrap_or(1) as usize;
                let region_height = self
                    .scroll_region_bottom
                    .saturating_sub(self.scroll_region_top)
                    + 1;
                let n = n.min(region_height);
                for _ in 0..n {
                    self.scroll_region_down(self.scroll_region_top, self.scroll_region_bottom);
                }
            }
            'n' => {
                // DSR - Device Status Report
                // ESC[6n requests cursor position
                if params.first().copied().unwrap_or(0) == 6 {
                    // Respond with CPR (Cursor Position Report): ESC[row;colR
                    // Row and Col are 1-indexed
                    let row = (self.cursor_row + 1) as u16;
                    let col = (self.cursor_col + 1) as u16;

                    // Send cursor position response back to PTY
                    let response = format!("\x1b[{};{}R", row, col);
                    self.output_buffer.extend(response.as_bytes());
                }
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
            'p' => {
                if private_prefix == Some(b'?')
                    && intermediates == *b"$"
                    && params.first().copied() == Some(5522)
                {
                    let state = if self.modes.contains(&5522) { 1 } else { 2 };
                    let response = format!("\x1b[?5522;{}$y", state);
                    crate::debug_log!("[OSC5522] DECRQM query -> {}", response);
                    self.output_buffer.extend_from_slice(response.as_bytes());
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
            'r' if private_prefix.is_none() => {
                // Set scroll region (DECSTBM)。带私有前缀(如 CSI ? Pm r 的 XTRESTORE)
                // 不是 DECSTBM,不能误设滚动区域,故仅在无前缀时处理。
                let top = match params.first().copied().unwrap_or(1) {
                    0 => 1,
                    v => v as usize,
                };
                let bottom = match params.get(1).copied().unwrap_or(self.grid.rows() as u16) {
                    0 => self.grid.rows(),
                    v => v as usize,
                };

                // Convert from 1-indexed to 0-indexed, and clamp to valid range
                self.scroll_region_top = top
                    .saturating_sub(1)
                    .min(self.grid.rows().saturating_sub(1));
                self.scroll_region_bottom = bottom
                    .saturating_sub(1)
                    .min(self.grid.rows().saturating_sub(1));

                // If range is invalid, reset to full screen
                if self.scroll_region_top > self.scroll_region_bottom {
                    self.scroll_region_top = 0;
                    self.scroll_region_bottom = self.grid.rows().saturating_sub(1);
                }

                // Move cursor to home position when setting scroll region. In
                // origin mode (DECOM) home is the top of the scroll region, not
                // the absolute top of the screen.
                self.cursor_row = if self.origin_mode {
                    self.scroll_region_top
                } else {
                    0
                };
                self.cursor_col = 0;
                self.pending_wrap = false;
            }
            '@' => {
                // ICH - Insert Character(s). Cap N to remaining columns; further
                // iterations would just keep dropping the rightmost cell.
                let n = params.first().copied().unwrap_or(1) as usize;
                let cols = self.grid.row_len();
                let blank_cell = self.create_blank_cell();
                if self.cursor_col < cols {
                    let n = n.min(cols - self.cursor_col);
                    for _ in 0..n {
                        self.grid
                            .insert_cell_in_row(self.cursor_row, self.cursor_col, blank_cell);
                    }
                    self.mark_row_dirty(self.cursor_row);
                }
            }
            'P' => {
                // DCH - Delete Character(s). Cap N to remaining columns.
                let n = params.first().copied().unwrap_or(1) as usize;
                let cols = self.grid.row_len();
                let blank_cell = self.create_blank_cell();
                if self.cursor_col < cols {
                    let n = n.min(cols - self.cursor_col);
                    for _ in 0..n {
                        self.grid
                            .remove_cell_from_row(self.cursor_row, self.cursor_col);
                        let last_col = cols - 1;
                        *self.grid.get_mut(self.cursor_row, last_col) = blank_cell;
                    }
                    self.mark_row_dirty(self.cursor_row);
                }
            }
            'X' => {
                // ECH - Erase Character(s)
                let n = params.first().copied().unwrap_or(1) as usize;
                for i in 0..n {
                    let col = self.cursor_col + i;
                    if col < self.grid.row_len() {
                        self.clear_cell(self.cursor_row, col);
                    } else {
                        break;
                    }
                }
                // Mark row as dirty after modification
                self.mark_row_dirty(self.cursor_row);
            }
            'q' => {
                if private_prefix == Some(b'>')
                    && intermediates.is_empty()
                    && params.first().copied().unwrap_or(0) == 0
                {
                    crate::debug_log!("[XTVERSION] report terminal version request");
                    self.output_buffer.extend_from_slice(XTERM_VERSION_RESPONSE);
                }

                // DECSCUSR - Set cursor style
                if private_prefix.is_none() && intermediates == *b" " {
                    let shape = params.first().copied().unwrap_or(0) as u8;
                    self.cursor_shape = match shape {
                        0 | 1 => CursorShape::Block,
                        2 => CursorShape::Underline,
                        3 => CursorShape::Beam,
                        _ => CursorShape::Block,
                    };
                }
            }
            'g' => {
                // TBC - Tab Clear
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        // Clear tab stop at cursor
                        if let Some(stop) = self.tab_stops.get_mut(self.cursor_col) {
                            *stop = false;
                        }
                    }
                    3 => {
                        // Clear all tab stops
                        for stop in self.tab_stops.iter_mut() {
                            *stop = false;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// 解析从下标 `i`(38/48/58)开始的扩展颜色子序列,返回 `(颜色, 最后消费的下标)`。
    /// 同时支持分号传统形式(`38;2;r;g;b`)与冒号 ITU 形式(`38:2:r:g:b` 或
    /// 带颜色空间字段的 `38:2:cs:r:g:b`)。冒号形式靠 colon_flags 识别,并据子参数
    /// 个数决定是否跳过颜色空间字段,避免把 cs 当成 r 而整体错位。
    pub(super) fn parse_sgr_extended_color(
        params: &[u16],
        colon_flags: &[bool],
        i: usize,
    ) -> Option<(Color, usize)> {
        match params.get(i + 1).copied()? {
            5 => {
                let idx = params.get(i + 2).copied()? as u8;
                Some((Color::Indexed(idx), i + 2))
            }
            2 => {
                // 计算从 i 起的冒号连续子参数组长度(组内各参数 colon_flag 为 true)。
                let mut end = i + 1;
                while end < params.len() && colon_flags.get(end).copied().unwrap_or(false) {
                    end += 1;
                }
                let colon_form = colon_flags.get(i + 1).copied().unwrap_or(false);
                // 冒号形式且组长 >=6 表示存在颜色空间字段(38,2,cs,r,g,b),r 偏移为 3;
                // 否则(传统分号形式或无 cs 的冒号形式)r 偏移为 2。
                let r_off = if colon_form && (end - i) >= 6 { 3 } else { 2 };
                let r = params.get(i + r_off).copied()?;
                let g = params.get(i + r_off + 1).copied()?;
                let b = params.get(i + r_off + 2).copied()?;
                Some((Color::Rgb(r as u8, g as u8, b as u8), i + r_off + 2))
            }
            _ => None,
        }
    }

    pub(super) fn handle_sgr(&mut self, params: &[u16], colon_flags: &[bool]) {
        if params.is_empty() {
            self.current_flags = StyleFlags::default();
            self.current_fg = Color::Default;
            self.current_bg = Color::Default;
            return;
        }

        let mut i = 0;
        while i < params.len() {
            let param = params[i];
            match param {
                0 => {
                    self.current_flags = StyleFlags::default();
                    self.current_fg = Color::Default;
                    self.current_bg = Color::Default;
                }
                1 => self.current_flags.set_bold(true),
                2 => self.current_flags.set_dim(true),
                3 => self.current_flags.set_italic(true),
                4 => {
                    // 仅当下一个参数是冒号子参数(`4:x`)时才作为扩展下划线样式;
                    // 分号分隔的 `4;x` 中 x 是独立 SGR(如 4;1 = 下划线+粗体),不能吞。
                    let next_is_substyle = i + 1 < params.len()
                        && colon_flags.get(i + 1).copied().unwrap_or(false)
                        && params[i + 1] <= 5;
                    if next_is_substyle {
                        let style = params[i + 1];
                        self.current_flags.set_underline(match style {
                            0 => UnderlineStyle::None,
                            1 => UnderlineStyle::Single,
                            2 => UnderlineStyle::Double,
                            3 => UnderlineStyle::Curly,
                            4 => UnderlineStyle::Dotted,
                            5 => UnderlineStyle::Dashed,
                            _ => UnderlineStyle::Single,
                        });
                        i += 1;
                    } else {
                        self.current_flags.set_underline(UnderlineStyle::Single);
                    }
                }
                5 => self.current_flags.set_blink(true),
                7 => self.current_flags.set_inverse(true),
                9 => self.current_flags.set_strikethrough(true),
                21 => self.current_flags.set_underline(UnderlineStyle::Double),
                22 => {
                    self.current_flags.set_bold(false);
                    self.current_flags.set_dim(false);
                }
                23 => self.current_flags.set_italic(false),
                24 => self.current_flags.set_underline(UnderlineStyle::None),
                25 => self.current_flags.set_blink(false),
                27 => self.current_flags.set_inverse(false),
                29 => self.current_flags.set_strikethrough(false),
                39 => self.current_fg = Color::Default,
                30..=37 => {
                    self.current_fg = match param {
                        30 => Color::Black,
                        31 => Color::Red,
                        32 => Color::Green,
                        33 => Color::Yellow,
                        34 => Color::Blue,
                        35 => Color::Magenta,
                        36 => Color::Cyan,
                        37 => Color::White,
                        _ => Color::Default,
                    };
                }
                49 => self.current_bg = Color::Default,
                40..=47 => {
                    self.current_bg = match param {
                        40 => Color::Black,
                        41 => Color::Red,
                        42 => Color::Green,
                        43 => Color::Yellow,
                        44 => Color::Blue,
                        45 => Color::Magenta,
                        46 => Color::Cyan,
                        47 => Color::White,
                        _ => Color::Default,
                    };
                    self.global_bg = self.current_bg; // Update global background
                    crate::debug_log!("[CSI] Background color set to: {:?}", self.current_bg);
                }
                90..=97 => {
                    self.current_fg = match param {
                        90 => Color::BrightBlack,
                        91 => Color::BrightRed,
                        92 => Color::BrightGreen,
                        93 => Color::BrightYellow,
                        94 => Color::BrightBlue,
                        95 => Color::BrightMagenta,
                        96 => Color::BrightCyan,
                        97 => Color::BrightWhite,
                        _ => Color::Default,
                    };
                }
                100..=107 => {
                    self.current_bg = match param {
                        100 => Color::BrightBlack,
                        101 => Color::BrightRed,
                        102 => Color::BrightGreen,
                        103 => Color::BrightYellow,
                        104 => Color::BrightBlue,
                        105 => Color::BrightMagenta,
                        106 => Color::BrightCyan,
                        107 => Color::BrightWhite,
                        _ => Color::Default,
                    };
                    self.global_bg = self.current_bg; // Update global background
                }
                // 扩展前景色:38;5;n / 38;2;r;g;b 及对应冒号形式
                38 => {
                    if let Some((color, last)) =
                        Self::parse_sgr_extended_color(params, colon_flags, i)
                    {
                        self.current_fg = color;
                        i = last;
                    }
                }
                // 扩展背景色
                48 => {
                    if let Some((color, last)) =
                        Self::parse_sgr_extended_color(params, colon_flags, i)
                    {
                        self.current_bg = color;
                        self.global_bg = self.current_bg;
                        i = last;
                    }
                }
                // 58 设置下划线颜色 / 59 复位。当前渲染管线未存储独立下划线颜色,
                // 故仅正确消费其子参数,避免颜色分量泄漏成后续 SGR 码被误解析。
                58 => {
                    if let Some((_color, last)) =
                        Self::parse_sgr_extended_color(params, colon_flags, i)
                    {
                        i = last;
                    }
                }
                59 => {}
                _ => {}
            }
            i += 1;
        }
    }

    /// 擦除整个屏幕单元格(保留当前背景色),不移动光标。
    /// 供 ED(`CSI 2J`)使用 —— 按 VT 规范擦除显示不得移动光标。
    pub(super) fn erase_screen(&mut self) {
        let bg_color = self.current_bg;
        for row in self.grid.iter_mut() {
            for cell in row.iter_mut() {
                *cell = TerminalCell {
                    character: ' ',
                    foreground: Color::Default,
                    background: bg_color,
                    flags: StyleFlags::default(),
                };
            }
        }
        // Mark all rows as dirty
        self.dirty_region.mark_all(self.grid.rows());
        self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
        self.kitty_graphics.clear_placements();
    }

    /// 擦除屏幕并把光标归位到左上角。供切换备用缓冲区等场景使用。
    pub(super) fn clear_screen(&mut self) {
        self.erase_screen();
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    /// 切入备用屏幕缓冲。`save_cursor` 控制是否保存主屏光标(1049/1048 语义),
    /// `clear` 控制切入后是否清屏(1047/1049 语义)。已在备用屏则无操作。
    pub(super) fn enter_alt_screen(&mut self, save_cursor: bool, clear: bool) {
        if self.use_alt_buffer {
            return;
        }
        // A selection belongs to the screen on which it was made. Keeping its
        // absolute cell coordinates while swapping buffers paints an unrelated
        // rectangle at the same viewport position in Vim/less.
        self.selection = None;
        if save_cursor {
            self.saved_cursor_row = self.cursor_row;
            self.saved_cursor_col = self.cursor_col;
        }
        // 备用屏不显示 scrollback
        self.scroll_offset = 0;
        std::mem::swap(&mut self.grid, &mut self.alt_grid);
        self.kitty_graphics.switch_screen();
        self.alt_cursor_row = self.cursor_row;
        self.alt_cursor_col = self.cursor_col;
        std::mem::swap(
            &mut self.keyboard_enhancement_flags,
            &mut self.alt_keyboard_enhancement_flags,
        );
        std::mem::swap(
            &mut self.keyboard_enhancement_stack,
            &mut self.alt_keyboard_enhancement_stack,
        );
        self.use_alt_buffer = true;
        if clear {
            self.clear_screen();
            // ED2 only removes placements intersecting the visible viewport so
            // primary-screen scrollback survives. A freshly entered alternate
            // buffer has no scrollback and must discard every placement left
            // from its previous use, including any off-screen residue.
            self.kitty_graphics.clear_current_screen_placements();
        }
    }

    /// 切回主屏幕缓冲。`restore_cursor` 控制是否恢复进入备用屏前保存的光标(1049 语义)。
    /// 不在备用屏则无操作。
    pub(super) fn exit_alt_screen(&mut self, restore_cursor: bool) {
        if !self.use_alt_buffer {
            return;
        }
        self.selection = None;
        self.alt_cursor_row = self.cursor_row;
        self.alt_cursor_col = self.cursor_col;
        std::mem::swap(&mut self.grid, &mut self.alt_grid);
        self.kitty_graphics.switch_screen();
        if restore_cursor {
            self.cursor_row = self.saved_cursor_row;
            self.cursor_col = self.saved_cursor_col;
        }
        std::mem::swap(
            &mut self.keyboard_enhancement_flags,
            &mut self.alt_keyboard_enhancement_flags,
        );
        std::mem::swap(
            &mut self.keyboard_enhancement_stack,
            &mut self.alt_keyboard_enhancement_stack,
        );
        self.use_alt_buffer = false;

        // 重置 SGR 属性,防止备用屏颜色泄漏到主屏
        self.current_fg = Color::Default;
        self.current_bg = Color::Default;
        self.global_bg = Color::Default;
        self.current_flags = StyleFlags::default();

        // 交换缓冲后强制整屏重绘(+rows+1 触发 ui.rs 的 grid_version_jumped)
        self.grid_version += self.grid.rows() as u64 + 1;
        for row_ver in &mut self.row_versions {
            *row_ver = self.grid_version;
        }
        self.dirty_region.mark_all(self.grid.rows());
    }

    pub(super) fn set_mode(&mut self, mode: u16) {
        match mode {
            25 => {
                // Show cursor (mode 25)
                self.modes.insert(25);
            }
            1004 => {
                // Focus event reporting
                self.modes.insert(1004);
            }
            2004 => {
                // Bracketed paste mode
                self.modes.insert(2004);
            }
            1000..=1003 => {
                // Mouse reporting modes
                self.modes.insert(mode);
            }
            1006 => {
                // SGR mouse reporting format
                self.modes.insert(mode);
            }
            47 => {
                // 备用屏(无保存光标、无清屏)
                self.enter_alt_screen(false, false);
                self.modes.insert(47);
            }
            1047 => {
                // 备用屏(切入时清屏)
                self.enter_alt_screen(false, true);
                self.modes.insert(1047);
            }
            1048 => {
                // 仅保存光标(等价 DECSC)
                self.saved_cursor_row = self.cursor_row;
                self.saved_cursor_col = self.cursor_col;
            }
            1049 => {
                // 备用屏:保存光标 + 切入 + 清屏
                self.enter_alt_screen(true, true);
                self.modes.insert(1049);
            }
            2026 => {
                // Synchronized output: suppress rendering until cleared
                self.modes.insert(2026);
                self.sync_output_active = true;
                self.sync_output_start = Some(std::time::Instant::now());
            }
            7 => {
                // Autowrap mode
                self.modes.insert(7);
            }
            6 => {
                // DECOM - 原点模式:寻址相对滚动区域,光标移到区域原点
                self.origin_mode = true;
                self.cursor_row = self.scroll_region_top;
                self.cursor_col = 0;
            }
            _ => {
                // Unknown mode, just store it
                self.modes.insert(mode);
            }
        }
    }

    /// ANSI 标准模式 (CSI Pn h,无 ? 前缀)。目前仅 IRM(4) 有实际效果。
    pub(super) fn set_ansi_mode(&mut self, mode: u16) {
        if mode == 4 {
            // IRM - 插入替换模式
            self.insert_mode = true;
        }
    }

    pub(super) fn reset_ansi_mode(&mut self, mode: u16) {
        if mode == 4 {
            self.insert_mode = false;
        }
    }

    pub(super) fn reset_mode(&mut self, mode: u16) {
        match mode {
            25 => {
                // Hide cursor
                self.modes.remove(&25);
            }
            1004 => {
                // Disable focus event reporting
                self.modes.remove(&1004);
            }
            2004 => {
                // Disable bracketed paste mode
                self.modes.remove(&2004);
            }
            5522 => {
                // A paste capability is valid only while the application keeps
                // paste-event mode enabled. Revocation must be immediate.
                self.modes.remove(&5522);
                self.pending_paste_grant = None;
            }
            1000..=1003 => {
                // Disable mouse reporting
                self.modes.remove(&mode);
            }
            1006 => {
                // Disable SGR mouse reporting format
                self.modes.remove(&mode);
            }
            47 => {
                // 退出备用屏(不恢复光标)
                self.exit_alt_screen(false);
                self.modes.remove(&47);
            }
            1047 => {
                // 退出备用屏(不恢复光标)
                self.exit_alt_screen(false);
                self.modes.remove(&1047);
            }
            1048 => {
                // 仅恢复光标(等价 DECRC)
                self.cursor_row = self
                    .saved_cursor_row
                    .min(self.grid.rows().saturating_sub(1));
                self.cursor_col = self
                    .saved_cursor_col
                    .min(self.grid.row_len().saturating_sub(1));
            }
            1049 => {
                // 退出备用屏并恢复进入前保存的光标
                self.exit_alt_screen(true);
                self.modes.remove(&1049);
            }
            2026 => {
                // End synchronized output: force full render
                if self.use_alt_buffer {
                    self.archive_visible_screen_to_scrollback_with_options(true, true);
                } else {
                    self.last_synced_primary_screen_snapshot =
                        self.visible_screen_snapshot().unwrap_or_default();
                }
                self.modes.remove(&2026);
                self.sync_output_active = false;
                self.sync_output_start = None;
                self.dirty_region.mark_all(self.grid.rows());
                self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
            }
            7 => {
                // Disable autowrap
                self.modes.remove(&7);
            }
            6 => {
                // DECOM 关闭:恢复绝对寻址,光标移到屏幕原点
                self.origin_mode = false;
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
            _ => {
                // Unknown mode, just remove it
                self.modes.remove(&mode);
            }
        }
    }
}
