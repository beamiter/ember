use super::*;

impl super::TerminalState {
    /// 解析 CSI 参数字节。
    ///
    /// 返回 `(params, colon_flags)`,其中 `colon_flags[k]` 表示参数 k 之前的
    /// 分隔符是否为冒号(子参数语法,如 `4:3`)。这样调用方可区分 `4:3`
    /// (扩展下划线样式)与 `4;3`(下划线 + 斜体两个独立 SGR)。
    ///
    /// 与 VT 规范一致:空字段默认为 0(`;5`→`[0,5]`、`5;`→`[5,0]`),
    /// 完全为空的参数串返回空向量(由各处理器使用各自默认值)。
    pub(super) fn parse_csi_params(
        param_bytes: &[u8],
    ) -> (SmallVec<[u16; 8]>, SmallVec<[bool; 8]>) {
        let mut params: SmallVec<[u16; 8]> = SmallVec::new();
        let mut colon_flags: SmallVec<[bool; 8]> = SmallVec::new();
        if param_bytes.is_empty() {
            return (params, colon_flags);
        }

        let mut current: u16 = 0;
        // 当前正在累积的参数之前的分隔符是否为冒号(首个参数无前导分隔符)
        let mut current_is_colon = false;

        for &byte in param_bytes {
            match byte {
                b'0'..=b'9' => {
                    current = current
                        .saturating_mul(10)
                        .saturating_add((byte - b'0') as u16);
                }
                b';' | b':' => {
                    params.push(current);
                    colon_flags.push(current_is_colon);
                    current = 0;
                    current_is_colon = byte == b':';
                }
                _ => {}
            }
        }
        params.push(current);
        colon_flags.push(current_is_colon);

        (params, colon_flags)
    }

    /// 默认每 8 列一个制表位。
    pub(super) fn default_tab_stops(cols: usize) -> Vec<bool> {
        (0..cols).map(|c| c % 8 == 0).collect()
    }

    /// 从给定列出发,返回下一个制表位的列(无则停在最后一列)。
    pub(super) fn next_tab_stop(&self, col: usize) -> usize {
        let cols = self.grid.row_len();
        let mut c = col + 1;
        while c < cols {
            if self.tab_stops.get(c).copied().unwrap_or(false) {
                return c;
            }
            c += 1;
        }
        cols.saturating_sub(1)
    }

    /// DECSC / CSI s:保存完整光标状态(含 SGR、字符集、模式)。
    pub(super) fn save_cursor_state(&mut self) {
        self.saved_state = Some(SavedCursorState {
            row: self.cursor_row,
            col: self.cursor_col,
            fg: self.current_fg,
            bg: self.current_bg,
            flags: self.current_flags,
            g0: self.g0_charset,
            g1: self.g1_charset,
            active: self.active_charset,
            origin_mode: self.origin_mode,
            autowrap: self.modes.contains(&7),
            pending_wrap: self.pending_wrap,
        });
    }

    /// DECRC / CSI u:恢复 save_cursor_state 保存的完整状态;
    /// 若从未保存过,按规范复位到原点。
    pub(super) fn restore_cursor_state(&mut self) {
        if let Some(s) = self.saved_state.clone() {
            self.cursor_row = s.row.min(self.grid.rows().saturating_sub(1));
            self.cursor_col = s.col.min(self.grid.row_len().saturating_sub(1));
            self.current_fg = s.fg;
            self.current_bg = s.bg;
            self.current_flags = s.flags;
            self.g0_charset = s.g0;
            self.g1_charset = s.g1;
            self.active_charset = s.active;
            self.origin_mode = s.origin_mode;
            if s.autowrap {
                self.modes.insert(7);
            } else {
                self.modes.remove(&7);
            }
            self.pending_wrap = s.pending_wrap;
        } else {
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.pending_wrap = false;
        }
    }

    /// CUP/HVP 光标定位(1 基参数)。原点模式下行相对滚动区域顶端并限制在区域内。
    pub(super) fn set_cursor_position(&mut self, row_param: usize, col_param: usize) {
        self.pending_wrap = false;
        let col = col_param
            .saturating_sub(1)
            .min(self.grid.row_len().saturating_sub(1));
        let row0 = row_param.saturating_sub(1);
        self.cursor_row = if self.origin_mode {
            (self.scroll_region_top + row0).min(self.scroll_region_bottom)
        } else {
            row0.min(self.grid.rows().saturating_sub(1))
        };
        self.cursor_col = col;
    }

    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = clamp_terminal_dimensions(cols, rows);
        let grid = TerminalGrid::new(rows, cols);
        let alt_grid = TerminalGrid::new(rows, cols);

        let mut modes = TerminalModes::default();
        modes.insert(25);
        modes.insert(7);

        let mut dirty_region = DirtyRegion::new();
        // Mark all rows as dirty on initialization to ensure first frame renders correctly
        dirty_region.mark_all(rows);

        TerminalState {
            grid,
            alt_grid,
            scrollback: VecDeque::new(),
            selection: None,
            scroll_offset: 0,
            max_scrollback: 10000,
            use_alt_buffer: false,
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor_row: 0,
            saved_cursor_col: 0,
            alt_cursor_row: 0,
            alt_cursor_col: 0,
            cursor_shape: CursorShape::default(),
            saved_state: None,
            insert_mode: false,
            origin_mode: false,
            tab_stops: Self::default_tab_stops(cols),
            pending_wrap: false,
            current_fg: Color::Default,
            current_bg: Color::Default,
            current_flags: StyleFlags::default(),
            window_title: String::new(),
            current_working_dir: None,
            global_bg: Color::Default,
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
            pending_escape: Vec::new(),
            g0_charset: Charset::Ascii,
            g1_charset: Charset::Ascii,
            active_charset: Charset::Ascii,
            ime_enabled: false,
            preedit_text: String::new(),
            preedit_cursor: 0,
            scroll_region_top: 0,
            scroll_region_bottom: rows.saturating_sub(1),
            modes,
            output_buffer: Vec::new(),
            keyboard_enhancement_flags: 0,
            keyboard_enhancement_stack: Vec::new(),
            alt_keyboard_enhancement_flags: 0,
            alt_keyboard_enhancement_stack: Vec::new(),
            xterm_modify_other_keys: 0,
            xterm_format_other_keys: 0,
            pending_clipboard_requests: Vec::new(),
            pending_paste_password: None,
            kitty_graphics: KittyGraphicsState::new(),
            dirty_region,
            grid_version: 1,
            // IMPORTANT: row_versions must match grid.rows(), not the parameter 'rows'
            // This ensures dirty tracking works correctly even with scrollback
            row_versions: vec![1; rows], // Use 'rows' here since grid.rows() == rows at init
            visible_cells_cache: None,
            current_hyperlink: None,
            sync_output_active: false,
            sync_output_start: None,
            last_archived_screen_snapshot: Vec::new(),
            last_synced_primary_screen_snapshot: Vec::new(),
            pending_osc52_clipboard_set: None,
            pending_osc52_clipboard_query: false,
            dynamic_fg: None,
            dynamic_bg: None,
            dynamic_cursor_color: None,
            pending_notifications: Vec::new(),
            total_lines_scrolled: 0,
            command_marks: VecDeque::new(),
        }
    }

    pub(super) fn decode_base64(value: &str) -> Option<String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value)
            .ok()?;
        String::from_utf8(bytes).ok()
    }

    pub(super) fn osc_terminator() -> &'static [u8] {
        b"\x1b\\"
    }

    pub(super) fn append_osc_5522_status(&mut self, metadata: &str, payload: Option<&str>) {
        self.output_buffer.extend_from_slice(b"\x1b]5522;");
        self.output_buffer.extend_from_slice(metadata.as_bytes());
        if let Some(payload) = payload {
            self.output_buffer.extend_from_slice(b";");
            self.output_buffer.extend_from_slice(payload.as_bytes());
        }
        self.output_buffer.extend_from_slice(Self::osc_terminator());
    }

    pub(super) fn handle_osc_color(&mut self, command: &str, value: &str) {
        if value == "?" {
            // Query: respond with current color
            let color = match command {
                "10" => self.dynamic_fg.unwrap_or((255, 255, 255)),
                "11" => self.dynamic_bg.unwrap_or((0, 0, 0)),
                "12" => self.dynamic_cursor_color.unwrap_or((255, 255, 255)),
                _ => return,
            };
            let response = format!(
                "\x1b]{};rgb:{:04x}/{:04x}/{:04x}\x1b\\",
                command,
                (color.0 as u16) * 257,
                (color.1 as u16) * 257,
                (color.2 as u16) * 257,
            );
            self.output_buffer.extend_from_slice(response.as_bytes());
        } else if let Some(rgb) = Self::parse_color_spec(value) {
            match command {
                "10" => self.dynamic_fg = Some(rgb),
                "11" => self.dynamic_bg = Some(rgb),
                "12" => self.dynamic_cursor_color = Some(rgb),
                _ => {}
            }
        }
    }

    /// Decode OSC 7 working-directory payload to a local filesystem path.
    /// Accepts either `file://host/path` (path is percent-encoded) or a raw
    /// path. Returns None if the payload is empty or malformed.
    pub(super) fn decode_osc7_cwd(value: &str) -> Option<String> {
        let path_part = if let Some(rest) = value.strip_prefix("file://") {
            // Skip optional hostname segment.
            match rest.find('/') {
                Some(i) => &rest[i..],
                None => return None,
            }
        } else if value.starts_with('/') {
            value
        } else {
            return None;
        };
        // Percent-decode. We don't pull in a url crate — the alphabet is small.
        let bytes = path_part.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                let byte = u8::from_str_radix(hex, 16).ok()?;
                out.push(byte);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        let s = String::from_utf8(out).ok()?;
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    pub(super) fn parse_color_spec(spec: &str) -> Option<(u8, u8, u8)> {
        // Parse rgb:RR/GG/BB / rgb:RRRR/GGGG/BBBB / rgb:R/G/B / rgb:RRR/GGG/BBB / #RRGGBB
        // Per XParseColor, each component is 1..=4 hex digits and is left-aligned
        // into a 16-bit field (i.e. component value * (2^16-1) / (2^bits-1)),
        // then truncated to 8 bits. The previous scale=1 fallback for 1/3-digit
        // components produced a u8 cast of e.g. 0xFFF=4095, which wraps to 255
        // for full-on but is wrong for any partial value.
        fn scale_to_u8(value: u16, hex_digits: usize) -> u8 {
            // Range of n hex digits is [0, 16^n - 1].
            let max_n: u32 = (1u32 << (hex_digits * 4)).saturating_sub(1).max(1);
            // Scale value to 0..=255, rounding to nearest.
            (((value as u32) * 255 + max_n / 2) / max_n) as u8
        }
        if let Some(hex) = spec.strip_prefix('#') {
            if hex.len() == 6 {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                return Some((r, g, b));
            }
        } else if let Some(rgb) = spec.strip_prefix("rgb:") {
            let parts: Vec<&str> = rgb.split('/').collect();
            if parts.len() == 3
                && (1..=4).contains(&parts[0].len())
                && parts[0].len() == parts[1].len()
                && parts[1].len() == parts[2].len()
            {
                let digits = parts[0].len();
                let r = u16::from_str_radix(parts[0], 16).ok()?;
                let g = u16::from_str_radix(parts[1], 16).ok()?;
                let b = u16::from_str_radix(parts[2], 16).ok()?;
                return Some((
                    scale_to_u8(r, digits),
                    scale_to_u8(g, digits),
                    scale_to_u8(b, digits),
                ));
            }
        }
        None
    }

    pub(super) fn handle_osc_52(&mut self, value: &str) {
        // OSC 52 format: <selection>;<base64-data>
        // selection: c=clipboard, p=primary, s=select (we treat all as clipboard)
        // data: ? means query, base64 means set.
        //
        // Cap on payload size: a remote process should not be able to push
        // arbitrary multi-MB blobs into the host clipboard. xterm uses 100 KB
        // by default; we match that.
        const OSC52_MAX_BYTES: usize = 100 * 1024;
        if let Some((_sel, data)) = value.split_once(';') {
            if data == "?" {
                self.pending_osc52_clipboard_query = true;
            } else if !data.is_empty() {
                if data.len() > OSC52_MAX_BYTES.saturating_mul(4) / 3 + 8 {
                    // Reject before even attempting to decode.
                    crate::debug_log!(
                        "[OSC52] rejecting clipboard set: encoded {} bytes exceeds limit",
                        data.len()
                    );
                    return;
                }
                if let Some(decoded) = Self::decode_base64(data) {
                    if decoded.len() <= OSC52_MAX_BYTES {
                        self.pending_osc52_clipboard_set = Some(decoded);
                    } else {
                        crate::debug_log!(
                            "[OSC52] rejecting clipboard set: decoded {} bytes exceeds {}",
                            decoded.len(),
                            OSC52_MAX_BYTES
                        );
                    }
                }
            }
        }
    }

    pub(super) fn handle_osc_5522(&mut self, metadata: &str, _payload: Option<&str>) {
        crate::debug_log!("[OSC5522] metadata={} payload={:?}", metadata, _payload);

        let mut message_type = None;
        let mut mime = None;
        let mut password = None;

        for part in metadata.split(':') {
            if let Some(value) = part.strip_prefix("type=") {
                message_type = Some(value);
            } else if let Some(value) = part.strip_prefix("mime=") {
                mime = Self::decode_base64(value);
            } else if let Some(value) = part.strip_prefix("password=") {
                password = Self::decode_base64(value);
            } else if let Some(value) = part.strip_prefix("pw=") {
                password = Self::decode_base64(value);
            }
        }

        if message_type != Some("read") {
            return;
        }

        let kind = if let Some(mime_type) = mime {
            if let Some(expected) = &self.pending_paste_password {
                if password.as_deref() != Some(expected.as_str()) {
                    self.append_osc_5522_status("type=read:status=EPERM", None);
                    return;
                }
            }
            self.pending_paste_password = None;
            ClipboardReadKind::MimeData(mime_type)
        } else {
            ClipboardReadKind::MimeList
        };

        self.pending_clipboard_requests
            .push(ClipboardReadRequest { kind });
    }

    pub(super) fn set_keyboard_enhancement_flags(&mut self, flags: u16, mode: u16) {
        match mode {
            1 => self.keyboard_enhancement_flags = flags,
            2 => self.keyboard_enhancement_flags |= flags,
            3 => self.keyboard_enhancement_flags &= !flags,
            _ => {}
        }
    }

    pub(super) fn push_keyboard_enhancement_flags(&mut self, flags: u16) {
        if self.keyboard_enhancement_stack.len() >= 32 {
            self.keyboard_enhancement_stack.remove(0);
        }
        self.keyboard_enhancement_stack
            .push(self.keyboard_enhancement_flags);
        self.keyboard_enhancement_flags = flags;
    }

    pub(super) fn pop_keyboard_enhancement_flags(&mut self, count: usize) {
        for _ in 0..count.max(1) {
            match self.keyboard_enhancement_stack.pop() {
                Some(flags) => self.keyboard_enhancement_flags = flags,
                None => {
                    self.keyboard_enhancement_flags = 0;
                    break;
                }
            }
        }
    }

    /// Compose a combining mark onto the most recently written cell using NFC.
    /// Only single-codepoint compositions are applied (the cell stores one char).
    pub(super) fn apply_combining_mark(&mut self, mark: char) {
        // After a char fills the last column, the wrap is deferred: the cursor
        // stays *on* the last column with pending_wrap set, so the base glyph is
        // at cursor_col itself rather than to its left.
        let mut base_col = if self.pending_wrap {
            self.cursor_col
        } else if self.cursor_col == 0 {
            return;
        } else {
            self.cursor_col - 1
        };
        if self
            .grid
            .get(self.cursor_row, base_col)
            .flags
            .wide_continuation()
        {
            if base_col == 0 {
                return;
            }
            base_col -= 1;
        }

        let base = self.grid.get(self.cursor_row, base_col).character;
        if base == ' ' || base == '\0' {
            return;
        }

        let mut composed = String::with_capacity(2);
        composed.push(base);
        composed.push(mark);
        let nfc: String =
            unicode_normalization::UnicodeNormalization::nfc(composed.as_str()).collect();
        let mut chars = nfc.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c != base {
                self.grid.get_mut(self.cursor_row, base_col).character = c;
                self.dirty_region.mark_row(self.cursor_row);
                self.mark_row_dirty(self.cursor_row);
            }
        }
    }

    pub(super) fn put_char(&mut self, ch: char) {
        let _orig_ch = ch;
        let ch = self.translate_char(ch);
        let width = crate::char_width::cached_char_width(ch);
        if width == 0 {
            // Zero-width characters are combining marks. Try to compose the mark
            // onto the previously written cell (NFC). Marks with no precomposed
            // single-codepoint form (e.g. stacked diacritics) are dropped, as the
            // cell can only hold one char.
            self.apply_combining_mark(ch);
            return;
        }

        let cols = self.grid.row_len();
        let blank_cell = self.create_blank_cell();
        let autowrap = self.modes.contains(&7);

        // Resolve a wrap deferred by the previous character (DEC Last Column Flag).
        // The actual line break happens here, when the next printable char arrives.
        if self.pending_wrap {
            self.pending_wrap = false;
            if autowrap {
                self.wrap_to_next_line();
            }
        }

        // A wide character that does not fit in the columns left on this line wraps
        // immediately (it cannot be split across the line boundary).
        if self.cursor_col + width > cols {
            if autowrap {
                self.wrap_to_next_line();
            } else {
                // Autowrap disabled: clamp cursor to last column instead of wrapping
                self.cursor_col = cols.saturating_sub(width);
            }
        }

        // IRM (insert mode, ANSI mode 4): shift existing cells right by `width`
        // before writing, discarding cells pushed past the end of the row.
        if self.insert_mode {
            for _ in 0..width {
                if self.cursor_col < cols {
                    self.grid.insert_cell_in_row(
                        self.cursor_row,
                        self.cursor_col,
                        blank_cell.clone(),
                    );
                }
            }
        }

        // If current position has a continuation cell to its left, clear the wide character
        if self.cursor_col > 0
            && self
                .grid
                .get(self.cursor_row, self.cursor_col)
                .flags
                .wide_continuation()
        {
            *self.grid.get_mut(self.cursor_row, self.cursor_col - 1) = blank_cell.clone();
        }

        // If current position has a wide character, clear its continuation cell
        if self.grid.get(self.cursor_row, self.cursor_col).flags.wide()
            && self.cursor_col + 1 < cols
        {
            *self.grid.get_mut(self.cursor_row, self.cursor_col + 1) = blank_cell.clone();
        }

        // Write character
        let cell = self.grid.get_mut(self.cursor_row, self.cursor_col);
        cell.character = ch;
        cell.foreground = self.current_fg;
        cell.background = self.current_bg;
        cell.flags = self.current_flags;
        cell.flags.set_wide(width == 2);
        cell.flags.set_wide_continuation(false);

        // Set up wide character continuation cell if needed
        if width == 2 && self.cursor_col + 1 < cols {
            let cont_cell = self.grid.get_mut(self.cursor_row, self.cursor_col + 1);
            *cont_cell = blank_cell;
            cont_cell.flags.set_wide_continuation(true);
        }

        self.cursor_col += width;
        // If we just filled the last cell, defer the wrap: keep the cursor on the
        // last column and set the Last Column Flag. The wrap fires on the next char.
        if self.cursor_col >= cols {
            if autowrap {
                self.cursor_col = cols.saturating_sub(width);
                self.pending_wrap = true;
            } else {
                self.cursor_col = cols.saturating_sub(width);
            }
        }
        // Mark the row as dirty after writing character
        self.dirty_region.mark_row(self.cursor_row);
        self.mark_row_dirty(self.cursor_row);
    }

    pub(super) fn put_ascii_run(&mut self, bytes: &[u8]) {
        // Insert mode (IRM) needs per-character right-shifting, which the fast
        // overwrite path below does not do. Fall back to put_char for each byte.
        if self.insert_mode {
            for &byte in bytes {
                self.put_char(byte as char);
            }
            return;
        }

        let cols = self.grid.row_len();
        let autowrap = self.modes.contains(&7);
        let mut pos = 0;

        while pos < bytes.len() {
            // 先结算上一次写入遗留的延迟换行(DEC 末列标志)。
            if self.pending_wrap {
                self.pending_wrap = false;
                if autowrap {
                    self.wrap_to_next_line();
                }
            }

            let remaining = cols - self.cursor_col;
            let chunk_len = (bytes.len() - pos).min(remaining);

            // Write chunk to grid directly through a single row slice
            // (avoids recomputing row*cols + bounds-check on every cell)
            let fg = self.current_fg;
            let bg = self.current_bg;
            let mut flags = self.current_flags;
            flags.set_wide(false);
            flags.set_wide_continuation(false);
            let col = self.cursor_col;
            let row = &mut self.grid[self.cursor_row][col..col + chunk_len];
            for (cell, &byte) in row.iter_mut().zip(&bytes[pos..pos + chunk_len]) {
                cell.character = byte as char;
                cell.foreground = fg;
                cell.background = bg;
                cell.flags = flags;
            }

            self.cursor_col += chunk_len;
            pos += chunk_len;

            self.dirty_region.mark_row(self.cursor_row);
            self.mark_row_dirty(self.cursor_row);

            // 写满末列时不立即换行,改为置延迟换行标志,
            // 光标停在末列,等待下一个可打印字符再决定是否换行。
            if self.cursor_col >= cols {
                self.cursor_col = cols - 1;
                if autowrap {
                    self.pending_wrap = true;
                }
            }
        }
    }

    /// 自动换行时推进到下一行,受 DECSTBM 滚动区底边距约束(与 LF 行为一致)。
    /// 恰在底边距时区域上滚(全屏区会把顶行压入 scrollback);否则在网格内下移光标。
    /// 此前换行只与 grid.rows() 比较并调用全屏 scroll_down(),会让区内文本溢出到
    /// 底边距下方,破坏 pager/分屏 TUI 布局。
    pub(super) fn wrap_to_next_line(&mut self) {
        self.grid.row_wrapped[self.cursor_row] = true;
        self.cursor_col = 0;
        if self.cursor_row == self.scroll_region_bottom {
            self.scroll_region_up(self.scroll_region_top, self.scroll_region_bottom);
        } else if self.cursor_row + 1 < self.grid.rows() {
            self.cursor_row += 1;
        }
    }

    pub(super) fn create_blank_cell(&self) -> TerminalCell {
        TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: self.current_bg, // Preserve current background color
            flags: StyleFlags::default(),
        }
    }

    pub(super) fn blank_line(&self, cols: usize) -> Vec<TerminalCell> {
        vec![self.create_blank_cell(); cols]
    }

    pub(super) fn normalize_line_width(
        &self,
        mut line: Vec<TerminalCell>,
        cols: usize,
    ) -> Vec<TerminalCell> {
        match line.len().cmp(&cols) {
            std::cmp::Ordering::Equal => line,
            std::cmp::Ordering::Greater => {
                line.truncate(cols);
                line
            }
            std::cmp::Ordering::Less => {
                line.resize(cols, self.create_blank_cell());
                line
            }
        }
    }

    pub(super) fn line_is_blank(&self, row: usize) -> bool {
        let blank = self.create_blank_cell();
        self.grid[row].iter().all(|cell| {
            cell.character == blank.character
                && cell.foreground == blank.foreground
                && cell.background == blank.background
                && cell.flags == blank.flags
        })
    }

    pub(super) fn archive_visible_screen_to_scrollback(&mut self) {
        self.archive_visible_screen_to_scrollback_with_options(false, false);
    }

    pub(super) fn visible_screen_snapshot(&self) -> Option<Vec<String>> {
        if self.grid.rows() == 0 {
            return None;
        }

        let first = (0..self.grid.rows()).find(|&row| !self.line_is_blank(row));
        let last = (0..self.grid.rows()).rfind(|&row| !self.line_is_blank(row));
        let (Some(first), Some(last)) = (first, last) else {
            return None;
        };

        Some(
            (first..=last)
                .map(|row| self.grid[row].iter().map(|cell| cell.character).collect())
                .collect(),
        )
    }

    pub(super) fn archive_primary_screen_unless_last_synced_snapshot(&mut self) {
        let Some(snapshot) = self.visible_screen_snapshot() else {
            return;
        };

        if snapshot == self.last_synced_primary_screen_snapshot {
            return;
        }

        self.archive_visible_screen_to_scrollback();
    }

    pub(super) fn archive_visible_screen_to_scrollback_with_options(
        &mut self,
        allow_alt_buffer: bool,
        dedupe_snapshot: bool,
    ) {
        if (self.use_alt_buffer && !allow_alt_buffer) || self.grid.rows() == 0 {
            return;
        }

        let first = (0..self.grid.rows()).find(|&row| !self.line_is_blank(row));
        let last = (0..self.grid.rows()).rfind(|&row| !self.line_is_blank(row));
        let (Some(first), Some(last)) = (first, last) else {
            return;
        };

        if dedupe_snapshot {
            let snapshot = self.visible_screen_snapshot().unwrap_or_default();
            if snapshot == self.last_archived_screen_snapshot {
                return;
            }
            self.last_archived_screen_snapshot = snapshot;
        }

        for row in first..=last {
            let line = ScrollbackLine::compress(&self.grid[row], self.grid.row_wrapped[row]);
            self.push_scrollback_compressed_with_options(line, allow_alt_buffer);
        }
    }

    pub(super) fn push_scrollback_compressed(&mut self, line: ScrollbackLine) {
        self.push_scrollback_compressed_with_options(line, false);
    }

    pub(super) fn push_scrollback_compressed_with_options(
        &mut self,
        line: ScrollbackLine,
        allow_alt_buffer: bool,
    ) {
        if self.use_alt_buffer && !allow_alt_buffer {
            return;
        }
        if self.scrollback.len() >= self.max_scrollback {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(line);
        self.total_lines_scrolled = self.total_lines_scrolled.saturating_add(1);
    }

    pub(super) fn scroll_region_down(&mut self, top: usize, bottom: usize) {
        if top >= self.grid.rows() || bottom >= self.grid.rows() || top > bottom {
            return;
        }
        let cols = self.grid.row_len();
        // Shift rows down: move [top..bottom) to [top+1..=bottom]
        let src_start = top * cols;
        let src_end = bottom * cols;
        let dst = (top + 1) * cols;
        self.grid.cells.copy_within(src_start..src_end, dst);
        // Clear top row(保留当前背景色 / BCE)
        let blank = self.create_blank_cell();
        self.grid.cells[src_start..src_start + cols].fill(blank);
        self.grid.row_wrapped.copy_within(top..bottom, top + 1);
        self.grid.row_wrapped[top] = false;
        self.dirty_region.mark_rows(top, bottom);
        self.mark_rows_dirty(top, bottom);
    }

    pub(super) fn scroll_region_up(&mut self, top: usize, bottom: usize) {
        if top >= self.grid.rows() || bottom >= self.grid.rows() || top > bottom {
            return;
        }

        let cols = self.grid.row_len();
        // VTE saves lines scrolled off the top margin into scrollback whenever
        // the scrolling region starts at the first screen row. The bottom margin
        // may be above the last row so TUIs can keep prompts/status lines fixed
        // while the history area scrolls.
        let scrolls_off_screen_top = top == 0;

        // Compress the removed line directly from the grid slice before mutating,
        // avoiding a per-line Vec allocation from get_row.
        let allow_alt_scrollback = self.use_alt_buffer && self.sync_output_active;
        let scrollback_line =
            if scrolls_off_screen_top && (!self.use_alt_buffer || allow_alt_scrollback) {
                Some(ScrollbackLine::compress(
                    &self.grid[top],
                    self.grid.row_wrapped[top],
                ))
            } else {
                None
            };

        let src_start = (top + 1) * cols;
        let src_end = (bottom + 1) * cols;
        let dst_start = top * cols;
        self.grid.cells.copy_within(src_start..src_end, dst_start);
        let blank_start = bottom * cols;
        // 保留当前背景色 / BCE
        let blank = self.create_blank_cell();
        self.grid.cells[blank_start..blank_start + cols].fill(blank);
        self.grid.row_wrapped.copy_within(top + 1..=bottom, top);
        self.grid.row_wrapped[bottom] = false;

        self.dirty_region.mark_rows(top, bottom);
        self.mark_rows_dirty(top, bottom);

        if let Some(line) = scrollback_line {
            self.push_scrollback_compressed_with_options(line, allow_alt_scrollback);
        }
    }

    pub(super) fn charset_from_designator(byte: u8) -> Charset {
        match byte {
            b'0' => Charset::DecSpecialGraphics,
            _ => Charset::Ascii,
        }
    }

    pub(super) fn translate_char(&self, ch: char) -> char {
        match self.active_charset {
            Charset::Ascii => ch,
            Charset::DecSpecialGraphics => match ch {
                '`' => '◆',
                'a' => '▒',
                'f' => '°',
                'g' => '±',
                'j' => '┘',
                'k' => '┐',
                'l' => '┌',
                'm' => '└',
                'n' => '┼',
                'o' => '⎺',
                'p' => '⎻',
                'q' => '─',
                'r' => '⎼',
                's' => '⎽',
                't' => '├',
                'u' => '┤',
                'v' => '┴',
                'w' => '┬',
                'x' => '│',
                'y' => '≤',
                'z' => '≥',
                '{' => 'π',
                '|' => '≠',
                '}' => '£',
                '~' => '·',
                _ => ch,
            },
        }
    }

    pub(super) fn clear_cell(&mut self, row: usize, col: usize) {
        let cols = self.grid.row_len();
        let bg_color = self.current_bg;
        let blank_cell = TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: bg_color,
            flags: StyleFlags::default(),
        };
        // If clearing a continuation cell, also clear the wide character body
        if self.grid.get(row, col).flags.wide_continuation() && col > 0 {
            *self.grid.get_mut(row, col - 1) = blank_cell.clone();
        }
        // If clearing a wide character body, also clear the continuation cell
        if self.grid.get(row, col).flags.wide() && col + 1 < cols {
            *self.grid.get_mut(row, col + 1) = blank_cell.clone();
        }
        *self.grid.get_mut(row, col) = blank_cell;
    }

    /// P3 优化：批量处理输入数据，只在处理完成后触发一次网格版本更新
    /// 相比多次 process_input，这个方法避免了多次网格版本递增
    pub fn process_batch(&mut self, input: &[u8]) {
        self.grid_version = self.grid_version.wrapping_add(1);
        self.process_input(input);
    }

    #[inline]
    pub(super) fn mark_row_dirty(&mut self, row: usize) {
        if row < self.row_versions.len() {
            self.row_versions[row] = self.grid_version;
        }
    }

    #[inline]
    pub(super) fn mark_rows_dirty(&mut self, start: usize, end: usize) {
        for row in start..=end.min(self.row_versions.len().saturating_sub(1)) {
            self.row_versions[row] = self.grid_version;
        }
    }

    /// P4：获取上次渲染后修改过的行索引
    pub fn get_dirty_rows(&self, last_rendered_version: u64, out: &mut Vec<usize>) {
        out.clear();
        for (i, &v) in self.row_versions.iter().enumerate() {
            if v > last_rendered_version {
                out.push(i);
            }
        }
    }

    /// P4：获取网格版本号（用于缓存比较）
    pub fn get_grid_version(&self) -> u64 {
        self.grid_version
    }

    pub fn take_osc52_clipboard_set(&mut self) -> Option<String> {
        self.pending_osc52_clipboard_set.take()
    }

    pub fn take_osc52_clipboard_query(&mut self) -> bool {
        let q = self.pending_osc52_clipboard_query;
        self.pending_osc52_clipboard_query = false;
        q
    }

    pub fn respond_osc52_clipboard(&mut self, content: &str) {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
        self.output_buffer.extend_from_slice(b"\x1b]52;c;");
        self.output_buffer.extend_from_slice(encoded.as_bytes());
        self.output_buffer.extend_from_slice(Self::osc_terminator());
    }

    /// Check if sync output timed out (>1s) and auto-clear if so
    pub fn check_sync_output_timeout(&mut self) {
        if self.sync_output_active {
            if let Some(start) = self.sync_output_start {
                if start.elapsed() > std::time::Duration::from_secs(1) {
                    if self.use_alt_buffer {
                        self.archive_visible_screen_to_scrollback_with_options(true, true);
                    } else {
                        self.last_synced_primary_screen_snapshot =
                            self.visible_screen_snapshot().unwrap_or_default();
                    }
                    self.sync_output_active = false;
                    self.sync_output_start = None;
                    self.modes.remove(&2026);
                    self.dirty_region.mark_all(self.grid.rows());
                    self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
                }
            }
        }
    }

    /// 处理一个 UTF-8 多字节引导字节。`expected` 是该序列总长度(2/3/4)。
    /// - 缓冲区剩余字节不足:把引导字节暂存,等待下一批输入续接(跨 PTY 读边界)。
    /// - 续接字节齐全且合法:解码并写入字符;非法则输出替换字符 U+FFFD。
    /// - 续接字节非法(不是 10xxxxxx):序列残缺,输出 U+FFFD 且只消费引导字节本身,
    ///   让那个意外字节按自身规则重新处理。
    pub(super) fn consume_utf8_lead(&mut self, byte: u8, expected: u8, data: &[u8], i: &mut usize) {
        let need = expected as usize;
        if *i + need > data.len() {
            // 不完整:剩余字节不够,暂存等待下一批
            self.utf8_buf[0] = byte;
            self.utf8_len = 1;
            self.utf8_expected = expected;
            *i += 1;
            return;
        }

        let all_continuation = (1..need).all(|k| (data[*i + k] & 0xC0) == 0x80);
        if all_continuation {
            match std::str::from_utf8(&data[*i..*i + need]) {
                Ok(s) => {
                    if let Some(ch) = s.chars().next() {
                        self.put_char(ch);
                    }
                }
                Err(_) => self.put_char('\u{FFFD}'),
            }
            *i += need;
        } else {
            // 引导字节后紧跟非续接字节:残缺序列
            self.put_char('\u{FFFD}');
            *i += 1;
        }
    }
}

impl super::TerminalState {
    pub fn max_scrollback(&self) -> usize {
        self.max_scrollback
    }

    /// 当前 scrollback 已有的行数(滚动上界)。
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// 当前是否处于备用屏幕缓冲(此时滚轮不滚动 scrollback)。
    pub fn is_alt_buffer(&self) -> bool {
        self.use_alt_buffer
    }

    pub fn set_max_scrollback(&mut self, max_scrollback: usize) {
        self.max_scrollback = max_scrollback.max(1);

        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        }

        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
    }

    pub fn is_cursor_visible(&self) -> bool {
        // Cursor is visible when mode 25 is SET (via \x1b[?25h)
        // Hidden when mode 25 is RESET (via \x1b[?25l)
        // While viewing scrollback we intentionally hide the live cursor,
        // because the viewport no longer tracks the active prompt line.
        self.modes.contains(&25) && self.scroll_offset == 0
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Current line id of the row the cursor is on. `line_id` is monotonic
    /// across the session and survives scrollback eviction; it can be
    /// translated back to a viewport/scrollback index via
    /// [`Self::line_id_to_scrollback_index`].
    fn current_cursor_line_id(&self) -> u64 {
        self.total_lines_scrolled
            .saturating_add(self.cursor_row as u64)
    }

    /// Translate a recorded `line_id` to its current `scrollback` index, or
    /// `None` if the line has been evicted (or now lives in the live grid,
    /// which means it's already on screen).
    fn line_id_to_scrollback_index(&self, line_id: u64) -> Option<usize> {
        if line_id >= self.total_lines_scrolled {
            // Line is either in the live grid (>= total_lines_scrolled) or
            // hasn't happened yet (impossible via this API). The grid is
            // already on screen, so caller can scroll to bottom.
            return None;
        }
        let first_scrollback_line_id = self
            .total_lines_scrolled
            .saturating_sub(self.scrollback.len() as u64);
        if line_id < first_scrollback_line_id {
            // Evicted from scrollback.
            return None;
        }
        Some((line_id - first_scrollback_line_id) as usize)
    }

    /// Drop marks that point to lines no longer in scrollback. Called
    /// lazily before navigation rather than on every scrollback push.
    fn prune_evicted_marks(&mut self) {
        let first_scrollback_line_id = self
            .total_lines_scrolled
            .saturating_sub(self.scrollback.len() as u64);
        while self
            .command_marks
            .front()
            .map(|m| m.line_id < first_scrollback_line_id)
            .unwrap_or(false)
        {
            self.command_marks.pop_front();
        }
    }

    /// Record an OSC 133;A boundary at the current cursor row. Called from
    /// the parser when a FinalTerm-aware shell emits the prompt-start mark.
    pub(super) fn record_prompt_start(&mut self) {
        // Bypass the alt buffer entirely (less / vim emit no marks; if they
        // did, they'd contaminate the main-screen history).
        if self.use_alt_buffer {
            return;
        }
        let line_id = self.current_cursor_line_id();

        // Coalesce: if the most recent mark is on the same row (e.g., shell
        // sent A twice for the same prompt) just keep the latest.
        if let Some(last) = self.command_marks.back_mut() {
            if last.line_id == line_id {
                last.exit_code = None;
                return;
            }
        }

        if self.command_marks.len() >= MAX_COMMAND_MARKS {
            self.command_marks.pop_front();
        }
        self.command_marks.push_back(CommandMark {
            line_id,
            exit_code: None,
        });
    }

    /// Attach an exit code to the most recently recorded prompt mark.
    /// Called for OSC 133;D[;<code>].
    pub(super) fn record_command_exit(&mut self, exit_code: Option<i32>) {
        if let Some(last) = self.command_marks.back_mut() {
            last.exit_code = exit_code;
        }
    }

    /// Scroll the viewport so the row at `line_id` lands at the top of
    /// the visible area (or as close as possible). Returns true if the
    /// jump did anything.
    fn scroll_to_line_id(&mut self, line_id: u64) -> bool {
        if let Some(scrollback_idx) = self.line_id_to_scrollback_index(line_id) {
            // Target a scrollback row; scroll_offset = scrollback.len() - idx
            // puts that row at the top of the viewport.
            let new_offset = self.scrollback.len().saturating_sub(scrollback_idx);
            self.scroll_offset = new_offset.min(self.scrollback.len());
            true
        } else if line_id >= self.total_lines_scrolled {
            // Already in the live grid; just snap to the bottom.
            self.scroll_offset = 0;
            true
        } else {
            false
        }
    }

    /// Move the viewport to the prompt mark immediately before the
    /// currently-visible top row. Returns true on a successful jump.
    pub fn jump_to_prev_command(&mut self) -> bool {
        if self.use_alt_buffer {
            return false;
        }
        self.prune_evicted_marks();

        // The "current top" line id of the viewport.
        let top_line_id = self
            .total_lines_scrolled
            .saturating_sub(self.scroll_offset as u64);

        // Find the latest mark strictly before the current top.
        let target = self
            .command_marks
            .iter()
            .rev()
            .find(|m| m.line_id < top_line_id)
            .copied();

        match target {
            Some(mark) => self.scroll_to_line_id(mark.line_id),
            None => false,
        }
    }

    /// Move the viewport to the next prompt mark after the currently-visible
    /// top row. Returns true on a successful jump.
    pub fn jump_to_next_command(&mut self) -> bool {
        if self.use_alt_buffer {
            return false;
        }
        self.prune_evicted_marks();

        let top_line_id = self
            .total_lines_scrolled
            .saturating_sub(self.scroll_offset as u64);

        let target = self
            .command_marks
            .iter()
            .find(|m| m.line_id > top_line_id)
            .copied();

        match target {
            Some(mark) => self.scroll_to_line_id(mark.line_id),
            None => {
                // No further mark; if we were scrolled up, snap to live view.
                if self.scroll_offset != 0 {
                    self.scroll_offset = 0;
                    true
                } else {
                    false
                }
            }
        }
    }

    pub fn get_mouse_report(&self, button: u8, col: usize, row: usize) -> Option<String> {
        // Check if any mouse reporting mode is enabled
        if !self.modes.contains(&1000) && !self.modes.contains(&1002) && !self.modes.contains(&1003)
        {
            return None;
        }

        // SGR format (mode 1006) is preferred: CSI < button ; col ; row M/m
        // Standard format (mode 1000/1002): CSI M button col row (3 bytes)

        if self.modes.contains(&1006) {
            // SGR format: CSI < button ; x ; y M (button press) or m (button release)
            // For now, we'll generate press events (M) - release tracking would need more state
            let x = (col as u32 + 1).min(255); // 1-indexed, max 255
            let y = (row as u32 + 1).min(255); // 1-indexed, max 255
            Some(format!("\x1b[<{};{};{}M", button, x, y))
        } else {
            // Standard xterm format: CSI M button col row (raw bytes)
            // Col and row are offset by 32 (space character)
            let button_byte = 32 + button;
            let col_byte = 32 + (col as u8).min(223);
            let row_byte = 32 + (row as u8).min(223);
            Some(format!(
                "\x1b[M{}{}{}",
                button_byte as char, col_byte as char, row_byte as char
            ))
        }
    }

    pub fn get_mouse_release_report(&self, button: u8, col: usize, row: usize) -> Option<String> {
        if !self.modes.contains(&1000) && !self.modes.contains(&1002) && !self.modes.contains(&1003)
        {
            return None;
        }

        if self.modes.contains(&1006) {
            // SGR format: lowercase 'm' for release
            let x = (col as u32 + 1).min(255);
            let y = (row as u32 + 1).min(255);
            Some(format!("\x1b[<{};{};{}m", button, x, y))
        } else {
            // Standard xterm: release is button 3
            let button_byte = 32 + 3u8;
            let col_byte = 32 + (col as u8).min(223);
            let row_byte = 32 + (row as u8).min(223);
            Some(format!(
                "\x1b[M{}{}{}",
                button_byte as char, col_byte as char, row_byte as char
            ))
        }
    }

    pub fn is_mouse_enabled(&self) -> bool {
        self.modes.contains(&1000) || self.modes.contains(&1002) || self.modes.contains(&1003)
    }

    pub fn is_alt_buffer_active(&self) -> bool {
        self.use_alt_buffer
    }

    pub fn is_bracketed_paste_enabled(&self) -> bool {
        self.modes.contains(&2004)
    }

    pub fn is_application_cursor_keys(&self) -> bool {
        self.modes.contains(&1)
    }

    pub fn is_paste_events_enabled(&self) -> bool {
        self.modes.contains(&5522)
    }

    pub fn keyboard_enhancement_flags(&self) -> u16 {
        self.keyboard_enhancement_flags
    }

    pub fn xterm_modify_other_keys(&self) -> u16 {
        self.xterm_modify_other_keys
    }

    pub fn xterm_format_other_keys(&self) -> u16 {
        self.xterm_format_other_keys
    }

    pub fn is_report_all_keys_enabled(&self) -> bool {
        self.modes.contains(&2031) || (self.keyboard_enhancement_flags & 0b1000) != 0
    }

    pub fn build_paste_event(&mut self, mime_types: &[String]) -> Vec<u8> {
        let password = uuid::Uuid::new_v4().to_string();
        self.pending_paste_password = Some(password.clone());
        let encoded_password =
            base64::engine::general_purpose::STANDARD.encode(password.as_bytes());
        let mut output = Vec::new();

        output.extend_from_slice(b"\x1b]5522;type=read:status=OK:password=");
        output.extend_from_slice(encoded_password.as_bytes());
        output.extend_from_slice(Self::osc_terminator());

        for mime_type in mime_types {
            let encoded_mime =
                base64::engine::general_purpose::STANDARD.encode(mime_type.as_bytes());
            output.extend_from_slice(b"\x1b]5522;type=read:status=DATA:mime=");
            output.extend_from_slice(encoded_mime.as_bytes());
            output.extend_from_slice(Self::osc_terminator());
        }

        output.extend_from_slice(b"\x1b]5522;type=read:status=DONE\x1b\\");
        output
    }

    pub fn take_clipboard_read_requests(&mut self) -> Vec<ClipboardReadRequest> {
        std::mem::take(&mut self.pending_clipboard_requests)
    }

    pub fn get_visible_cells(&mut self) -> std::sync::Arc<Vec<Vec<TerminalCell>>> {
        if let Some((cached_version, cached_offset, ref cells)) = self.visible_cells_cache {
            if cached_version == self.grid_version && cached_offset == self.scroll_offset {
                return std::sync::Arc::clone(cells);
            }
        }

        // Cache miss - rebuild
        let rows = self.grid.rows();
        let cols = if rows > 0 { self.grid.row_len() } else { 80 };

        // Try to recycle the previous allocation. The renderer drops its returned
        // Arc each frame, so by the next miss we are usually the sole owner and can
        // refill the existing nested Vecs in place instead of reallocating per row.
        let prev = self.visible_cells_cache.take();
        let prev_version = prev.as_ref().map(|(v, _, _)| *v);
        let prev_offset = prev.as_ref().map(|(_, o, _)| *o);
        let mut recycled = prev.map(|(_, _, a)| a);

        if self.scroll_offset == 0 {
            // Fast path: copy current grid, reusing inner Vec capacity when possible.
            if let Some(mut arc) = recycled.take() {
                if let Some(buf) = std::sync::Arc::get_mut(&mut arc) {
                    // Incremental path: if the recycled buffer already holds a same-sized
                    // snapshot taken at scroll_offset==0, only re-copy rows whose
                    // row_versions changed since that snapshot. Untouched rows already
                    // hold valid data, turning an O(rows*cols) copy into O(dirty cells).
                    let can_incremental = prev_offset == Some(0)
                        && buf.len() == rows
                        && buf.iter().all(|r| r.len() == cols);
                    if can_incremental {
                        let base = prev_version.unwrap_or(0);
                        for (r, (dst, chunk)) in buf.iter_mut().zip(self.grid.iter()).enumerate() {
                            if self.row_versions[r] > base {
                                dst.clear();
                                dst.extend_from_slice(chunk);
                            }
                        }
                    } else {
                        buf.resize_with(rows, Vec::new);
                        for (dst, chunk) in buf.iter_mut().zip(self.grid.iter()) {
                            dst.clear();
                            dst.extend_from_slice(chunk);
                        }
                    }
                    self.visible_cells_cache = Some((
                        self.grid_version,
                        self.scroll_offset,
                        std::sync::Arc::clone(&arc),
                    ));
                    return arc;
                }
                // 仍被他处共享,无法原地复用;放回供下方 fallback 分支重建。
                recycled = Some(arc);
            }
        }

        let cells = if self.scroll_offset == 0 {
            // Fast path (shared allocation): fresh copy of current grid.
            self.grid.to_vec()
        } else {
            // Slow path: reflow scrollback
            let blank_cell = self.create_blank_cell();

            let mut start_idx = self
                .scrollback
                .len()
                .saturating_sub(self.scroll_offset + rows);
            while start_idx > 0 && self.scrollback[start_idx - 1].is_wrapped {
                start_idx -= 1;
            }
            let end_idx = self.scrollback.len();
            let to_reflow: Vec<ScrollbackLine> = self
                .scrollback
                .iter()
                .skip(start_idx)
                .take(end_idx - start_idx)
                .cloned()
                .collect();

            let reflowed = Self::reflow_lines(&to_reflow, cols, &blank_cell);
            let skip = reflowed.len().saturating_sub(self.scroll_offset + rows);
            let visible_start = skip + (reflowed.len() - skip).saturating_sub(self.scroll_offset);
            let mut result: Vec<Vec<TerminalCell>> = reflowed[visible_start..]
                .iter()
                .map(|l| l.decompress())
                .collect();

            if result.len() > rows {
                result.truncate(rows);
            }

            for row in self.grid.iter() {
                if result.len() < rows {
                    result.push(self.normalize_line_width(row.to_vec(), cols));
                } else {
                    break;
                }
            }

            while result.len() < rows {
                result.push(self.blank_line(cols));
            }

            result
        };

        // Reuse the recycled Arc's outer allocation if we still solely own it.
        let arc = match recycled.take() {
            Some(mut arc) => match std::sync::Arc::get_mut(&mut arc) {
                Some(buf) => {
                    *buf = cells;
                    arc
                }
                None => std::sync::Arc::new(cells),
            },
            None => std::sync::Arc::new(cells),
        };
        self.visible_cells_cache = Some((
            self.grid_version,
            self.scroll_offset,
            std::sync::Arc::clone(&arc),
        ));
        arc
    }

    pub fn get_cursor_pos(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    /// 获取当前可见行的wrapped状态，用于跨行链接检测
    pub fn get_visible_row_wrapped(&self) -> Vec<bool> {
        let rows = self.grid.rows();

        if self.scroll_offset == 0 {
            // Fast path: just get current grid wrapped flags
            self.grid.row_wrapped.clone()
        } else {
            // Slow path: need to reconstruct from scrollback
            // For simplicity, when scrolling we disable wrapped link detection
            // by returning all false (can be improved later with full reflow)
            vec![false; rows]
        }
    }

    pub fn get_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output_buffer)
    }

    #[inline]
    pub(super) fn viewport_row_to_absolute(&self, viewport_row: usize) -> usize {
        self.scrollback.len().saturating_sub(self.scroll_offset) + viewport_row
    }

    /// Start a new selection at a viewport-relative position.
    /// Converts to absolute buffer coordinates internally.
    pub fn start_selection(&mut self, viewport_pos: (usize, usize)) {
        self.start_selection_with_mode(viewport_pos, SelectionMode::Normal);
    }

    pub fn start_block_selection(&mut self, viewport_pos: (usize, usize)) {
        self.start_selection_with_mode(viewport_pos, SelectionMode::Block);
    }

    pub(super) fn start_selection_with_mode(
        &mut self,
        viewport_pos: (usize, usize),
        mode: SelectionMode,
    ) {
        let abs = (
            self.viewport_row_to_absolute(viewport_pos.0),
            viewport_pos.1,
        );
        self.selection = Some(Selection {
            anchor: abs,
            active: abs,
            mode,
        });
    }

    /// Update the active end of the current selection with a viewport-relative position.
    pub fn update_selection(&mut self, viewport_pos: (usize, usize)) {
        let abs_row = self.viewport_row_to_absolute(viewport_pos.0);
        if let Some(ref mut sel) = self.selection {
            sel.active = (abs_row, viewport_pos.1);
        }
    }

    /// Select the word at the given (row, col) position in the visible grid.
    /// Word boundaries are determined by character class: alphanumeric/underscore,
    /// whitespace, or punctuation/symbols.
    pub fn select_word_at(&mut self, row: usize, col: usize) {
        let visible = self.get_visible_cells();
        if row >= visible.len() {
            return;
        }
        let line = &visible[row];
        let cols = line.len();
        if col >= cols {
            return;
        }

        // Skip wide_continuation to find the real character
        let mut start_col = col;
        if line[start_col].flags.wide_continuation() && start_col > 0 {
            start_col -= 1;
        }

        if let Some((left, right)) = Self::select_extended_token_span(line, start_col) {
            let abs_row = self.viewport_row_to_absolute(row);
            self.selection = Some(Selection {
                anchor: (abs_row, left),
                active: (abs_row, right),
                mode: SelectionMode::Normal,
            });
            return;
        }

        let ch = line[start_col].character;
        let class = char_class(ch);

        // Expand left
        let mut left = start_col;
        while left > 0 {
            let prev = left - 1;
            let c = line[prev].character;
            if line[prev].flags.wide_continuation() {
                left = prev;
                continue;
            }
            if char_class(c) != class {
                break;
            }
            left = prev;
        }

        // Expand right
        let mut right = start_col;
        loop {
            let next = if line[right].flags.wide() {
                right + 2
            } else {
                right + 1
            };
            if next >= cols {
                break;
            }
            if line[next].flags.wide_continuation() {
                // shouldn't happen after a non-wide char, but skip
                if next + 1 < cols {
                    if char_class(line[next + 1].character) != class {
                        break;
                    }
                    right = next + 1;
                    continue;
                }
                break;
            }
            if char_class(line[next].character) != class {
                break;
            }
            right = next;
        }
        // If the selected end is a wide char, include its continuation cell
        if line[right].flags.wide() && right + 1 < cols {
            right += 1;
        }

        let abs_row = self.viewport_row_to_absolute(row);
        self.selection = Some(Selection {
            anchor: (abs_row, left),
            active: (abs_row, right),
            mode: SelectionMode::Normal,
        });
    }

    pub fn select_line_at(&mut self, row: usize) {
        let visible = self.get_visible_cells();
        if row >= visible.len() {
            return;
        }

        let line = &visible[row];
        let mut right = line.len().saturating_sub(1);
        while right > 0 {
            let cell = &line[right];
            if !cell.flags.wide_continuation() && cell.character != ' ' {
                break;
            }
            right -= 1;
        }

        if line
            .get(right)
            .is_some_and(|cell| cell.flags.wide() && right + 1 < line.len())
        {
            right += 1;
        }

        let abs_row = self.viewport_row_to_absolute(row);
        self.selection = Some(Selection {
            anchor: (abs_row, 0),
            active: (abs_row, right),
            mode: SelectionMode::Normal,
        });
    }

    pub(super) fn select_extended_token_span(
        line: &[TerminalCell],
        start_col: usize,
    ) -> Option<(usize, usize)> {
        let cols = line.len();
        if start_col >= cols {
            return None;
        }

        let start_char = line[start_col].character;
        if !is_extended_token_char(start_char) {
            return None;
        }

        let mut left = start_col;
        while left > 0 {
            let prev = left - 1;
            if line[prev].flags.wide_continuation() {
                left = prev;
                continue;
            }
            if !is_extended_token_char(line[prev].character) {
                break;
            }
            left = prev;
        }

        let mut right = start_col;
        loop {
            let next = if line[right].flags.wide() {
                right + 2
            } else {
                right + 1
            };
            if next >= cols {
                break;
            }
            if line[next].flags.wide_continuation() {
                if next + 1 < cols && is_extended_token_char(line[next + 1].character) {
                    right = next + 1;
                    continue;
                }
                break;
            }
            if !is_extended_token_char(line[next].character) {
                break;
            }
            right = next;
        }

        while left < start_col && is_token_prefix_wrapper(line[left].character) {
            left += 1;
        }

        while right > start_col && is_token_suffix_wrapper(line[right].character) {
            right -= if line[right].flags.wide_continuation() && right > 0 {
                2
            } else {
                1
            };
        }

        if left > right || start_col < left || start_col > right {
            return None;
        }

        let mut has_alnum = false;
        let mut has_separator = false;
        for cell in &line[left..=right] {
            if cell.flags.wide_continuation() {
                continue;
            }
            let ch = cell.character;
            has_alnum |= ch.is_alphanumeric();
            has_separator |= is_extended_token_separator(ch);
        }

        if !has_alnum || !has_separator {
            return None;
        }

        if line[right].flags.wide() && right + 1 < cols {
            right += 1;
        }

        Some((left, right))
    }

    pub fn copy_selection(&self) -> Option<String> {
        self.selection.map(|sel| {
            let (start, end) = if sel.anchor <= sel.active {
                (sel.anchor, sel.active)
            } else {
                (sel.active, sel.anchor)
            };
            let mut result = String::new();
            let scrollback_len = self.scrollback.len();
            let grid_rows = self.grid.rows();
            let cols = self.grid.row_len();
            let total_rows = scrollback_len + grid_rows;

            for abs_row in start.0..=end.0.min(total_rows.saturating_sub(1)) {
                let start_col = if abs_row == start.0 { start.1 } else { 0 };
                let end_col = if abs_row == end.0 {
                    end.1.min(cols.saturating_sub(1))
                } else {
                    cols.saturating_sub(1)
                };

                // 行是否因到达行末被自动换行(软换行)。复制时软换行不应插入 \n,
                // 否则像 URL 这种被终端宽度截断的字符串会被切断成多段。
                let row_wrapped = if abs_row < scrollback_len {
                    self.scrollback[abs_row].is_wrapped
                } else {
                    let grid_row = abs_row - scrollback_len;
                    self.grid
                        .row_wrapped
                        .get(grid_row)
                        .copied()
                        .unwrap_or(false)
                };

                let mut line_buf = String::new();
                if abs_row < scrollback_len {
                    // Read from scrollback
                    let line = self.scrollback[abs_row].decompress();
                    for col in start_col..=end_col.min(line.len().saturating_sub(1)) {
                        if !line[col].flags.wide_continuation() {
                            line_buf.push(line[col].character);
                        }
                    }
                } else {
                    // Read from current grid
                    let grid_row = abs_row - scrollback_len;
                    if grid_row < grid_rows {
                        for col in start_col..=end_col {
                            let cell = self.grid.get(grid_row, col);
                            if !cell.flags.wide_continuation() {
                                line_buf.push(cell.character);
                            }
                        }
                    }
                }

                // 软换行(URL 等被终端宽度截断)拼接时去掉尾部填充空白,
                // 避免还原后的字符串里夹杂大段空格。
                if row_wrapped && abs_row < end.0 {
                    let trimmed_len = line_buf.trim_end_matches(' ').len();
                    line_buf.truncate(trimmed_len);
                }

                result.push_str(&line_buf);

                if abs_row < end.0 && !row_wrapped {
                    result.push('\n');
                }
            }

            result
        })
    }

    pub fn scroll(&mut self, lines: isize) {
        // Don't scroll ordinary alternate-screen apps (less, vim, git log, etc.).
        // Synchronized TUIs such as Codex may archive snapshots into local
        // scrollback, in which case wheel/scrollbar navigation should work.
        if self.use_alt_buffer && self.scrollback.is_empty() {
            return;
        }

        if lines > 0 {
            // Scroll up (show earlier lines)
            self.scroll_offset = self.scroll_offset.saturating_add(lines as usize);
        } else {
            // Scroll down (show later lines)
            self.scroll_offset = self.scroll_offset.saturating_sub((-lines) as usize);
        }

        // Clamp scroll_offset to valid range
        let max_scroll = self.scrollback.len();
        self.scroll_offset = self.scroll_offset.min(max_scroll);

        // When scrolling to bottom (offset 0), reset to live view
        if self.scroll_offset == 0 {
            self.scroll_offset = 0;
        }
    }

    pub(super) fn strip_trailing_blanks(cells: &[TerminalCell]) -> &[TerminalCell] {
        let mut end = cells.len();
        while end > 0
            && cells[end - 1].character == ' '
            && cells[end - 1].background == Color::Default
            && !cells[end - 1].flags.wide()
            && !cells[end - 1].flags.wide_continuation()
        {
            end -= 1;
        }
        &cells[..end]
    }

    pub(super) fn reflow_lines(
        lines: &[ScrollbackLine],
        new_cols: usize,
        blank_cell: &TerminalCell,
    ) -> Vec<ScrollbackLine> {
        let mut result = Vec::new();
        let len = lines.len();
        let mut i = 0;

        while i < len {
            let mut logical_line: Vec<TerminalCell> = Vec::new();
            let decompressed = lines[i].decompress();
            logical_line.extend_from_slice(Self::strip_trailing_blanks(&decompressed));
            while i < len && lines[i].is_wrapped {
                i += 1;
                if i < len {
                    let dc = lines[i].decompress();
                    logical_line.extend_from_slice(Self::strip_trailing_blanks(&dc));
                }
            }
            i += 1;

            if logical_line.is_empty() {
                result.push(ScrollbackLine::compress(
                    &vec![blank_cell.clone(); new_cols],
                    false,
                ));
                continue;
            }

            let chunks: Vec<&[TerminalCell]> = logical_line.chunks(new_cols).collect();
            let num_chunks = chunks.len();
            for (ci, chunk) in chunks.into_iter().enumerate() {
                if chunk.len() == new_cols {
                    result.push(ScrollbackLine::compress(chunk, ci + 1 < num_chunks));
                } else {
                    let mut cells = chunk.to_vec();
                    cells.resize(new_cols, blank_cell.clone());
                    result.push(ScrollbackLine::compress(&cells, ci + 1 < num_chunks));
                }
            }
        }

        result
    }

    pub fn on_resize(&mut self, cols: usize, rows: usize) {
        if cols == 0 || rows == 0 {
            return;
        }

        let (cols, rows) = clamp_terminal_dimensions(cols, rows);

        let old_rows = self.grid.rows();
        let had_full_screen_region = old_rows == 0
            || (self.scroll_region_top == 0 && self.scroll_region_bottom + 1 >= old_rows);

        let blank_cell = self.create_blank_cell();

        // 缩小高度时,grid.resize 默认保留顶部行、丢弃底部(含光标行与近期输出),
        // 导致缩小窗口丢失最新输出。改为把顶部溢出行压入 scrollback 并将内容上移,
        // 尽量保留底部内容并保持光标可见(与 xterm/kitty 一致)。仅处理主屏 ——
        // 备用屏应用会在 SIGWINCH 后自行重绘,无需保留。
        if rows < old_rows && !self.use_alt_buffer && !self.grid.is_empty() {
            let need = old_rows - rows;
            // 最多从顶部移除到光标所在行,避免把光标行本身推入 scrollback;
            // 剩余需移除的行位于光标下方,由随后的 grid.resize 截断(通常为空白)。
            let from_top = need.min(self.cursor_row);
            if from_top > 0 {
                for r in 0..from_top {
                    let line = ScrollbackLine::compress(&self.grid[r], self.grid.row_wrapped[r]);
                    self.push_scrollback_compressed(line);
                }
                self.grid.scroll_up_by(from_top, blank_cell.clone());
                self.cursor_row -= from_top;
            }
        }

        self.grid.resize(rows, cols, blank_cell.clone());
        self.alt_grid.resize(rows, cols, blank_cell.clone());

        // CRITICAL: Sync row_versions size with grid size to prevent dirty mark loss
        // When grid grows, we need to extend row_versions; when it shrinks, truncate it
        if rows != self.row_versions.len() {
            self.row_versions.resize(rows, self.grid_version);
        }

        self.scroll_offset = 0;
        self.pending_wrap = false;
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.saved_cursor_row = self.saved_cursor_row.min(rows.saturating_sub(1));
        self.saved_cursor_col = self.saved_cursor_col.min(cols.saturating_sub(1));

        // Resize tab stops: keep existing stops, default new columns to every 8th.
        if cols != self.tab_stops.len() {
            let old_len = self.tab_stops.len();
            self.tab_stops.resize(cols, false);
            for c in old_len..cols {
                self.tab_stops[c] = c % 8 == 0;
            }
        }

        // Clamp saved cursor state (DECSC/CSI s) to new bounds.
        if let Some(s) = self.saved_state.as_mut() {
            s.row = s.row.min(rows.saturating_sub(1));
            s.col = s.col.min(cols.saturating_sub(1));
        }
        self.alt_cursor_row = self.alt_cursor_row.min(rows.saturating_sub(1));
        self.alt_cursor_col = self.alt_cursor_col.min(cols.saturating_sub(1));
        if had_full_screen_region {
            self.scroll_region_top = 0;
            self.scroll_region_bottom = rows.saturating_sub(1);
        } else {
            self.scroll_region_top = self.scroll_region_top.min(rows.saturating_sub(1));
            self.scroll_region_bottom = self.scroll_region_bottom.min(rows.saturating_sub(1));

            if self.scroll_region_top > self.scroll_region_bottom {
                self.scroll_region_top = 0;
                self.scroll_region_bottom = rows.saturating_sub(1);
            }
        }
    }

    pub fn get_dimensions(&self) -> (usize, usize) {
        if self.grid.is_empty() {
            (0, 0)
        } else {
            (self.grid.row_len(), self.grid.rows())
        }
    }

    #[inline]
    pub fn row_selection_cols(&self, viewport_row: usize) -> Option<(usize, usize)> {
        let sel = self.selection?;
        let abs_row = self.viewport_row_to_absolute(viewport_row);
        let (start, end) = if sel.anchor <= sel.active {
            (sel.anchor, sel.active)
        } else {
            (sel.active, sel.anchor)
        };

        if abs_row < start.0 || abs_row > end.0 {
            return None;
        }

        match sel.mode {
            SelectionMode::Block => {
                let col_min = sel.anchor.1.min(sel.active.1);
                let col_max = sel.anchor.1.max(sel.active.1);
                Some((col_min, col_max))
            }
            SelectionMode::Normal => {
                let col_start = if abs_row == start.0 { start.1 } else { 0 };
                let col_end = if abs_row == end.0 { end.1 } else { usize::MAX };
                Some((col_start, col_end))
            }
        }
    }

    // IME support methods
    pub fn set_preedit(&mut self, text: String, cursor: usize) {
        self.preedit_text = text;
        self.preedit_cursor = cursor;
    }

    pub fn clear_preedit(&mut self) {
        self.preedit_text.clear();
        self.preedit_cursor = 0;
    }
}
