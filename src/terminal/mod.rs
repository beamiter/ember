use crate::kitty_graphics::KittyGraphicsState;
use base64::Engine;
use smallvec::SmallVec;
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
/// Hard cap on bytes carried across PTY read batches inside an unfinished
/// OSC/DCS/CSI escape. Any well-formed sequence is far below this; a
/// runaway/binary stream that never sends a terminator (BEL/ST/final byte)
/// would otherwise grow `pending_escape` without bound.
pub const MAX_PENDING_ESCAPE: usize = 4 * 1024 * 1024;

/// Hard cap on tracked OSC 133 command marks. Each mark is a few u64s, so
/// 1024 ≈ 32 KiB; well beyond any reasonable session's prompt count, but
/// bounded so a malicious shell can't grow this without limit.
pub const MAX_COMMAND_MARKS: usize = 1024;

/// One entry recorded by an OSC 133-aware shell. `line_id` is the monotonic
/// id of the row where the prompt began; we resolve it to a current
/// scrollback/grid row at navigation time, since scrollback indices shift
/// when old lines get evicted.
#[derive(Clone, Copy, Debug)]
pub struct CommandMark {
    /// Monotonic line id of the prompt row, equal to total_lines_scrolled
    /// at record time plus the prompt's viewport row.
    pub line_id: u64,
    /// Exit code reported by `OSC 133;D;<n>`. None until the command exits.
    pub exit_code: Option<i32>,
}

pub fn clamp_terminal_dimensions(cols: usize, rows: usize) -> (usize, usize) {
    (
        cols.clamp(1, MAX_TERMINAL_COLS),
        rows.clamp(1, MAX_TERMINAL_ROWS),
    )
}

mod grid;
mod parser;
mod state;
#[cfg(test)]
mod tests;

pub use grid::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMode {
    Normal,
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, usize),
    pub active: (usize, usize),
    pub mode: SelectionMode,
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

#[derive(Clone, Debug, Default)]
struct TerminalModes {
    bits: u32,
}

impl TerminalModes {
    const fn bit_index(mode: u16) -> Option<u32> {
        match mode {
            7 => Some(0),
            25 => Some(1),
            1000 => Some(2),
            1001 => Some(3),
            1002 => Some(4),
            1003 => Some(5),
            1004 => Some(6),
            1006 => Some(7),
            1049 => Some(8),
            2004 => Some(9),
            2026 => Some(10),
            2031 => Some(11),
            5522 => Some(12),
            _ => None,
        }
    }

    #[inline]
    fn contains(&self, mode: &u16) -> bool {
        match Self::bit_index(*mode) {
            Some(bit) => self.bits & (1 << bit) != 0,
            None => false,
        }
    }

    #[inline]
    fn insert(&mut self, mode: u16) {
        if let Some(bit) = Self::bit_index(mode) {
            self.bits |= 1 << bit;
        }
    }

    #[inline]
    fn remove(&mut self, mode: &u16) {
        if let Some(bit) = Self::bit_index(*mode) {
            self.bits &= !(1 << bit);
        }
    }
}

/// DECSC/DECRC 保存的完整光标状态(含 SGR 属性、字符集、模式),
/// 与备用屏幕缓冲使用的 saved_cursor_row/col 解耦。
#[derive(Clone)]
struct SavedCursorState {
    row: usize,
    col: usize,
    fg: Color,
    bg: Color,
    flags: StyleFlags,
    g0: Charset,
    g1: Charset,
    active: Charset,
    origin_mode: bool,
    autowrap: bool,
    // VT510: DECSC 须保存 Last Column Flag(延迟换行标志),DECRC 须恢复。
    // 否则右 prompt(如 starship cmd_duration)写到末列后 pending_wrap=true,
    // 经 CSI u/ESC 8 恢复光标到 prompt 之后,下一字符(如 zsh-autosuggestions 的
    // ghost 文本)会立刻触发换行,在屏底触发滚动,光标看似下移一行。
    pending_wrap: bool,
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

    // DECSC/DECRC 完整保存状态
    saved_state: Option<SavedCursorState>,
    // IRM 插入模式 (ANSI mode 4):写字符时右移而非覆盖
    insert_mode: bool,
    // DECOM 原点模式 (DEC private ?6):光标寻址相对滚动区域顶端
    origin_mode: bool,
    // 自定义制表位 (HTS/TBC),index 为列,true 表示该列是制表位
    tab_stops: Vec<bool>,
    // DEC 延迟换行 (Last Column Flag):写满最后一列后置位,下一个可打印字符才换行
    pending_wrap: bool,

    current_fg: Color,
    current_bg: Color,
    current_flags: StyleFlags,
    pub window_title: String,
    /// Working directory reported by the shell via OSC 7
    /// (`ESC ] 7 ; file://host/path ST`). Optional because many shells need
    /// PROMPT_COMMAND wiring to emit it. When absent the session manager
    /// falls back to /proc/[pid]/cwd. Survives across shell PWD changes —
    /// each prompt re-emits OSC 7.
    pub current_working_dir: Option<String>,

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

    modes: TerminalModes,

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
    visible_cells_cache: Option<(u64, usize, std::sync::Arc<Vec<Vec<TerminalCell>>>)>,

    // OSC 8 hyperlink tracking
    current_hyperlink: Option<(String, Option<String>)>, // (url, id)

    // Synchronized output (mode 2026): suppress rendering until mode is cleared
    pub sync_output_active: bool,
    sync_output_start: Option<std::time::Instant>,
    last_archived_screen_snapshot: Vec<String>,
    last_synced_primary_screen_snapshot: Vec<String>,

    // OSC 52 clipboard set requests (selection_param, decoded_text)
    pub pending_osc52_clipboard_set: Option<String>,
    // OSC 52 clipboard query pending (needs clipboard read + response)
    pub pending_osc52_clipboard_query: bool,

    // OSC 10/11/12 dynamic colors
    pub dynamic_fg: Option<(u8, u8, u8)>,
    pub dynamic_bg: Option<(u8, u8, u8)>,
    pub dynamic_cursor_color: Option<(u8, u8, u8)>,

    // OSC 9/777 pending notifications
    pub pending_notifications: Vec<(String, String)>,

    /// Total lines ever pushed into `scrollback` (does not decrement on
    /// `pop_front`). Combined with current `scrollback.len()`, lets us
    /// translate a stable `line_id` back to a current scrollback index even
    /// after old lines have been evicted.
    pub total_lines_scrolled: u64,

    /// OSC 133 command boundaries recorded by FinalTerm-aware shells. FIFO,
    /// capped at `MAX_COMMAND_MARKS`. Marks pointing to lines that have been
    /// evicted from scrollback are pruned lazily during navigation.
    pub command_marks: VecDeque<CommandMark>,
}
