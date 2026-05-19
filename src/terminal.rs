use crate::kitty_graphics::KittyGraphicsState;
use base64::Engine;
use std::collections::VecDeque;

/// Character class for word selection boundaries.
#[derive(PartialEq)]
enum CharClass {
    Word,
    Whitespace,
    Symbol,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_whitespace_char(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\0'
}

fn char_class(c: char) -> CharClass {
    if is_word_char(c) {
        CharClass::Word
    } else if is_whitespace_char(c) {
        CharClass::Whitespace
    } else {
        CharClass::Symbol
    }
}

fn is_extended_token_separator(c: char) -> bool {
    matches!(
        c,
        '/' | '\\' | '.' | ':' | '-' | '~' | '?' | '&' | '=' | '#' | '%' | '+' | '@'
    )
}

fn is_extended_token_char(c: char) -> bool {
    is_word_char(c) || is_extended_token_separator(c)
}

fn is_token_prefix_wrapper(c: char) -> bool {
    matches!(c, '"' | '\'' | '`' | '(' | '[' | '{' | '<')
}

fn is_token_suffix_wrapper(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | '`' | ')' | ']' | '}' | '>' | ',' | ';' | '!'
    )
}

const PRIMARY_DEVICE_ATTRIBUTES_RESPONSE: &[u8] = b"\x1b[?65;1;9c";
const SECONDARY_DEVICE_ATTRIBUTES_RESPONSE: &[u8] = b"\x1b[>1;7802;0c";
const XTERM_VERSION_RESPONSE: &[u8] = b"\x1bP>|VTE(7802)\x1b\\";
pub const MAX_TERMINAL_COLS: usize = 1024;
pub const MAX_TERMINAL_ROWS: usize = 512;

pub fn clamp_terminal_dimensions(cols: usize, rows: usize) -> (usize, usize) {
    (
        cols.clamp(1, MAX_TERMINAL_COLS),
        rows.clamp(1, MAX_TERMINAL_ROWS),
    )
}

/// 连续内存网格存储 - 优化内存局部性和缓存命中率
/// 内存布局: cells[row * cols + col] 对应 grid[row][col]
#[derive(Clone)]
pub struct TerminalGrid {
    cells: Vec<TerminalCell>,
    rows: usize,
    cols: usize,
    pub row_wrapped: Vec<bool>,
}

impl TerminalGrid {
    pub fn new(rows: usize, cols: usize) -> Self {
        TerminalGrid {
            cells: vec![TerminalCell::default(); rows * cols],
            rows,
            cols,
            row_wrapped: vec![false; rows],
        }
    }

    #[inline]
    pub fn get(&self, row: usize, col: usize) -> &TerminalCell {
        &self.cells[row * self.cols + col]
    }

    #[inline]
    pub fn get_mut(&mut self, row: usize, col: usize) -> &mut TerminalCell {
        &mut self.cells[row * self.cols + col]
    }

    #[inline]
    pub fn rows(&self) -> usize {
        self.rows
    }

    #[inline]
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// 获取行作Vec引用（用于兼容旧代码）
    pub fn get_row(&self, row: usize) -> Vec<TerminalCell> {
        let start = row * self.cols;
        let end = start + self.cols;
        self.cells[start..end].to_vec()
    }

    /// 返回行数（兼容 grid.len()）
    #[inline]
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.rows
    }

    /// 返回行数（兼容 grid[i].len()）
    #[inline]
    pub fn row_len(&self) -> usize {
        self.cols
    }

    /// 设置整行
    pub fn set_row(&mut self, row: usize, cells: Vec<TerminalCell>) {
        let start = row * self.cols;
        let _end = start + self.cols;
        for (i, cell) in cells.into_iter().enumerate() {
            if start + i < self.cells.len() {
                self.cells[start + i] = cell;
            }
        }
    }

    /// 获取所有行为Vec<Vec> (用于兼容旧代码)
    pub fn to_vec(&self) -> Vec<Vec<TerminalCell>> {
        let mut result = Vec::with_capacity(self.rows);
        for row in 0..self.rows {
            result.push(self.get_row(row));
        }
        result
    }


    /// 在行内指定列插入一个cell，右侧cell右移，末尾cell被丢弃
    pub fn insert_cell_in_row(&mut self, row: usize, col: usize, cell: TerminalCell) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let start = row * self.cols;
        // Shift cells right from the end of the row down to col+1
        for i in (start + col..start + self.cols - 1).rev() {
            self.cells[i + 1] = self.cells[i].clone();
        }
        self.cells[start + col] = cell;
    }

    /// 删除行内指定列的cell，右侧cell左移，末尾补blank
    pub fn remove_cell_from_row(&mut self, row: usize, col: usize) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let start = row * self.cols;
        // Shift cells left
        for i in start + col..start + self.cols - 1 {
            self.cells[i] = self.cells[i + 1].clone();
        }
        // Fill last cell with default
        self.cells[start + self.cols - 1] = TerminalCell::default();
    }

    /// 删除第一行，向上移动所有行，末尾补新行
    pub fn remove_first_row(&mut self) -> (Vec<TerminalCell>, bool) {
        let removed = self.get_row(0);
        let was_wrapped = self.row_wrapped[0];
        // Shift all cells up by one row
        for i in 0..self.cells.len() - self.cols {
            self.cells[i] = self.cells[i + self.cols].clone();
        }
        // Clear last row
        let last_start = (self.rows - 1) * self.cols;
        for i in last_start..self.cells.len() {
            self.cells[i] = TerminalCell::default();
        }
        // Shift row_wrapped
        for i in 0..self.rows - 1 {
            self.row_wrapped[i] = self.row_wrapped[i + 1];
        }
        self.row_wrapped[self.rows - 1] = false;
        (removed, was_wrapped)
    }

    /// 用blank_cell填充末尾一行
    pub fn fill_last_row(&mut self, cell: TerminalCell) {
        let last_start = (self.rows - 1) * self.cols;
        for i in last_start..self.cells.len() {
            self.cells[i] = cell.clone();
        }
    }

    /// 是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// 调整网格大小
    pub fn resize(&mut self, new_rows: usize, new_cols: usize, default_cell: TerminalCell) {
        let mut new_cells = vec![default_cell; new_rows * new_cols];
        let copy_rows = self.rows.min(new_rows);
        let copy_cols = self.cols.min(new_cols);
        for row in 0..copy_rows {
            for col in 0..copy_cols {
                new_cells[row * new_cols + col] = self.cells[row * self.cols + col].clone();
            }
        }
        self.cells = new_cells;
        let mut new_wrapped = vec![false; new_rows];
        for row in 0..copy_rows {
            new_wrapped[row] = self.row_wrapped[row];
        }
        self.row_wrapped = new_wrapped;
        self.rows = new_rows;
        self.cols = new_cols;
    }

    /// 获取mut访问所有行
    pub fn iter_mut(&mut self) -> std::slice::ChunksMut<'_, TerminalCell> {
        self.cells.chunks_mut(self.cols)
    }

    /// 获取只读访问所有行
    pub fn iter(&self) -> std::slice::Chunks<'_, TerminalCell> {
        self.cells.chunks(self.cols)
    }
}

impl std::ops::Index<usize> for TerminalGrid {
    type Output = [TerminalCell];
    fn index(&self, row: usize) -> &[TerminalCell] {
        let start = row * self.cols;
        &self.cells[start..start + self.cols]
    }
}

impl std::ops::IndexMut<usize> for TerminalGrid {
    fn index_mut(&mut self, row: usize) -> &mut [TerminalCell] {
        let start = row * self.cols;
        &mut self.cells[start..start + self.cols]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    Indexed(u8),
    Rgb(u8, u8, u8),
    Default,
}

#[derive(Clone, Debug)]
#[derive(Default)]
pub enum CursorShape {
    #[default]
    Block,     // 0 or 1 - block cursor (default)
    Underline, // 2 - underline cursor
    Beam,      // 3 - beam/vertical line cursor
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[derive(Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    #[allow(dead_code)]
    Curly,   // SGR 4:3
    #[allow(dead_code)]
    Dotted,  // SGR 4:4
    #[allow(dead_code)]
    Dashed,  // SGR 4:5
}


#[derive(Clone, Copy, Debug, Default)]
pub struct StyleFlags {
    pub bold: bool,
    pub italic: bool,
    pub underline: UnderlineStyle,
    pub inverse: bool,
    pub dim: bool,
    pub blink: bool,
    pub strikethrough: bool,
}

#[derive(Clone, Debug)]
pub struct ScrollbackLine {
    pub cells: Vec<TerminalCell>,
    pub is_wrapped: bool,
}

#[derive(Clone, Debug)]
pub struct TerminalCell {
    pub character: char,
    pub foreground: Color,
    pub background: Color,
    pub flags: StyleFlags,
    pub wide: bool,
    pub wide_continuation: bool,
}

impl Default for TerminalCell {
    fn default() -> Self {
        TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: Color::Default,
            flags: StyleFlags::default(),
            wide: false,
            wide_continuation: false,
        }
    }
}

/// 追踪改变的行和列区间（脏矩形）
#[derive(Clone, Debug)]
pub struct DirtyRegion {
    pub rows: Vec<(usize, usize)>, // (row_start, row_end)，包含端点
    #[allow(dead_code)]
    pub col_start: usize,
    #[allow(dead_code)]
    pub col_end: usize,
}

impl DirtyRegion {
    pub fn new(cols: usize) -> Self {
        DirtyRegion {
            rows: Vec::new(),
            col_start: 0,
            col_end: cols,
        }
    }

    /// 标记某一行为脏
    pub fn mark_row(&mut self, row: usize) {
        if let Some(last) = self.rows.last_mut() {
            if row > 0 && last.1 == row - 1 {
                // 合并相邻的行
                last.1 = row;
                return;
            }
        }
        self.rows.push((row, row));
    }

    /// 标记行范围为脏
    pub fn mark_rows(&mut self, start: usize, end: usize) {
        for row in start..=end {
            self.mark_row(row);
        }
    }

    /// 标记整个网格为脏
    pub fn mark_all(&mut self, rows: usize) {
        self.rows.clear();
        self.rows.push((0, rows.saturating_sub(1)));
    }

    /// 清除脏标记
    pub fn clear(&mut self) {
        self.rows.clear();
    }

}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    /// 鼠标按下时的锚点（不排序，保持原始位置）
    pub anchor: (usize, usize),
    /// 鼠标当前位置
    pub active: (usize, usize),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Charset {
    #[default]
    Ascii,
    DecSpecialGraphics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardReadKind {
    MimeList,
    MimeData(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardReadRequest {
    pub kind: ClipboardReadKind,
}

pub struct TerminalState {
    pub grid: TerminalGrid,
    alt_grid: TerminalGrid,
    pub scrollback: VecDeque<ScrollbackLine>,
    pub selection: Option<Selection>,
    pub scroll_offset: usize,
    max_scrollback: usize,
    use_alt_buffer: bool,

    pub cursor_row: usize,
    pub cursor_col: usize,
    saved_cursor_row: usize,
    saved_cursor_col: usize,
    alt_cursor_row: usize,
    alt_cursor_col: usize,
    pub cursor_shape: CursorShape,

    current_fg: Color,
    current_bg: Color,
    current_flags: StyleFlags,
    pub window_title: String,

    // Global background color set by vim (CSI ... m)
    pub global_bg: Color,

    // Scrolling region (DECSTBM)
    scroll_region_top: usize,
    scroll_region_bottom: usize,

    // UTF-8 decoding buffer
    utf8_buf: [u8; 4],
    utf8_len: u8,
    utf8_expected: u8,

    // Incomplete escape sequence buffer across PTY reads
    pending_escape: Vec<u8>,

    g0_charset: Charset,
    g1_charset: Charset,
    active_charset: Charset,

    // IME support
    pub ime_enabled: bool,
    pub preedit_text: String,
    pub preedit_cursor: usize,

    // DECSET modes
    modes: std::collections::HashSet<u16>,

    // Output buffer for DSR/CPR responses to be sent back to PTY
    pub output_buffer: Vec<u8>,

    keyboard_enhancement_flags: u16,
    keyboard_enhancement_stack: Vec<u16>,
    alt_keyboard_enhancement_flags: u16,
    alt_keyboard_enhancement_stack: Vec<u16>,
    xterm_modify_other_keys: u16,
    xterm_format_other_keys: u16,
    pending_clipboard_requests: Vec<ClipboardReadRequest>,
    pending_paste_password: Option<String>,

    // Kitty graphics protocol support
    pub kitty_graphics: KittyGraphicsState,

    // Dirty rectangle tracking for optimized rendering
    pub dirty_region: DirtyRegion,

    // P4 优化：行版本化追踪
    pub grid_version: u64,      // 全局网格版本号
    pub row_versions: Vec<u64>, // 每行的修改版本号

    // Cached visible cells to avoid per-frame cloning
    visible_cells_cache: Option<(u64, usize, Vec<Vec<TerminalCell>>)>,

    // OSC 8 hyperlink tracking
    current_hyperlink: Option<(String, Option<String>)>, // (url, id)
    #[allow(dead_code)]
    osc8_hyperlinks: Vec<crate::link::Link>, // Stored hyperlinks from OSC 8
}

impl TerminalState {
    fn parse_csi_params(param_bytes: &[u8]) -> Vec<u16> {
        let mut params = Vec::new();
        let mut current = String::new();

        for &byte in param_bytes {
            match byte {
                b'0'..=b'9' => current.push(byte as char),
                b';' | b':' => {
                    if let Ok(value) = current.parse::<u16>() {
                        params.push(value);
                    }
                    current.clear();
                }
                _ => {}
            }
        }

        if let Ok(value) = current.parse::<u16>() {
            params.push(value);
        }

        params
    }

    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = clamp_terminal_dimensions(cols, rows);
        let grid = TerminalGrid::new(rows, cols);
        let alt_grid = TerminalGrid::new(rows, cols);

        let mut modes = std::collections::HashSet::new();
        modes.insert(25); // Cursor visible by default
        modes.insert(7); // Autowrap mode enabled by default (DECAWM)

        let mut dirty_region = DirtyRegion::new(cols);
        // Mark all rows as dirty on initialization to ensure first frame renders correctly
        dirty_region.mark_all(rows);

        TerminalState {
            grid,
            alt_grid,
            scrollback: VecDeque::new(),
            selection: None,
            scroll_offset: 0,
            max_scrollback: 2000,
            use_alt_buffer: false,
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor_row: 0,
            saved_cursor_col: 0,
            alt_cursor_row: 0,
            alt_cursor_col: 0,
            cursor_shape: CursorShape::default(),
            current_fg: Color::Default,
            current_bg: Color::Default,
            current_flags: StyleFlags::default(),
            window_title: String::new(),
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
            row_versions: vec![1; rows],  // Use 'rows' here since grid.rows() == rows at init
            visible_cells_cache: None,
            current_hyperlink: None,
            osc8_hyperlinks: Vec::new(),
        }
    }

    fn decode_base64(value: &str) -> Option<String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value)
            .ok()?;
        String::from_utf8(bytes).ok()
    }

    fn osc_terminator() -> &'static [u8] {
        b"\x1b\\"
    }

    fn append_osc_5522_status(&mut self, metadata: &str, payload: Option<&str>) {
        self.output_buffer.extend_from_slice(b"\x1b]5522;");
        self.output_buffer.extend_from_slice(metadata.as_bytes());
        if let Some(payload) = payload {
            self.output_buffer.extend_from_slice(b";");
            self.output_buffer.extend_from_slice(payload.as_bytes());
        }
        self.output_buffer.extend_from_slice(Self::osc_terminator());
    }

    fn handle_osc_5522(&mut self, metadata: &str, payload: Option<&str>) {
        crate::debug_log!("[OSC5522] metadata={} payload={:?}", metadata, payload);

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

    fn set_keyboard_enhancement_flags(&mut self, flags: u16, mode: u16) {
        match mode {
            1 => self.keyboard_enhancement_flags = flags,
            2 => self.keyboard_enhancement_flags |= flags,
            3 => self.keyboard_enhancement_flags &= !flags,
            _ => {}
        }
    }

    fn push_keyboard_enhancement_flags(&mut self, flags: u16) {
        if self.keyboard_enhancement_stack.len() >= 32 {
            self.keyboard_enhancement_stack.remove(0);
        }
        self.keyboard_enhancement_stack
            .push(self.keyboard_enhancement_flags);
        self.keyboard_enhancement_flags = flags;
    }

    fn pop_keyboard_enhancement_flags(&mut self, count: usize) {
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

    fn put_char(&mut self, ch: char) {
        let _orig_ch = ch;
        let ch = self.translate_char(ch);
        let width = crate::char_width::cached_char_width(ch);
        if width == 0 {
            return; // Skip zero-width characters for now
        }

        let cols = self.grid.row_len();
        let blank_cell = self.create_blank_cell();

        // If character doesn't fit at end of line, handle based on autowrap mode
        if self.cursor_col + width > cols {
            // Only wrap to next line if autowrap mode (mode 7) is enabled
            if self.modes.contains(&7) {
                self.grid.row_wrapped[self.cursor_row] = true;
                self.cursor_col = 0;
                self.cursor_row += 1;
                if self.cursor_row >= self.grid.rows() {
                    self.cursor_row = self.grid.rows() - 1;
                    self.scroll_down();
                }
            } else {
                // Autowrap disabled: clamp cursor to last column instead of wrapping
                self.cursor_col = cols.saturating_sub(width);
            }
        }

        // If current position has a continuation cell to its left, clear the wide character
        if self.cursor_col > 0
            && self
                .grid
                .get(self.cursor_row, self.cursor_col)
                .wide_continuation
        {
            *self.grid.get_mut(self.cursor_row, self.cursor_col - 1) = blank_cell.clone();
        }

        // If current position has a wide character, clear its continuation cell
        if self.grid.get(self.cursor_row, self.cursor_col).wide && self.cursor_col + 1 < cols {
            *self.grid.get_mut(self.cursor_row, self.cursor_col + 1) = blank_cell.clone();
        }

        // Write character
        let cell = self.grid.get_mut(self.cursor_row, self.cursor_col);
        cell.character = ch;
        cell.foreground = self.current_fg;
        cell.background = self.current_bg;
        cell.flags = self.current_flags;
        cell.wide = width == 2;
        cell.wide_continuation = false;

        // Set up wide character continuation cell if needed
        if width == 2 && self.cursor_col + 1 < cols {
            let cont_cell = self.grid.get_mut(self.cursor_row, self.cursor_col + 1);
            *cont_cell = blank_cell;
            cont_cell.wide_continuation = true;
        }

        self.cursor_col += width;
        // Mark the row as dirty after writing character
        self.dirty_region.mark_row(self.cursor_row);
        self.mark_row_dirty(self.cursor_row);
    }

    fn create_blank_cell(&self) -> TerminalCell {
        TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: self.current_bg, // Preserve current background color
            flags: StyleFlags::default(),
            wide: false,
            wide_continuation: false,
        }
    }

    fn blank_line(&self, cols: usize) -> Vec<TerminalCell> {
        vec![self.create_blank_cell(); cols]
    }

    fn normalize_line_width(&self, mut line: Vec<TerminalCell>, cols: usize) -> Vec<TerminalCell> {
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

    fn push_scrollback_line(&mut self, cells: Vec<TerminalCell>, is_wrapped: bool) {
        if self.use_alt_buffer {
            return;
        }

        if self.scrollback.len() >= self.max_scrollback {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(ScrollbackLine {
            cells,
            is_wrapped,
        });
    }

    fn scroll_region_up(&mut self, top: usize, bottom: usize) {
        if top >= self.grid.rows() || bottom >= self.grid.rows() || top > bottom {
            return;
        }

        let cols = self.grid.row_len();
        let removed_line = self.grid.get_row(top);
        let was_wrapped = self.grid.row_wrapped[top];

        for row in top..bottom {
            let next_row = self.grid.get_row(row + 1);
            self.grid.set_row(row, next_row);
            self.grid.row_wrapped[row] = self.grid.row_wrapped[row + 1];
        }
        self.grid.set_row(bottom, self.blank_line(cols));
        self.grid.row_wrapped[bottom] = false;

        // Mark the scrolled region as dirty
        self.dirty_region.mark_rows(top, bottom);
        self.mark_rows_dirty(top, bottom);

        let is_full_screen_region = top == 0 && bottom + 1 == self.grid.rows();
        if is_full_screen_region {
            self.push_scrollback_line(removed_line, was_wrapped);
        }
    }

    fn charset_from_designator(byte: u8) -> Charset {
        match byte {
            b'0' => Charset::DecSpecialGraphics,
            _ => Charset::Ascii,
        }
    }

    fn translate_char(&self, ch: char) -> char {
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

    fn clear_cell(&mut self, row: usize, col: usize) {
        let cols = self.grid.row_len();
        let bg_color = self.current_bg;
        let blank_cell = TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: bg_color,
            flags: StyleFlags::default(),
            wide: false,
            wide_continuation: false,
        };
        // If clearing a continuation cell, also clear the wide character body
        if self.grid.get(row, col).wide_continuation && col > 0 {
            *self.grid.get_mut(row, col - 1) = blank_cell.clone();
        }
        // If clearing a wide character body, also clear the continuation cell
        if self.grid.get(row, col).wide && col + 1 < cols {
            *self.grid.get_mut(row, col + 1) = blank_cell.clone();
        }
        *self.grid.get_mut(row, col) = blank_cell;
    }

    /// P3 优化：批量处理输入数据，只在处理完成后触发一次网格版本更新
    /// 相比多次 process_input，这个方法避免了多次网格版本递增
    pub fn process_batch(&mut self, input: &[u8]) {
        // 累积所有输入字节，一次性处理
        self.process_input(input);
        // 网格版本已在 process_input 中根据实际改变自动递增
    }

    /// P4：标记一行已修改
    #[inline]
    fn mark_row_dirty(&mut self, row: usize) {
        self.grid_version = self.grid_version.wrapping_add(1);
        if row < self.row_versions.len() {
            self.row_versions[row] = self.grid_version;
        }
    }

    /// P4：标记多行已修改
    #[inline]
    fn mark_rows_dirty(&mut self, start: usize, end: usize) {
        self.grid_version = self.grid_version.wrapping_add(1);
        for row in start..=end.min(self.row_versions.len() - 1) {
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

    #[allow(dead_code)]
    pub fn is_focus_event_mode(&self) -> bool {
        self.modes.contains(&1004)
    }

    #[allow(dead_code)]
    pub fn is_bracketed_paste_mode(&self) -> bool {
        self.modes.contains(&2004)
    }

    #[allow(dead_code)]
    pub fn emit_focus_in(&mut self) {
        if self.modes.contains(&1004) {
            self.output_buffer.extend_from_slice(b"\x1b[I");
        }
    }

    #[allow(dead_code)]
    pub fn emit_focus_out(&mut self) {
        if self.modes.contains(&1004) {
            self.output_buffer.extend_from_slice(b"\x1b[O");
        }
    }

    pub fn process_input(&mut self, input: &[u8]) {
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

            match byte {
                b'\x08' | b'\x7f' => {
                    // Backspace (0x08) and Delete (0x7f) - just move cursor left
                    // Shell handles actual deletion and sends back updated display
                    if self.cursor_col > 0 {
                        self.cursor_col -= 1;
                    }
                    i += 1;
                }
                b'\n' => {
                    // Linefeed - move cursor down or scroll
                    if self.cursor_row < self.scroll_region_bottom {
                        // Cursor is not at bottom of scroll region, just move down
                        self.cursor_row += 1;
                    } else {
                        // Cursor is at bottom of scroll region, scroll the region
                        self.scroll_region_up(self.scroll_region_top, self.scroll_region_bottom);
                        // Cursor stays at bottom row of the scroll region
                    }
                    i += 1;
                }
                b'\r' => {
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
                    // Tab
                    self.cursor_col = ((self.cursor_col + 8) / 8) * 8;
                    if self.cursor_col >= self.grid.row_len() {
                        self.cursor_col = self.grid.row_len() - 1;
                    }
                    i += 1;
                }
                b'\x1b' => {
                    let esc_start = i;

                    if i + 1 >= data_slice.len() {
                        self.pending_escape.extend_from_slice(&data_slice[esc_start..]);
                        break;
                    }

                    match data_slice[i + 1] {
                        b'7' => {
                            // DECSC - Save Cursor Position
                            self.saved_cursor_row = self.cursor_row;
                            self.saved_cursor_col = self.cursor_col;
                            i += 2;
                        }
                        b'8' => {
                            // DECRC - Restore Cursor Position
                            self.cursor_row = self.saved_cursor_row.min(self.grid.rows() - 1);
                            self.cursor_col = self.saved_cursor_col.min(self.grid.row_len() - 1);
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
                                self.pending_escape.extend_from_slice(&data_slice[esc_start..]);
                                break;
                            }

                            let payload_end = if data_slice[i - 1] == 0x07 { i - 1 } else { i - 2 };
                            if payload_end >= payload_start {
                                if let Ok(payload) =
                                    std::str::from_utf8(&data_slice[payload_start..payload_end])
                                {
                                    if let Some((command, value)) = payload.split_once(';') {
                                        if command == "0" || command == "2" {
                                            self.window_title.clear();
                                            self.window_title.push_str(value);
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
                                                    self.current_hyperlink = Some((uri.to_string(), id));
                                                }
                                            } else if value.is_empty() {
                                                // OSC 8 ; ; (close hyperlink)
                                                self.current_hyperlink = None;
                                            }
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
                            i += 2;

                            let mut terminated = false;
                            let dcs_start = i;
                            while i < data_slice.len() {
                                if i + 1 < data_slice.len() && data_slice[i] == 0x1b && data_slice[i + 1] == 0x5c {
                                    // Extract DCS payload
                                    let payload = &data_slice[dcs_start..i];

                                    // Check if this is a Kitty graphics protocol DCS
                                    if let Ok(payload_str) = std::str::from_utf8(payload) {
                                        // Kitty graphics protocol starts with @ or other specific markers
                                        if payload_str.starts_with('@')
                                            || payload_str.contains("a=")
                                            || payload_str.starts_with("kitty")
                                        {
                                            if let Err(e) = self
                                                .kitty_graphics
                                                .parse_graphics_payload(payload_str)
                                            {
                                                crate::debug_log!(
                                                    "[DCS] Kitty graphics error: {}",
                                                    e
                                                );
                                            }
                                        }
                                    }

                                    i += 2;
                                    terminated = true;
                                    break;
                                }
                                i += 1;
                            }

                            if !terminated {
                                self.pending_escape.extend_from_slice(&data_slice[esc_start..]);
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
                                self.pending_escape.extend_from_slice(&data_slice[esc_start..]);
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
                            i += 2;

                            if self.cursor_row > self.scroll_region_top {
                                self.cursor_row -= 1;
                            } else if self.scroll_region_top < self.grid.rows()
                                && self.scroll_region_bottom < self.grid.rows()
                                && self.scroll_region_top <= self.scroll_region_bottom
                            {
                                let cols = self.grid.row_len();
                                let mut new_lines = vec![self.blank_line(cols)];

                                for row in self.scroll_region_top..self.scroll_region_bottom {
                                    if row < self.grid.rows() {
                                        new_lines.push(self.grid.get_row(row));
                                    }
                                }

                                for (offset, line) in new_lines.iter().enumerate() {
                                    if self.scroll_region_top + offset
                                        <= self.scroll_region_bottom
                                    {
                                        self.grid.set_row(
                                            self.scroll_region_top + offset,
                                            line.clone(),
                                        );
                                    }
                                }
                                self.mark_rows_dirty(
                                    self.scroll_region_top,
                                    self.scroll_region_bottom,
                                );
                            }
                        }
                        b'D' => {
                            i += 2;

                            if self.cursor_row < self.scroll_region_bottom {
                                self.cursor_row += 1;
                            } else {
                                self.scroll_region_up(
                                    self.scroll_region_top,
                                    self.scroll_region_bottom,
                                );
                            }
                        }
                        b'[' => {
                            i += 2;

                            // Use stack arrays for CSI params (typical CSI sequences are short)
                            let mut param_bytes = [0u8; 32];
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
                                self.pending_escape.extend_from_slice(&data_slice[esc_start..]);
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
                            let params = Self::parse_csi_params(&param_bytes[..param_len]);
                            let cmd = final_byte as char;

                            self.handle_escape_sequence(
                                &params,
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
                    // ASCII printable character
                    self.put_char(byte as char);
                    i += 1;
                }
                // UTF-8 2-byte sequence (0xC2-0xDF)
                0xC2..=0xDF => {
                    self.utf8_buf[0] = byte;
                    self.utf8_len = 1;
                    self.utf8_expected = 2;
                    i += 1;
                }
                // UTF-8 3-byte sequence (0xE0-0xEF)
                0xE0..=0xEF => {
                    self.utf8_buf[0] = byte;
                    self.utf8_len = 1;
                    self.utf8_expected = 3;
                    i += 1;
                }
                // UTF-8 4-byte sequence (0xF0-0xF4)
                0xF0..=0xF4 => {
                    self.utf8_buf[0] = byte;
                    self.utf8_len = 1;
                    self.utf8_expected = 4;
                    i += 1;
                }
                _ => {
                    // Invalid byte or continuation byte with no sequence - skip
                    if self.utf8_len > 0 && (byte & 0xC0) == 0x80 {
                        // UTF-8 continuation byte
                        self.utf8_buf[self.utf8_len as usize] = byte;
                        self.utf8_len += 1;
                        if self.utf8_len == self.utf8_expected {
                            // Sequence complete, decode it
                            if let Ok(s) =
                                std::str::from_utf8(&self.utf8_buf[..self.utf8_len as usize])
                            {
                                if let Some(ch) = s.chars().next() {
                                    self.put_char(ch);
                                }
                            }
                            self.utf8_len = 0;
                        }
                    } else {
                        // Invalid continuation byte or stray byte - reset buffer and skip
                        self.utf8_len = 0;
                    }
                    i += 1;
                }
            }
        }
    }

    fn handle_escape_sequence(
        &mut self,
        params: &[u16],
        cmd: char,
        private_prefix: Option<u8>,
        intermediates: &[u8],
    ) {
        match cmd {
            'A' => {
                // Cursor up - should scroll region down if at top
                let n = params.first().copied().unwrap_or(1) as usize;

                let mut scrolled = false;
                for _ in 0..n {
                    if self.cursor_row > self.scroll_region_top {
                        // Cursor is not at top of scroll region, just move up
                        self.cursor_row -= 1;
                    } else {
                        // Cursor is at top of scroll region, scroll the region down
                        if self.scroll_region_top < self.grid.rows()
                            && self.scroll_region_bottom < self.grid.rows()
                        {
                            let cols = self.grid.row_len();
                            let mut new_lines = vec![self.blank_line(cols)]; // New blank line at top

                            // Keep lines from top to bottom-1
                            for i in self.scroll_region_top..self.scroll_region_bottom {
                                if i < self.grid.rows() {
                                    new_lines.push(self.grid.get_row(i));
                                }
                            }

                            // Replace region lines
                            for (j, line) in new_lines.iter().enumerate() {
                                if self.scroll_region_top + j <= self.scroll_region_bottom {
                                    self.grid.set_row(self.scroll_region_top + j, line.clone());
                                }
                            }
                            scrolled = true;
                        }
                        // Cursor stays at top row
                    }
                }
                if scrolled {
                    self.mark_rows_dirty(self.scroll_region_top, self.scroll_region_bottom);
                }
            }
            'B' => {
                let n = params.first().copied().unwrap_or(1) as usize;
                self.cursor_row = (self.cursor_row + n).min(self.grid.rows() - 1);
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
                // Move cursor down and to start of line
                let n = params.first().copied().unwrap_or(1) as usize;
                self.cursor_row = (self.cursor_row + n).min(self.grid.rows() - 1);
                self.cursor_col = 0;
            }
            'F' => {
                // Move cursor up and to start of line
                let n = params.first().copied().unwrap_or(1) as usize;
                self.cursor_row = self.cursor_row.saturating_sub(n);
                self.cursor_col = 0;
            }
            'G' => {
                // Move cursor to column
                let col = params.first().copied().unwrap_or(1) as usize;
                self.cursor_col = col.saturating_sub(1).min(self.grid.row_len() - 1);
            }
            'H' => {
                let row = params.first().copied().unwrap_or(1) as usize;
                let col = params.get(1).copied().unwrap_or(1) as usize;
                self.cursor_row = row.saturating_sub(1).min(self.grid.rows() - 1);
                self.cursor_col = col.saturating_sub(1).min(self.grid.row_len() - 1);
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
                    self.cursor_row = row.saturating_sub(1).min(self.grid.rows() - 1);
                    self.cursor_col = col.saturating_sub(1).min(self.grid.row_len() - 1);
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
                        self.clear_screen();
                        // clear_screen already marks all rows as dirty
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
                // Insert line(s) at cursor position (push lines down)
                let n = params.first().copied().unwrap_or(1) as usize;
                for _ in 0..n {
                    if self.cursor_row >= self.scroll_region_top
                        && self.cursor_row <= self.scroll_region_bottom
                    {
                        let cols = self.grid.row_len();
                        // Shift lines down within scroll region: move cursor_row..bottom-1 to cursor_row+1..bottom
                        for row in (self.cursor_row..self.scroll_region_bottom).rev() {
                            let src = self.grid.get_row(row);
                            self.grid.set_row(row + 1, src);
                        }
                        // Insert blank line at cursor position
                        self.grid.set_row(self.cursor_row, self.blank_line(cols));
                    }
                }
                self.mark_rows_dirty(self.cursor_row, self.scroll_region_bottom);
            }
            'M' => {
                // Delete line(s) at cursor position (pull lines up)
                let n = params.first().copied().unwrap_or(1) as usize;
                for _ in 0..n {
                    if self.cursor_row >= self.scroll_region_top
                        && self.cursor_row <= self.scroll_region_bottom
                    {
                        let cols = self.grid.row_len();
                        // Shift lines up within scroll region: move cursor_row+1..bottom to cursor_row..bottom-1
                        for row in self.cursor_row..self.scroll_region_bottom {
                            let src = self.grid.get_row(row + 1);
                            self.grid.set_row(row, src);
                        }
                        // Insert blank line at bottom of region
                        self.grid
                            .set_row(self.scroll_region_bottom, self.blank_line(cols));
                    }
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
                    self.handle_sgr(params);
                }
            }
            's' => {
                if private_prefix.is_none() && intermediates.is_empty() {
                    self.saved_cursor_row = self.cursor_row;
                    self.saved_cursor_col = self.cursor_col;
                }
            }
            'u' => {
                if intermediates.is_empty() {
                    match private_prefix {
                        None => {
                            self.cursor_row = self.saved_cursor_row.min(self.grid.rows() - 1);
                            self.cursor_col = self.saved_cursor_col.min(self.grid.row_len() - 1);
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
                // Scroll up (Scroll Up, SU) - content moves up, new lines appear at bottom
                let n = params.first().copied().unwrap_or(1) as usize;
                // Scroll within the scroll region by moving lines
                for _ in 0..n {
                    self.scroll_region_up(self.scroll_region_top, self.scroll_region_bottom);
                }
            }
            'T' => {
                // Scroll down (Scroll Down, SD) - content moves down, new lines appear at top
                let n = params.first().copied().unwrap_or(1) as usize;
                // Scroll within the scroll region by moving lines
                for _ in 0..n {
                    if self.scroll_region_top < self.grid.rows()
                        && self.scroll_region_bottom < self.grid.rows()
                        && self.scroll_region_top <= self.scroll_region_bottom
                    {
                        let cols = self.grid.row_len();

                        // Shift lines down within the region by collecting from bottom to top
                        let mut new_lines = vec![self.blank_line(cols)]; // New blank line at top

                        // Keep lines from top to bottom-1
                        for i in self.scroll_region_top..self.scroll_region_bottom {
                            if i < self.grid.rows() {
                                new_lines.push(self.grid.get_row(i));
                            }
                        }

                        // Replace region lines
                        for (i, line) in new_lines.iter().enumerate() {
                            if self.scroll_region_top + i <= self.scroll_region_bottom {
                                self.grid.set_row(self.scroll_region_top + i, line.clone());
                            }
                        }
                        self.mark_rows_dirty(self.scroll_region_top, self.scroll_region_bottom);
                    }
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
                if private_prefix == Some(b'?') && intermediates == [b'$']
                    && params.first().copied() == Some(5522) {
                        let state = if self.modes.contains(&5522) { 1 } else { 2 };
                        let response = format!("\x1b[?5522;{}$y", state);
                        crate::debug_log!("[OSC5522] DECRQM query -> {}", response);
                        self.output_buffer.extend_from_slice(response.as_bytes());
                    }
            }
            'h' => {
                // Set mode (DECSET)
                for &mode in params {
                    self.set_mode(mode);
                }
            }
            'l' => {
                // Reset mode (DECRST)
                for &mode in params {
                    self.reset_mode(mode);
                }
            }
            'r' => {
                // Set scroll region (DECSTBM)
                let top = params.first().copied().unwrap_or(1) as usize;
                let bottom = params.get(1).copied().unwrap_or(self.grid.rows() as u16) as usize;

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

                // Move cursor to home position when setting scroll region
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
            '@' => {
                // ICH - Insert Character(s)
                let n = params.first().copied().unwrap_or(1) as usize;
                let cols = self.grid.row_len();
                let blank_cell = self.create_blank_cell();
                if self.cursor_col < cols {
                    // Insert n blank cells at cursor position, shifting content right
                    // insert_cell_in_row shifts cells right and discards the last cell
                    for _ in 0..n {
                        if self.cursor_col < cols {
                            self.grid.insert_cell_in_row(
                                self.cursor_row,
                                self.cursor_col,
                                blank_cell.clone(),
                            );
                        }
                    }
                    // Mark row as dirty after modification
                    self.mark_row_dirty(self.cursor_row);
                }
            }
            'P' => {
                // DCH - Delete Character(s)
                let n = params.first().copied().unwrap_or(1) as usize;
                let blank_cell = self.create_blank_cell();
                for _ in 0..n {
                    if self.cursor_col < self.grid.row_len() {
                        self.grid
                            .remove_cell_from_row(self.cursor_row, self.cursor_col);
                        // Fill the last cell with proper blank (remove_cell_from_row uses default)
                        let last_col = self.grid.row_len() - 1;
                        *self.grid.get_mut(self.cursor_row, last_col) = blank_cell.clone();
                    }
                }
                // Mark row as dirty after modification
                self.mark_row_dirty(self.cursor_row);
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
                if private_prefix == Some(b'>') && intermediates.is_empty()
                    && params.first().copied().unwrap_or(0) == 0 {
                        crate::debug_log!("[XTVERSION] report terminal version request");
                        self.output_buffer.extend_from_slice(XTERM_VERSION_RESPONSE);
                    }

                // DECSCUSR - Set cursor style
                if private_prefix.is_none() && intermediates == [b' '] {
                    let shape = params.first().copied().unwrap_or(0) as u8;
                    self.cursor_shape = match shape {
                        0 | 1 => CursorShape::Block,
                        2 => CursorShape::Underline,
                        3 => CursorShape::Beam,
                        _ => CursorShape::Block,
                    };
                }
            }
            _ => {}
        }
    }

    fn handle_sgr(&mut self, params: &[u16]) {
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
                1 => self.current_flags.bold = true,
                2 => self.current_flags.dim = true,
                3 => self.current_flags.italic = true,
                4 => self.current_flags.underline = UnderlineStyle::Single,
                5 => self.current_flags.blink = true,
                7 => self.current_flags.inverse = true,
                9 => self.current_flags.strikethrough = true,
                21 => self.current_flags.underline = UnderlineStyle::Double,
                22 => {
                    self.current_flags.bold = false;
                    self.current_flags.dim = false;
                }
                23 => self.current_flags.italic = false,
                24 => self.current_flags.underline = UnderlineStyle::None,
                25 => self.current_flags.blink = false,
                27 => self.current_flags.inverse = false,
                29 => self.current_flags.strikethrough = false,
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
                // Extended color support: 38;5;n (256 color) and 38;2;r;g;b (RGB)
                38 => {
                    if i + 2 < params.len() {
                        match params[i + 1] {
                            5 => {
                                // 256 color mode
                                self.current_fg = Color::Indexed(params[i + 2] as u8);
                                i += 2;
                            }
                            2 => {
                                // RGB mode
                                if i + 4 < params.len() {
                                    self.current_fg = Color::Rgb(
                                        params[i + 2] as u8,
                                        params[i + 3] as u8,
                                        params[i + 4] as u8,
                                    );
                                    i += 4;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                48 => {
                    if i + 2 < params.len() {
                        match params[i + 1] {
                            5 => {
                                // 256 color mode for background
                                self.current_bg = Color::Indexed(params[i + 2] as u8);
                                self.global_bg = self.current_bg; // Update global background
                                crate::debug_log!(
                                    "[CSI] Background color (256-color) set to: index {}",
                                    params[i + 2]
                                );
                                i += 2;
                            }
                            2 => {
                                // RGB mode for background
                                if i + 4 < params.len() {
                                    self.current_bg = Color::Rgb(
                                        params[i + 2] as u8,
                                        params[i + 3] as u8,
                                        params[i + 4] as u8,
                                    );
                                    self.global_bg = self.current_bg; // Update global background
                                    crate::debug_log!(
                                        "[CSI] Background color (RGB) set to: ({}, {}, {})",
                                        params[i + 2],
                                        params[i + 3],
                                        params[i + 4]
                                    );
                                    i += 4;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn clear_screen(&mut self) {
        let bg_color = self.current_bg;
        for row in self.grid.iter_mut() {
            for cell in row.iter_mut() {
                *cell = TerminalCell {
                    character: ' ',
                    foreground: Color::Default,
                    background: bg_color,
                    flags: StyleFlags::default(),
                    wide: false,
                    wide_continuation: false,
                };
            }
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
        // Mark all rows as dirty
        self.dirty_region.mark_all(self.grid.rows());
        self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
    }

    fn set_mode(&mut self, mode: u16) {
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
            1049 => {
                // Alternate screen buffer
                if !self.use_alt_buffer {
                    // Save main buffer state (cursor position)
                    self.saved_cursor_row = self.cursor_row;
                    self.saved_cursor_col = self.cursor_col;

                    // Reset scroll offset so we don't show scrollback in alt buffer
                    self.scroll_offset = 0;

                    // Switch to alternate buffer
                    std::mem::swap(&mut self.grid, &mut self.alt_grid);
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

                    // Clear alt buffer and move cursor to home
                    self.clear_screen();
                    self.modes.insert(1049);
                }
            }
            7 => {
                // Autowrap mode
                self.modes.insert(7);
            }
            _ => {
                // Unknown mode, just store it
                self.modes.insert(mode);
            }
        }
    }

    fn reset_mode(&mut self, mode: u16) {
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
            1000..=1003 => {
                // Disable mouse reporting
                self.modes.remove(&mode);
            }
            1006 => {
                // Disable SGR mouse reporting format
                self.modes.remove(&mode);
            }
            1049 => {
                // Restore main screen buffer
                if self.use_alt_buffer {
                    // Save alt buffer state (cursor position)
                    self.alt_cursor_row = self.cursor_row;
                    self.alt_cursor_col = self.cursor_col;

                    // Switch back to main buffer
                    std::mem::swap(&mut self.grid, &mut self.alt_grid);
                    self.cursor_row = self.saved_cursor_row;
                    self.cursor_col = self.saved_cursor_col;
                    std::mem::swap(
                        &mut self.keyboard_enhancement_flags,
                        &mut self.alt_keyboard_enhancement_flags,
                    );
                    std::mem::swap(
                        &mut self.keyboard_enhancement_stack,
                        &mut self.alt_keyboard_enhancement_stack,
                    );
                    self.use_alt_buffer = false;
                    self.modes.remove(&1049);

                    // Reset SGR attributes to prevent alternate screen colors from bleeding through
                    self.current_fg = Color::Default;
                    self.current_bg = Color::Default;
                    self.global_bg = Color::Default;
                    self.current_flags = StyleFlags::default();

                    // Mark all rows dirty after grid swap to force full re-render
                    // Increment by rows+1 to trigger grid_version_jumped in ui.rs
                    self.grid_version += self.grid.rows() as u64 + 1;
                    for row_ver in &mut self.row_versions {
                        *row_ver = self.grid_version;
                    }
                    self.dirty_region.mark_all(self.grid.rows());
                }
            }
            7 => {
                // Disable autowrap
                self.modes.remove(&7);
            }
            _ => {
                // Unknown mode, just remove it
                self.modes.remove(&mode);
            }
        }
    }

    pub fn max_scrollback(&self) -> usize {
        self.max_scrollback
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
            let button_byte = 32 + button ;
            let col_byte = 32 + (col as u8).min(223) ;
            let row_byte = 32 + (row as u8).min(223) ;
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

    fn scroll_down(&mut self) {
        if self.grid.rows() > 0 {
            crate::debug_log!(
                "[SCROLL] scroll_down() in buffer (alt={})",
                self.use_alt_buffer
            );
            let bg_color = self.current_bg;
            let blank_cell = TerminalCell {
                character: ' ',
                foreground: Color::Default,
                background: bg_color,
                flags: StyleFlags::default(),
                wide: false,
                wide_continuation: false,
            };
            let (old_line, was_wrapped) = self.grid.remove_first_row();
            self.grid.fill_last_row(blank_cell);
            self.push_scrollback_line(old_line, was_wrapped);
            // Mark all rows as dirty after scrolling
            self.dirty_region.mark_all(self.grid.rows());
            self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
        }
    }

    pub fn get_visible_cells(&mut self) -> Vec<Vec<TerminalCell>> {
        // Check if cache is valid
        if let Some((cached_version, cached_offset, ref cells)) = self.visible_cells_cache {
            if cached_version == self.grid_version && cached_offset == self.scroll_offset {
                // Return clone of cached data (cheap: only clones Vec pointers, not cells)
                return cells.clone();
            }
        }

        // Cache miss - rebuild
        let rows = self.grid.rows();
        let cols = if rows > 0 { self.grid.row_len() } else { 80 };

        let cells = if self.scroll_offset == 0 {
            // Fast path: just get current grid
            self.grid.to_vec()
        } else {
            // Slow path: reflow scrollback
            let blank_cell = self.create_blank_cell();

            let mut start_idx = self.scrollback.len().saturating_sub(self.scroll_offset + rows);
            while start_idx > 0 && self.scrollback[start_idx - 1].is_wrapped {
                start_idx -= 1;
            }
            let end_idx = self.scrollback.len();
            let to_reflow: Vec<ScrollbackLine> = self.scrollback
                .iter()
                .skip(start_idx)
                .take(end_idx - start_idx)
                .cloned()
                .collect();

            let reflowed = Self::reflow_lines(&to_reflow, cols, &blank_cell);
            let skip = reflowed.len().saturating_sub(self.scroll_offset + rows);
            let visible_start = skip + (reflowed.len() - skip).saturating_sub(self.scroll_offset);
            let mut result: Vec<Vec<TerminalCell>> = reflowed[visible_start..].iter().map(|l| l.cells.clone()).collect();

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

        // Store in cache and return clone
        let result_clone = cells.clone();
        self.visible_cells_cache = Some((self.grid_version, self.scroll_offset, cells));
        result_clone
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

    /// Convert a viewport-relative row to an absolute row in the full buffer
    /// (scrollback + grid). Absolute row 0 = first scrollback line.
    fn viewport_row_to_absolute(&self, viewport_row: usize) -> usize {
        self.scrollback.len().saturating_sub(self.scroll_offset) + viewport_row
    }

    #[allow(dead_code)]
    pub fn select_text(&mut self, anchor: (usize, usize), active: (usize, usize)) {
        self.selection = Some(Selection { anchor, active });
    }

    /// Start a new selection at a viewport-relative position.
    /// Converts to absolute buffer coordinates internally.
    pub fn start_selection(&mut self, viewport_pos: (usize, usize)) {
        let abs = (
            self.viewport_row_to_absolute(viewport_pos.0),
            viewport_pos.1,
        );
        self.selection = Some(Selection {
            anchor: abs,
            active: abs,
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
        if line[start_col].wide_continuation && start_col > 0 {
            start_col -= 1;
        }

        if let Some((left, right)) = Self::select_extended_token_span(line, start_col) {
            let abs_row = self.viewport_row_to_absolute(row);
            self.selection = Some(Selection {
                anchor: (abs_row, left),
                active: (abs_row, right),
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
            if line[prev].wide_continuation {
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
            let next = if line[right].wide {
                right + 2
            } else {
                right + 1
            };
            if next >= cols {
                break;
            }
            if line[next].wide_continuation {
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
        if line[right].wide && right + 1 < cols {
            right += 1;
        }

        let abs_row = self.viewport_row_to_absolute(row);
        self.selection = Some(Selection {
            anchor: (abs_row, left),
            active: (abs_row, right),
        });
    }

    fn select_extended_token_span(
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
            if line[prev].wide_continuation {
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
            let next = if line[right].wide {
                right + 2
            } else {
                right + 1
            };
            if next >= cols {
                break;
            }
            if line[next].wide_continuation {
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
            right -= if line[right].wide_continuation && right > 0 {
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
            if cell.wide_continuation {
                continue;
            }
            let ch = cell.character;
            has_alnum |= ch.is_alphanumeric();
            has_separator |= is_extended_token_separator(ch);
        }

        if !has_alnum || !has_separator {
            return None;
        }

        if line[right].wide && right + 1 < cols {
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

                if abs_row < scrollback_len {
                    // Read from scrollback
                    let line = &self.scrollback[abs_row].cells;
                    for col in start_col..=end_col.min(line.len().saturating_sub(1)) {
                        if !line[col].wide_continuation {
                            result.push(line[col].character);
                        }
                    }
                } else {
                    // Read from current grid
                    let grid_row = abs_row - scrollback_len;
                    if grid_row < grid_rows {
                        for col in start_col..=end_col {
                            let cell = self.grid.get(grid_row, col);
                            if !cell.wide_continuation {
                                result.push(cell.character);
                            }
                        }
                    }
                }

                if abs_row < end.0 {
                    result.push('\n');
                }
            }

            result
        })
    }

    pub fn scroll(&mut self, lines: isize) {
        // Don't scroll scrollback when in alternate screen buffer (less, vim, git log, etc.)
        if self.use_alt_buffer {
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

    fn strip_trailing_blanks(cells: &[TerminalCell]) -> &[TerminalCell] {
        let mut end = cells.len();
        while end > 0 && cells[end - 1].character == ' ' && cells[end - 1].background == Color::Default && !cells[end - 1].wide && !cells[end - 1].wide_continuation {
            end -= 1;
        }
        &cells[..end]
    }

    fn reflow_lines(lines: &[ScrollbackLine], new_cols: usize, blank_cell: &TerminalCell) -> Vec<ScrollbackLine> {
        let mut result = Vec::new();
        let len = lines.len();
        let mut i = 0;

        while i < len {
            let mut logical_line: Vec<TerminalCell> = Vec::new();
            logical_line.extend_from_slice(Self::strip_trailing_blanks(&lines[i].cells));
            while i < len && lines[i].is_wrapped {
                i += 1;
                if i < len {
                    logical_line.extend_from_slice(Self::strip_trailing_blanks(&lines[i].cells));
                }
            }
            i += 1;

            if logical_line.is_empty() {
                result.push(ScrollbackLine {
                    cells: vec![blank_cell.clone(); new_cols],
                    is_wrapped: false,
                });
                continue;
            }

            let chunks: Vec<&[TerminalCell]> = logical_line.chunks(new_cols).collect();
            let num_chunks = chunks.len();
            for (ci, chunk) in chunks.into_iter().enumerate() {
                let mut cells = chunk.to_vec();
                if cells.len() < new_cols {
                    cells.resize(new_cols, blank_cell.clone());
                }
                result.push(ScrollbackLine {
                    cells,
                    is_wrapped: ci + 1 < num_chunks,
                });
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

        self.grid.resize(rows, cols, blank_cell.clone());
        self.alt_grid.resize(rows, cols, blank_cell.clone());

        // CRITICAL: Sync row_versions size with grid size to prevent dirty mark loss
        // When grid grows, we need to extend row_versions; when it shrinks, truncate it
        if rows != self.row_versions.len() {
            self.row_versions.resize(rows, self.grid_version);
        }

        self.scroll_offset = 0;
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.saved_cursor_row = self.saved_cursor_row.min(rows.saturating_sub(1));
        self.saved_cursor_col = self.saved_cursor_col.min(cols.saturating_sub(1));
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

    /// Returns the selected column range for a given viewport row.
    /// Returns None if row is not selected, or Some((start_col, end_col)) where:
    /// - For partial row selection: actual column range
    /// - For fully-selected middle rows: (0, usize::MAX)
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

        let col_start = if abs_row == start.0 { start.1 } else { 0 };
        let col_end = if abs_row == end.0 { end.1 } else { usize::MAX };
        Some((col_start, col_end))
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

#[cfg(test)]
mod tests {
    use super::{ClipboardReadKind, Color, TerminalState};

    #[test]
    fn resize_preserves_full_screen_scroll_region() {
        let mut terminal = TerminalState::new(4, 3);

        terminal.on_resize(4, 6);

        assert_eq!(terminal.scroll_region_top, 0);
        assert_eq!(terminal.scroll_region_bottom, 5);
    }

    #[test]
    fn linefeed_at_bottom_pushes_to_scrollback_for_full_screen_region() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid[0][0].character = 'A';
        terminal.grid[1][0].character = 'B';
        terminal.cursor_row = 1;
        terminal.cursor_col = 0;

        terminal.process_input(b"\n");

        assert_eq!(terminal.scrollback.len(), 1);
        assert_eq!(terminal.scrollback[0].cells[0].character, 'A');
        assert_eq!(terminal.grid[0][0].character, 'B');
        assert_eq!(terminal.grid[1][0].character, ' ');
    }

    #[test]
    fn visible_cells_keep_rectangular_shape_after_resize_with_scrollback() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;

        terminal.process_input(b"\n");
        terminal.on_resize(5, 2);
        terminal.scroll(1);

        let visible = terminal.get_visible_cells();

        assert_eq!(visible.len(), 2);
        assert!(visible.iter().all(|row| row.len() == 5));
        assert_eq!(visible[0][0].character, 'A');
        assert_eq!(visible[0][4].character, ' ');
    }

    #[test]
    fn cursor_is_hidden_while_viewing_scrollback() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;

        terminal.process_input(b"\n");

        assert!(terminal.is_cursor_visible());

        terminal.scroll(1);

        assert!(!terminal.is_cursor_visible());
    }

    #[test]
    fn scroll_to_bottom_restores_live_cursor_visibility() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;

        terminal.process_input(b"\n");
        terminal.scroll(1);
        terminal.scroll_to_bottom();

        assert_eq!(terminal.scroll_offset, 0);
        assert!(terminal.is_cursor_visible());
    }

    #[test]
    fn sgr_39_and_49_restore_default_colors() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[36;44mA\x1b[39;49mB");

        let first = &terminal.grid[0][0];
        let second = &terminal.grid[0][1];

        assert_eq!(first.foreground, Color::Cyan);
        assert_eq!(first.background, Color::Blue);
        assert_eq!(second.foreground, Color::Default);
        assert_eq!(second.background, Color::Default);
    }

    #[test]
    fn cleared_cells_keep_active_background() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[44mAB\x1b[1;1H\x1b[K");

        assert_eq!(terminal.grid[0][0].background, Color::Blue);
        assert_eq!(terminal.grid[0][1].background, Color::Blue);
    }

    #[test]
    fn empty_sgr_sequence_resets_attributes() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[7;36;44mA\x1b[mB");

        let first = &terminal.grid[0][0];
        let second = &terminal.grid[0][1];

        assert!(first.flags.inverse);
        assert_eq!(first.foreground, Color::Cyan);
        assert_eq!(first.background, Color::Blue);

        assert!(!second.flags.inverse);
        assert_eq!(second.foreground, Color::Default);
        assert_eq!(second.background, Color::Default);
    }

    #[test]
    fn split_truecolor_sequence_does_not_leak_text() {
        let mut terminal = TerminalState::new(32, 2);

        terminal.process_input(b"\x1b[38");
        terminal.process_input(b";2;81;175;239msrc");

        assert_eq!(terminal.grid[0][0].character, 's');
        assert_eq!(terminal.grid[0][1].character, 'r');
        assert_eq!(terminal.grid[0][2].character, 'c');
        assert_eq!(terminal.grid[0][0].foreground, Color::Rgb(81, 175, 239));
    }

    #[test]
    fn trailing_escape_is_buffered_until_next_chunk() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b");
        terminal.process_input(b"[31mX");

        assert_eq!(terminal.grid[0][0].character, 'X');
        assert_eq!(terminal.grid[0][0].foreground, Color::Red);
    }

    #[test]
    fn dec_special_graphics_charset_maps_line_drawing() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b(0qx\x0fA");

        assert_eq!(terminal.grid[0][0].character, '─');
        assert_eq!(terminal.grid[0][1].character, '│');
        assert_eq!(terminal.grid[0][2].character, 'A');
    }

    #[test]
    fn decscusr_with_intermediate_space_does_not_leak_text() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[0 qX");

        assert_eq!(terminal.grid[0][0].character, 'X');
    }

    #[test]
    fn private_csi_u_sequence_does_not_restore_cursor_or_leak() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"AB");
        terminal.process_input(b"\x1b[?4uC");

        assert_eq!(terminal.grid[0][0].character, 'A');
        assert_eq!(terminal.grid[0][1].character, 'B');
        assert_eq!(terminal.grid[0][2].character, 'C');
    }

    #[test]
    fn csi_with_gt_prefix_is_consumed_without_printing_parameters() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[>4;1mZ");

        assert_eq!(terminal.grid[0][0].character, 'Z');
        assert_eq!(terminal.grid[0][1].character, ' ');
    }

    #[test]
    fn dcs_sequence_is_consumed_without_leaking_text() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1bP$q q\x1b\\X");

        assert_eq!(terminal.grid[0][0].character, 'X');
        assert_eq!(terminal.grid[0][1].character, ' ');
    }

    #[test]
    fn primary_and_secondary_device_attributes_are_reported() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[c\x1b[>c");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[?65;1;9c\x1b[>1;7802;0c"
        );
    }

    #[test]
    fn xtversion_query_is_reported() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[>0q");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1bP>|VTE(7802)\x1b\\"
        );
    }

    #[test]
    fn double_click_selects_full_url() {
        let mut terminal = TerminalState::new(64, 2);

        terminal.process_input(b"see https://example.com/path?a=1&b=2 now");
        terminal.select_word_at(0, 12);

        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("https://example.com/path?a=1&b=2")
        );
    }

    #[test]
    fn double_click_selects_file_path_with_line_number() {
        let mut terminal = TerminalState::new(64, 2);

        terminal.process_input(b"open src/main.rs:1480 please");
        terminal.select_word_at(0, 8);

        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("src/main.rs:1480")
        );
    }

    #[test]
    fn double_click_excludes_wrapping_punctuation() {
        let mut terminal = TerminalState::new(64, 2);

        terminal.process_input(b"(https://example.com/path), next");
        terminal.select_word_at(0, 10);

        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("https://example.com/path")
        );
    }

    #[test]
    fn bracketed_paste_mode_is_tracked() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[?2004h");
        assert!(terminal.is_bracketed_paste_enabled());

        terminal.process_input(b"\x1b[?2004l");
        assert!(!terminal.is_bracketed_paste_enabled());
    }

    #[test]
    fn kitty_keyboard_flags_can_be_set_queried_and_popped() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[=1u");
        assert_eq!(terminal.keyboard_enhancement_flags(), 1);

        terminal.process_input(b"\x1b[?u");
        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[?1u"
        );

        terminal.process_input(b"\x1b[>5u");
        assert_eq!(terminal.keyboard_enhancement_flags(), 5);

        terminal.process_input(b"\x1b[<u");
        assert_eq!(terminal.keyboard_enhancement_flags(), 1);
    }

    #[test]
    fn xtmodkeys_and_xtfmtkeys_state_is_tracked() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[>4;2m\x1b[>4;1f");

        assert_eq!(terminal.xterm_modify_other_keys(), 2);
        assert_eq!(terminal.xterm_format_other_keys(), 1);
    }

    #[test]
    fn vte_report_all_keys_mode_is_tracked() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[?2031h");
        assert!(terminal.is_report_all_keys_enabled());

        terminal.process_input(b"\x1b[?2031l");
        assert!(!terminal.is_report_all_keys_enabled());
    }

    #[test]
    fn osc_5522_read_request_is_queued() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b]5522;type=read;Lg==\x1b\\");

        let requests = terminal.take_clipboard_read_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].kind, ClipboardReadKind::MimeList);
    }

    #[test]
    fn decrqm_reports_5522_support() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[?5522$p");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[?5522;2$y"
        );
    }
}
