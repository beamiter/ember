use crate::color;
use crate::gpu;
use crate::terminal::{clamp_terminal_dimensions, TerminalState};
use crate::theme::ThemeExt as _;
use egui::{Color32, FontId, Response, Ui, Vec2};
use lru::LruCache;
use std::borrow::Cow;
use std::num::NonZeroUsize;

/// Kitty reserves the lower half of the i32 z-index range for images that
/// must sit beneath non-default cell backgrounds. Other negative values sit
/// above cell backgrounds but below glyphs and decorations.
#[derive(Clone, Copy)]
enum KittyImageLayer {
    BelowCellBackgrounds,
    BelowText,
    AboveText,
}

impl KittyImageLayer {
    const BACKGROUND_CUTOFF: i32 = i32::MIN / 2;

    fn contains(self, z_index: i32) -> bool {
        match self {
            Self::BelowCellBackgrounds => z_index < Self::BACKGROUND_CUTOFF,
            Self::BelowText => (Self::BACKGROUND_CUTOFF..0).contains(&z_index),
            Self::AboveText => z_index >= 0,
        }
    }
}

/// Per-column ligature override produced by shaping a printable-ASCII run.
#[derive(Clone, Copy)]
enum LigOverride {
    /// This column is the anchor of a shaped (possibly multi-cell) glyph.
    Glyph {
        region: gpu::font_backend::GlyphRegion,
    },
    /// This column is consumed by a ligature anchored at an earlier column;
    /// suppress its foreground glyph (background/underline stay per-cell).
    Covered,
}

fn snapped_span(origin: f32, index: usize, cell_size: f32) -> (f32, f32) {
    let start = (origin + index as f32 * cell_size).round();
    let end = (origin + (index + 1) as f32 * cell_size).round();
    (start, (end - start).max(1.0))
}

fn hovered_link_color() -> Color32 {
    Color32::from_rgb(100, 200, 255)
}

fn viewport_search_map<'a>(
    terminal: &TerminalState,
    matches: &'a [crate::search::SearchMatch],
    rows: usize,
) -> Vec<Vec<&'a crate::search::SearchMatch>> {
    let mut map = vec![Vec::new(); rows];
    if !terminal.viewport_buffer_mapping_is_exact() {
        return map;
    }
    for search_match in matches {
        if let Some(viewport_row) = search_match
            .viewport_row(terminal)
            .filter(|viewport_row| *viewport_row < rows)
        {
            map[viewport_row].push(search_match);
        }
    }
    map
}

fn cursor_rect(
    rect: egui::Rect,
    row: usize,
    col: usize,
    char_width: f32,
    line_height: f32,
) -> egui::Rect {
    let (x, width) = snapped_span(rect.left(), col, char_width);
    let (y, height) = snapped_span(rect.top(), row, line_height);
    egui::Rect::from_min_size(egui::pos2(x, y), Vec2::new(width, height))
}

pub(crate) fn grid_position_from_content(
    pos: egui::Pos2,
    content_rect: egui::Rect,
    char_width: f32,
    line_height: f32,
    cols: usize,
    rows: usize,
) -> (usize, usize) {
    let clamped_x = (pos.x - content_rect.left()).clamp(0.0, content_rect.width().max(0.0));
    let clamped_y = (pos.y - content_rect.top()).clamp(0.0, content_rect.height().max(0.0));

    let col = if char_width > 0.0 {
        ((clamped_x / char_width) as usize).min(cols.saturating_sub(1))
    } else {
        0
    };
    let row = if line_height > 0.0 {
        ((clamped_y / line_height) as usize).min(rows.saturating_sub(1))
    } else {
        0
    };

    (row, col)
}

fn local_selection_capture_after_press(
    current_terminal: Option<usize>,
    rendered_terminal: usize,
    mouse_reporting: bool,
    interaction_enabled: bool,
    hovered: bool,
    primary_pressed: bool,
    shift: bool,
) -> Option<usize> {
    if !interaction_enabled {
        None
    } else if hovered && primary_pressed {
        (!mouse_reporting || shift).then_some(rendered_terminal)
    } else if current_terminal == Some(rendered_terminal) {
        current_terminal
    } else {
        None
    }
}

fn should_clear_selection_on_click(
    local_selection_enabled: bool,
    ctrl_held: bool,
    clicked: bool,
    double_clicked: bool,
    triple_clicked: bool,
    dragging_scrollbar: bool,
    pointer_in_content: bool,
) -> bool {
    local_selection_enabled
        && !ctrl_held
        && clicked
        && !double_clicked
        && !triple_clicked
        && !dragging_scrollbar
        && pointer_in_content
}

fn key_to_terminal_sequence(
    key: egui::Key,
    modifiers: egui::Modifiers,
    application_cursor_keys: bool,
) -> Option<Cow<'static, str>> {
    // The functional-key family carries its modifiers as a CSI parameter, so it
    // must be encoded before the guard below: returning None there is what used
    // to make Ctrl+Left send zero bytes.
    if let Some(sequence) = legacy_function_key_sequence(key, modifiers, application_cursor_keys) {
        return Some(sequence);
    }

    // The remaining keys encode to a bare control byte with no room for a
    // modifier parameter. Their modified chords belong to the shortcut table and
    // to the Ctrl+letter mapping in `handle_keyboard_input`, so staying silent
    // here keeps one keypress from being delivered twice.
    if modifiers.ctrl || modifiers.alt || modifiers.mac_cmd || modifiers.command_only() {
        return None;
    }

    match key {
        egui::Key::Enter => Some(Cow::Borrowed("\r")),
        egui::Key::Escape => Some(Cow::Borrowed("\x1b")),
        egui::Key::Backspace => Some(Cow::Borrowed("\x7f")), // Send DEL (0x7f)
        egui::Key::Tab => {
            if modifiers.shift {
                Some(Cow::Borrowed("\x1b[Z")) // Shift+Tab -> backtab (CSI Z)
            } else {
                Some(Cow::Borrowed("\t"))
            }
        }
        _ => None,
    }
}

/// Encode the legacy xterm/terminfo functional-key family: cursor keys, the
/// editing block, and F1-F12. A held modifier becomes the standard xterm
/// parameter (`1 + shift + 2*alt + 4*ctrl + 8*meta`) instead of being dropped —
/// without it Ctrl+Left/Ctrl+Right, the word-wise motions every shell binds,
/// produced no bytes at all.
///
/// Each arm also spells out its unmodified bytes verbatim. That redundancy is
/// deliberate: applications look these up through terminfo, so an unmodified
/// press (SS3 form under DECCKM included) must stay byte-identical.
fn legacy_function_key_sequence(
    key: egui::Key,
    modifiers: egui::Modifiers,
    application_cursor_keys: bool,
) -> Option<Cow<'static, str>> {
    let modifier = kitty_modifier_value(modifiers);
    // 1 is the "no modifier" parameter value; xterm omits it entirely.
    let modified = modifier > 1;

    // Shift+PageUp/PageDown belong to the scrollback, as in xterm and in
    // frost, so the viewport is their only handler. `viewport_scroll_delta`
    // scrolls on every non-ctrl Page press, and this path used to also send a
    // sequence for the shifted form — the pane and a full-screen app both moved
    // on one keystroke.
    if modifiers.shift
        && !modifiers.ctrl
        && !modifiers.alt
        && matches!(key, egui::Key::PageUp | egui::Key::PageDown)
    {
        return None;
    }

    // Cursor keys and Home/End follow DECCKM only while unmodified: the
    // parameterized form is always CSI, never SS3.
    let cursor =
        |normal: &'static str, application: &'static str, final_byte: char| -> Cow<'static, str> {
            if modified {
                Cow::Owned(format!("\x1b[1;{modifier}{final_byte}"))
            } else if application_cursor_keys {
                Cow::Borrowed(application)
            } else {
                Cow::Borrowed(normal)
            }
        };
    let tilde = |plain: &'static str, code: u8| -> Cow<'static, str> {
        if modified {
            Cow::Owned(format!("\x1b[{code};{modifier}~"))
        } else {
            Cow::Borrowed(plain)
        }
    };
    // F1-F4 are SS3 unmodified but join the CSI 1;<mod> form once modified;
    // they are not affected by DECCKM.
    let function = |plain: &'static str, final_byte: char| -> Cow<'static, str> {
        if modified {
            Cow::Owned(format!("\x1b[1;{modifier}{final_byte}"))
        } else {
            Cow::Borrowed(plain)
        }
    };

    Some(match key {
        egui::Key::ArrowUp => cursor("\x1b[A", "\x1bOA", 'A'),
        egui::Key::ArrowDown => cursor("\x1b[B", "\x1bOB", 'B'),
        egui::Key::ArrowRight => cursor("\x1b[C", "\x1bOC", 'C'),
        egui::Key::ArrowLeft => cursor("\x1b[D", "\x1bOD", 'D'),
        egui::Key::Home => cursor("\x1b[H", "\x1bOH", 'H'),
        egui::Key::End => cursor("\x1b[F", "\x1bOF", 'F'),
        egui::Key::Insert => tilde("\x1b[2~", 2),
        egui::Key::Delete => tilde("\x1b[3~", 3),
        egui::Key::PageUp => tilde("\x1b[5~", 5),
        egui::Key::PageDown => tilde("\x1b[6~", 6),
        egui::Key::F1 => function("\x1bOP", 'P'),
        egui::Key::F2 => function("\x1bOQ", 'Q'),
        egui::Key::F3 => function("\x1bOR", 'R'),
        egui::Key::F4 => function("\x1bOS", 'S'),
        egui::Key::F5 => tilde("\x1b[15~", 15),
        egui::Key::F6 => tilde("\x1b[17~", 17),
        egui::Key::F7 => tilde("\x1b[18~", 18),
        egui::Key::F8 => tilde("\x1b[19~", 19),
        egui::Key::F9 => tilde("\x1b[20~", 20),
        egui::Key::F10 => tilde("\x1b[21~", 21),
        egui::Key::F11 => tilde("\x1b[23~", 23),
        egui::Key::F12 => tilde("\x1b[24~", 24),
        _ => return None,
    })
}

fn kitty_text_key_code(key: egui::Key) -> Option<u32> {
    match key {
        egui::Key::A => Some('a' as u32),
        egui::Key::B => Some('b' as u32),
        egui::Key::C => Some('c' as u32),
        egui::Key::D => Some('d' as u32),
        egui::Key::E => Some('e' as u32),
        egui::Key::F => Some('f' as u32),
        egui::Key::G => Some('g' as u32),
        egui::Key::H => Some('h' as u32),
        egui::Key::I => Some('i' as u32),
        egui::Key::J => Some('j' as u32),
        egui::Key::K => Some('k' as u32),
        egui::Key::L => Some('l' as u32),
        egui::Key::M => Some('m' as u32),
        egui::Key::N => Some('n' as u32),
        egui::Key::O => Some('o' as u32),
        egui::Key::P => Some('p' as u32),
        egui::Key::Q => Some('q' as u32),
        egui::Key::R => Some('r' as u32),
        egui::Key::S => Some('s' as u32),
        egui::Key::T => Some('t' as u32),
        egui::Key::U => Some('u' as u32),
        egui::Key::V => Some('v' as u32),
        egui::Key::W => Some('w' as u32),
        egui::Key::X => Some('x' as u32),
        egui::Key::Y => Some('y' as u32),
        egui::Key::Z => Some('z' as u32),
        egui::Key::Num0 => Some('0' as u32),
        egui::Key::Num1 => Some('1' as u32),
        egui::Key::Num2 => Some('2' as u32),
        egui::Key::Num3 => Some('3' as u32),
        egui::Key::Num4 => Some('4' as u32),
        egui::Key::Num5 => Some('5' as u32),
        egui::Key::Num6 => Some('6' as u32),
        egui::Key::Num7 => Some('7' as u32),
        egui::Key::Num8 => Some('8' as u32),
        egui::Key::Num9 => Some('9' as u32),
        _ => None,
    }
}

fn text_key_code(key: egui::Key, modifiers: egui::Modifiers) -> Option<u32> {
    let codepoint = kitty_text_key_code(key)?;
    if modifiers.shift {
        Some(match key {
            egui::Key::A => 'A' as u32,
            egui::Key::B => 'B' as u32,
            egui::Key::C => 'C' as u32,
            egui::Key::D => 'D' as u32,
            egui::Key::E => 'E' as u32,
            egui::Key::F => 'F' as u32,
            egui::Key::G => 'G' as u32,
            egui::Key::H => 'H' as u32,
            egui::Key::I => 'I' as u32,
            egui::Key::J => 'J' as u32,
            egui::Key::K => 'K' as u32,
            egui::Key::L => 'L' as u32,
            egui::Key::M => 'M' as u32,
            egui::Key::N => 'N' as u32,
            egui::Key::O => 'O' as u32,
            egui::Key::P => 'P' as u32,
            egui::Key::Q => 'Q' as u32,
            egui::Key::R => 'R' as u32,
            egui::Key::S => 'S' as u32,
            egui::Key::T => 'T' as u32,
            egui::Key::U => 'U' as u32,
            egui::Key::V => 'V' as u32,
            egui::Key::W => 'W' as u32,
            egui::Key::X => 'X' as u32,
            egui::Key::Y => 'Y' as u32,
            egui::Key::Z => 'Z' as u32,
            _ => codepoint,
        })
    } else {
        Some(codepoint)
    }
}

fn kitty_modifier_value(modifiers: egui::Modifiers) -> u8 {
    let mut bits = 0u8;
    if modifiers.shift {
        bits |= 0b1;
    }
    if modifiers.alt {
        bits |= 0b10;
    }
    if modifiers.ctrl {
        bits |= 0b100;
    }
    if modifiers.command && !modifiers.ctrl {
        bits |= 0b1000;
    }
    bits + 1
}

fn consumed_key_name(key: egui::Key, modifiers: egui::Modifiers) -> String {
    let mut parts = Vec::new();
    if modifiers.ctrl {
        parts.push("Ctrl");
    }
    if modifiers.shift {
        parts.push("Shift");
    }
    if modifiers.alt {
        parts.push("Alt");
    }
    if modifiers.mac_cmd || (modifiers.command && !modifiers.ctrl) {
        parts.push("Cmd");
    }
    parts.push(match key {
        egui::Key::Insert => "Insert",
        egui::Key::C => "C",
        egui::Key::V => "V",
        egui::Key::X => "X",
        egui::Key::Plus => "Plus",
        egui::Key::Equals => "Equals",
        egui::Key::Minus => "Minus",
        egui::Key::Num0 => "0",
        _ => return String::new(),
    });
    parts.join("+")
}

fn kitty_encode_key_event(
    key: egui::Key,
    modifiers: egui::Modifiers,
    keyboard_flags: u16,
) -> Option<String> {
    let disambiguate = (keyboard_flags & 0b1) != 0;
    let report_all_keys = (keyboard_flags & 0b1000) != 0;
    if !disambiguate && !report_all_keys {
        return None;
    }

    let codepoint = kitty_text_key_code(key)?;
    let should_encode = report_all_keys || modifiers.ctrl || modifiers.alt || modifiers.command;
    if !should_encode {
        return None;
    }

    Some(format!(
        "\x1b[{};{}u",
        codepoint,
        kitty_modifier_value(modifiers)
    ))
}

fn xterm_encode_modify_other_keys(
    key: egui::Key,
    modifiers: egui::Modifiers,
    modify_other_keys: u16,
    format_other_keys: u16,
    report_all_keys: bool,
) -> Option<String> {
    let codepoint = text_key_code(key, modifiers)?;
    let modifier_value = kitty_modifier_value(modifiers);
    let has_non_shift_modifier = modifiers.ctrl || modifiers.alt || modifiers.command;
    let should_encode = if report_all_keys {
        modifier_value > 1
    } else {
        match modify_other_keys {
            0 => false,
            1 => modifiers.alt || (modifiers.command && !modifiers.ctrl),
            _ => has_non_shift_modifier || modifiers.shift,
        }
    };

    if !should_encode {
        return None;
    }

    if format_other_keys == 1 || report_all_keys {
        Some(format!("\x1b[{};{}u", codepoint, modifier_value))
    } else {
        Some(format!("\x1b[27;{};{}~", modifier_value, codepoint))
    }
}

/// Per-frame owned snapshot of one visible command block, used for both the
/// gutter click hit test and the chrome painting.
struct BlockChromeEntry {
    id: String,
    span: crate::block_mode::VisibleBlockSpan,
    outcome: crate::block_mode::BlockOutcome,
    duration_ms: Option<u64>,
    /// Finish time, appended to the badge while the block is selected.
    finished_at: Option<std::time::SystemTime>,
}

pub struct TerminalRenderer {
    pub font_size: f32,
    pub char_width: f32,
    pub line_height: f32,
    pub padding: f32,
    pub line_spacing: f32,
    pub dragging_scrollbar: bool,
    /// Stable terminal identity selected by a local primary-button press.
    /// This locks Shift bypass through release and prevents a renderer reused
    /// by another tab/pane from applying an in-flight drag to the new terminal.
    local_selection_terminal: Option<usize>,
    pub scrollbar_visibility: crate::config::ScrollbarVisibility,
    pub theme: crate::theme::Theme,
    requested_initial_focus: bool,
    ime_enabled: bool,
    last_ime_rect: Option<egui::Rect>,
    // Kitty graphics texture cache with count and byte-budget eviction.
    texture_cache: LruCache<u32, (egui::TextureHandle, u32, u32, u64)>,
    texture_cache_bytes: usize,
    /// The content rect from the last render, used for mouse-to-grid coordinate conversion
    pub last_content_rect: Option<egui::Rect>,
    pub opacity: f32,
    /// Whether to enable font ligatures (HarfRust shaping of ASCII runs)
    pub font_ligatures: bool,
    /// Whether a plain click places the shell's edit cursor (`click_moves_cursor`)
    pub click_moves_cursor: bool,
    /// Whether to draw command-block chrome (`block_mode`): gutter stripes,
    /// separators and outcome badges derived from OSC 133 records.
    pub block_mode: bool,
    /// Record id of the app-selected block for the terminal this renderer is
    /// about to draw. Set by the app each frame; an id that matches no record
    /// simply draws nothing (selection must never dangle).
    pub selected_block_id: Option<String>,
    /// Block-mode hit-test outcome of this frame's click, drained by the app
    /// the way `cursor_move_input` is.
    pub block_click: Option<crate::block_mode::BlockClick>,
    /// Whether to use GPU-accelerated grid rendering
    pub gpu_rendering: bool,
    /// wgpu render state for GPU-accelerated grid rendering
    pub wgpu_render_state: Option<egui_wgpu::RenderState>,
    /// Stable key for this renderer's per-surface GPU buffers. egui-wgpu
    /// prepares all callbacks before painting any of them, so pane renderers
    /// must not share instance or uniform storage.
    gpu_surface_id: gpu::callback::GridSurfaceId,
    /// Pending cursor movement input (arrow keys) from mouse clicks
    pub cursor_move_input: Vec<u8>,
    /// Stable identity of the terminal that produced `cursor_move_input`.
    /// Renderers are reused when tabs/panes switch, so bytes without this tag
    /// could otherwise be delivered to the replacement PTY next frame.
    pub cursor_move_terminal_ptr: Option<usize>,
    /// Sub-line pixel offset for smooth scrolling animation
    pub scroll_pixel_offset: f32,

    /// Per-pane 链接检测缓存(多窗格路径用)。单窗格走 App 上的缓存,多窗格每个
    /// pane 一份,仅当 grid_version/scroll_offset 变化时重建,避免每帧重做检测+String 分配。
    /// 用 Arc 以便渲染前 O(1) clone 出来,规避 &mut self 与字段 & 的借用冲突。
    pub cached_links: std::sync::Arc<Vec<crate::link::Link>>,
    pub cached_links_grid_version: u64,
    pub cached_links_scroll_offset: usize,
    pub cached_links_terminal_ptr: usize,

    // Dirty-region rendering cache
    cached_instances: std::sync::Arc<Vec<gpu::instance::CellInstance>>,
    row_instance_offsets: std::sync::Arc<Vec<usize>>,
    row_instance_counts: std::sync::Arc<Vec<usize>>,
    last_rendered_grid_version: u64,
    last_rendered_scroll_offset: usize,
    last_rendered_selection: Option<crate::terminal::Selection>,
    last_rendered_search_hash: u64,
    last_search_match_lines: Vec<usize>,
    last_rendered_hovered_link: Option<crate::link::Link>,
    last_rendered_cols: usize,
    last_rendered_rows: usize,
    last_rendered_terminal_ptr: usize,
    dirty_rows: std::sync::Arc<Vec<bool>>,
    changed_rows_buffer: Vec<usize>,
    row_instances_scratch: Vec<gpu::instance::CellInstance>,
    cached_atlas_w: f32,
    cached_atlas_h: f32,
    last_rendered_font_generation: (u64, u64),
}

impl TerminalRenderer {
    const SCROLLBAR_WIDTH: f32 = 8.0;
    const SCROLLBAR_GAP: f32 = 2.0;
    const MIN_THUMB_HEIGHT: f32 = 24.0;
    const MIN_SPLIT_COLS: f32 = 8.0;
    const MIN_SPLIT_ROWS: f32 = 3.0;
    const SCROLLBAR_HIT_EXPAND: f32 = 8.0;
    /// This is per renderer (split panes have independent caches), so keep it
    /// tight enough that several panes cannot multiply into a multi-GiB VRAM
    /// allocation under adversarial terminal output.
    const MAX_KITTY_TEXTURE_CACHE_BYTES: usize = 64 * 1024 * 1024;
    const MAX_KITTY_TEXTURE_CACHE_ENTRIES: usize = 100;

    pub fn new(
        font_size: f32,
        padding: f32,
        line_spacing: f32,
        scrollbar_visibility: crate::config::ScrollbarVisibility,
        theme: crate::theme::Theme,
    ) -> Self {
        // For monospace fonts, approximate char_width is around 0.5x font_size
        // This is an initial estimate before sync_font_metrics is called
        let char_width = font_size * 0.5;
        let line_height = font_size * line_spacing;

        TerminalRenderer {
            font_size,
            char_width,
            line_height,
            padding,
            line_spacing,
            dragging_scrollbar: false,
            local_selection_terminal: None,
            scrollbar_visibility,
            theme,
            requested_initial_focus: false,
            ime_enabled: false,
            last_content_rect: None,
            last_ime_rect: None,
            opacity: 1.0,
            font_ligatures: true,
            click_moves_cursor: jterm_core::click_cursor::ENABLED_BY_DEFAULT,
            block_mode: true,
            selected_block_id: None,
            block_click: None,
            gpu_rendering: true,
            texture_cache: LruCache::new(NonZeroUsize::new(100).unwrap()),
            texture_cache_bytes: 0,
            wgpu_render_state: None,
            gpu_surface_id: gpu::callback::GridSurfaceId::allocate(),
            cursor_move_input: Vec::new(),
            cursor_move_terminal_ptr: None,
            scroll_pixel_offset: 0.0,
            cached_links: std::sync::Arc::new(Vec::new()),
            // u64::MAX 作哨兵,保证首帧必定重建链接缓存。
            cached_links_grid_version: u64::MAX,
            cached_links_scroll_offset: usize::MAX,
            cached_links_terminal_ptr: usize::MAX,
            // Dirty-region rendering cache (initialized empty)
            cached_instances: std::sync::Arc::new(Vec::new()),
            row_instance_offsets: std::sync::Arc::new(Vec::new()),
            row_instance_counts: std::sync::Arc::new(Vec::new()),
            last_rendered_grid_version: 0,
            last_rendered_scroll_offset: 0,
            last_rendered_selection: None,
            last_rendered_search_hash: 0,
            last_search_match_lines: Vec::new(),
            last_rendered_hovered_link: None,
            last_rendered_cols: 0,
            last_rendered_rows: 0,
            last_rendered_terminal_ptr: 0,
            dirty_rows: std::sync::Arc::new(Vec::new()),
            changed_rows_buffer: Vec::new(),
            row_instances_scratch: Vec::new(),
            cached_atlas_w: 1.0,
            cached_atlas_h: 1.0,
            last_rendered_font_generation: (0, 0),
        }
    }

    /// 重置 renderer 的 IME 状态缓存，使下一帧重新同步 IME 状态
    pub fn reset_ime_state(&mut self) {
        self.ime_enabled = false;
        self.last_ime_rect = None;
    }

    pub fn cancel_local_selection_capture(&mut self) {
        self.local_selection_terminal = None;
    }

    pub fn invalidate_font_cache(&mut self) {
        self.cached_instances = std::sync::Arc::new(Vec::new());
        self.last_rendered_grid_version = 0;
    }

    pub fn sync_font_metrics(&mut self, ctx: &egui::Context) {
        // When GPU rendering is active, derive cell size from the GPU atlas font
        // metrics (ascent + |descent| and advance width) which give tighter spacing
        // than egui's row_height (which includes extra leading).
        if self.gpu_rendering {
            if let Some(render_state) = &self.wgpu_render_state {
                let ppp = ctx.pixels_per_point();
                let renderer = render_state.renderer.read();
                if let Some(gpu_res) = renderer
                    .callback_resources
                    .get::<gpu::callback::GpuResources>()
                {
                    let (ascent, descent, advance) = gpu_res.atlas.font_metrics();
                    // Convert from physical pixels back to logical points
                    let cw = advance / ppp;
                    let ch = ((ascent - descent) / ppp) * self.line_spacing.max(0.5); // descent is negative
                    if cw.is_finite() && cw > 0.0 {
                        self.char_width = cw;
                    }
                    if ch.is_finite() && ch > 0.0 {
                        self.line_height = ch;
                    }
                    return;
                }
            }
        }

        // CPU fallback: use egui font metrics
        let font_id = FontId::monospace(self.font_size);
        let (char_width, line_height) = ctx.fonts_mut(|fonts| {
            let glyph_width = fonts.glyph_width(&font_id, '0');
            let row_height = fonts.row_height(&font_id);
            (glyph_width, row_height)
        });

        if char_width.is_finite() && char_width > 0.0 {
            self.char_width = char_width;
        }

        let line_height = line_height * self.line_spacing.max(0.5);

        if line_height.is_finite() && line_height > 0.0 {
            self.line_height = line_height;
        }
    }

    /// 获取纹理缓存大小（用于性能监控）
    pub fn texture_cache_len(&self) -> usize {
        self.texture_cache.len()
    }

    /// 获取或创建图像纹理
    fn get_image_texture(
        &mut self,
        ctx: &egui::Context,
        image_id: u32,
        image: &crate::kitty_graphics::KittyImage,
    ) -> Option<egui::TextureHandle> {
        // Check cache first (get_mut to update LRU order)
        if let Some((handle, _w, _h, revision)) = self.texture_cache.get(&image_id) {
            if *revision == image.revision {
                return Some(handle.clone());
            }
        }
        if let Some((_handle, width, height, _revision)) = self.texture_cache.pop(&image_id) {
            self.texture_cache_bytes = self
                .texture_cache_bytes
                .saturating_sub(Self::texture_bytes(width, height));
        }

        // Create new texture from image data.
        // 防御性校验：image.data 应为 width*height*4 字节的 RGBA。
        // from_rgba_unmultiplied 内部 assert，不匹配会 panic 整个进程，故先校验。
        let expected = (image.width as usize)
            .checked_mul(image.height as usize)
            .and_then(|px| px.checked_mul(4));
        match expected {
            Some(n) if n == image.data.len() => {}
            _ => {
                log::warn!(
                    "[KITTY_GRAPHICS] Skip texture for image {}: {}x{} expects {:?} bytes, got {}",
                    image_id,
                    image.width,
                    image.height,
                    expected,
                    image.data.len()
                );
                return None;
            }
        }
        let texture_bytes = expected.expect("texture dimensions were validated above");
        if texture_bytes > Self::MAX_KITTY_TEXTURE_CACHE_BYTES {
            log::warn!(
                "[KITTY_GRAPHICS] Skip texture for image {}: {} bytes exceeds GPU cache limit",
                image_id,
                texture_bytes
            );
            return None;
        }
        while self.texture_cache_bytes.saturating_add(texture_bytes)
            > Self::MAX_KITTY_TEXTURE_CACHE_BYTES
            || self.texture_cache.len() >= Self::MAX_KITTY_TEXTURE_CACHE_ENTRIES
        {
            let Some((_id, (_handle, width, height, _revision))) = self.texture_cache.pop_lru()
            else {
                break;
            };
            self.texture_cache_bytes = self
                .texture_cache_bytes
                .saturating_sub(Self::texture_bytes(width, height));
        }
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [image.width as usize, image.height as usize],
            &image.data,
        );
        let handle = ctx.load_texture(
            format!("kitty_{}_{}", image_id, image.revision),
            color_image,
            Default::default(),
        );

        // Cache it (LRU will auto-evict oldest if at capacity)
        let result = handle.clone();
        self.texture_cache.put(
            image_id,
            (handle, image.width, image.height, image.revision),
        );
        self.texture_cache_bytes = self.texture_cache_bytes.saturating_add(texture_bytes);
        Some(result)
    }

    fn texture_bytes(width: u32, height: u32) -> usize {
        (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4)
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_kitty_image_layer(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        terminal: &TerminalState,
        content_rect: egui::Rect,
        char_width: f32,
        line_height: f32,
        layer: KittyImageLayer,
    ) {
        let viewport_rows = i64::try_from(terminal.grid.rows()).unwrap_or(i64::MAX);
        let scroll_offset = terminal.scroll_offset;
        let mut placements: Vec<_> = terminal
            .kitty_graphics
            .get_placements()
            .iter()
            .filter(|placement| {
                let viewport_row = placement.viewport_row(scroll_offset);
                layer.contains(placement.z_index)
                    && viewport_row < viewport_rows
                    && viewport_row.saturating_add(i64::from(placement.height)) > 0
            })
            .collect();
        placements.sort_by_key(|placement| (placement.z_index, placement.image_id));

        let painter = painter.with_clip_rect(content_rect);
        let pixels_per_point = ctx.pixels_per_point().max(0.1);
        for placement in placements {
            let Some(image) = terminal.kitty_graphics.get_image(placement.image_id) else {
                continue;
            };
            let natural_width = placement.source_width as f32 / pixels_per_point;
            let natural_height = placement.source_height as f32 / pixels_per_point;
            let (display_width, full_display_height) =
                match (placement.requested_columns, placement.requested_rows) {
                    (None, None) => (natural_width, natural_height),
                    (Some(columns), None) => {
                        let width = columns as f32 * char_width;
                        (width, width * natural_height / natural_width.max(0.1))
                    }
                    (None, Some(rows)) => {
                        let height = rows as f32 * line_height;
                        (height * natural_width / natural_height.max(0.1), height)
                    }
                    (Some(columns), Some(rows)) => {
                        (columns as f32 * char_width, rows as f32 * line_height)
                    }
                };
            let top_clip =
                (placement.clip_top_rows as f32 * line_height).min(full_display_height.max(0.0));
            let bottom_clip = (placement.clip_bottom_rows as f32 * line_height)
                .min((full_display_height - top_clip).max(0.0));
            let display_height = full_display_height - top_clip - bottom_clip;
            if display_width <= 0.0 || display_height <= 0.0 {
                continue;
            }
            let viewport_row = placement.viewport_row(scroll_offset);
            let image_rect = egui::Rect::from_min_size(
                egui::pos2(
                    content_rect.left()
                        + placement.x as f32 * char_width
                        + placement.cell_x_offset as f32 / pixels_per_point,
                    content_rect.top()
                        + viewport_row as f32 * line_height
                        + placement.cell_y_offset as f32 / pixels_per_point,
                ),
                Vec2::new(display_width, display_height),
            );
            let Some(texture) = self.get_image_texture(ctx, image.id, image) else {
                continue;
            };
            let u0 = placement.source_x as f32 / image.width as f32;
            let u1 = (placement.source_x + placement.source_width) as f32 / image.width as f32;
            let source_v0 = placement.source_y as f32 / image.height as f32;
            let source_v1 =
                (placement.source_y + placement.source_height) as f32 / image.height as f32;
            let source_v_span = source_v1 - source_v0;
            let v0 = source_v0 + source_v_span * top_clip / full_display_height.max(0.1);
            let v1 = source_v1 - source_v_span * bottom_clip / full_display_height.max(0.1);
            let mesh = egui::Mesh {
                indices: vec![0, 1, 2, 0, 2, 3],
                vertices: vec![
                    egui::epaint::Vertex {
                        pos: image_rect.left_top(),
                        uv: egui::pos2(u0, v0),
                        color: Color32::WHITE,
                    },
                    egui::epaint::Vertex {
                        pos: image_rect.right_top(),
                        uv: egui::pos2(u1, v0),
                        color: Color32::WHITE,
                    },
                    egui::epaint::Vertex {
                        pos: image_rect.right_bottom(),
                        uv: egui::pos2(u1, v1),
                        color: Color32::WHITE,
                    },
                    egui::epaint::Vertex {
                        pos: image_rect.left_bottom(),
                        uv: egui::pos2(u0, v1),
                        color: Color32::WHITE,
                    },
                ],
                texture_id: texture.id(),
            };
            painter.add(egui::Shape::mesh(mesh));
        }
    }

    fn content_size(&self, available: Vec2) -> Vec2 {
        let outer_width = (available.x - self.padding * 2.0).max(self.char_width);
        let outer_height = (available.y - self.padding * 2.0).max(self.line_height);
        let reserved_scrollbar_width = (Self::SCROLLBAR_WIDTH + Self::SCROLLBAR_GAP)
            .min((outer_width - self.char_width).max(0.0));

        Vec2::new(
            (outer_width - reserved_scrollbar_width).max(self.char_width),
            outer_height,
        )
    }

    /// Minimum size of each child produced by a split. This is deliberately
    /// based on cells rather than a pane-count cap, so a larger window can
    /// still host more panes while every new terminal remains usable.
    pub fn minimum_split_pane_size(&self) -> Vec2 {
        Vec2::new(
            self.char_width * Self::MIN_SPLIT_COLS
                + self.padding * 2.0
                + Self::SCROLLBAR_WIDTH
                + Self::SCROLLBAR_GAP,
            self.line_height * Self::MIN_SPLIT_ROWS + self.padding * 2.0,
        )
    }

    fn layout_rects(&self, rect: egui::Rect) -> (egui::Rect, egui::Rect) {
        // Deeply nested layouts can restore panes smaller than one cell. Keep
        // all child geometry inside the pane instead of extending a forced
        // one-cell rectangle into its neighbours.
        let safe_padding = if self.padding.is_finite() {
            self.padding.max(0.0)
        } else {
            0.0
        };
        let inset_x = safe_padding.min(rect.width().max(0.0) * 0.5);
        let inset_y = safe_padding.min(rect.height().max(0.0) * 0.5);
        let outer_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + inset_x, rect.top() + inset_y),
            egui::pos2(rect.right() - inset_x, rect.bottom() - inset_y),
        );

        let reserved_scrollbar_width = (Self::SCROLLBAR_WIDTH + Self::SCROLLBAR_GAP)
            .min((outer_rect.width() - self.char_width).max(0.0));
        let content_rect = egui::Rect::from_min_max(
            outer_rect.min,
            egui::pos2(
                (outer_rect.right() - reserved_scrollbar_width).max(outer_rect.left()),
                outer_rect.bottom(),
            ),
        );
        let scrollbar_rect = egui::Rect::from_min_max(
            egui::pos2(
                (outer_rect.right() - Self::SCROLLBAR_WIDTH).max(content_rect.right()),
                outer_rect.top(),
            ),
            outer_rect.max,
        );

        (content_rect, scrollbar_rect)
    }

    /// Snapshot the visible command blocks, or `None` when chrome must not be
    /// drawn: `block_mode` is off, the pane is on the alternate screen, or
    /// visible scrollback was reflowed after a width change (the same gate
    /// search overlays use, via `viewport_buffer_mapping_is_exact`).
    fn compute_block_chrome(
        &self,
        terminal: &TerminalState,
        rows: usize,
    ) -> Option<Vec<BlockChromeEntry>> {
        if !self.block_mode
            || terminal.is_alt_buffer_active()
            || !terminal.viewport_buffer_mapping_is_exact()
        {
            return None;
        }
        let records = terminal.command_records();
        if records.is_empty() {
            return None;
        }
        // Normalize pending-wrap anchors (`column == cols`): the prompt
        // renders on the row after the one the anchor names.
        let cols = terminal.grid.row_len();
        let prompt_line_ids: Vec<u64> = records
            .iter()
            .map(|record| {
                crate::block_mode::prompt_row_line_id(
                    record.prompt_start.line_id,
                    record.prompt_start.column,
                    cols,
                )
            })
            .collect();
        let spans = crate::block_mode::visible_block_spans(
            &prompt_line_ids,
            terminal.viewport_top_line_id(),
            rows,
        );
        let newest = records.len() - 1;
        let running_duration_ms = terminal.running_duration_ms();
        Some(
            spans
                .into_iter()
                .map(|span| {
                    let record = &records[span.record_index];
                    let outcome = crate::block_mode::classify_outcome(
                        record.command.as_deref(),
                        record.exit_code,
                        record.state,
                        record.complete,
                        span.record_index == newest,
                    );
                    BlockChromeEntry {
                        id: record.id.clone(),
                        outcome,
                        duration_ms: if outcome == crate::block_mode::BlockOutcome::Running {
                            running_duration_ms
                        } else {
                            record.duration_ms
                        },
                        finished_at: record.finished_at,
                        span,
                    }
                })
                .collect(),
        )
    }

    /// Outcome → theme color, using the same sources the bottom bar maps
    /// `Tone::Positive`/`Negative` to (ANSI green/red), the cursor accent for
    /// the running block, and disabled text for background/unknown.
    fn block_outcome_color(&self, outcome: crate::block_mode::BlockOutcome) -> Color32 {
        use crate::block_mode::BlockOutcome;
        match outcome {
            BlockOutcome::Success => {
                crate::theme::Theme::rgb_to_color32(self.theme.terminal.ansi_colors[2])
            }
            BlockOutcome::Failed(_) => {
                crate::theme::Theme::rgb_to_color32(self.theme.terminal.ansi_colors[1])
            }
            BlockOutcome::Running => self.theme.cursor_color(),
            BlockOutcome::Prompt | BlockOutcome::Background | BlockOutcome::Unknown => {
                crate::theme::Theme::rgb_to_color32(self.theme.ui.text_disabled)
            }
        }
    }

    /// Paint all block chrome for one pane with plain painter calls, the way
    /// the pane header and bottom bar draw. Never touches the grid instances.
    fn draw_block_chrome(
        &self,
        painter: &egui::Painter,
        entries: &[BlockChromeEntry],
        grid: &[Vec<crate::terminal::TerminalCell>],
        content_rect: egui::Rect,
        char_width: f32,
        line_height: f32,
    ) {
        use crate::block_mode::{self, BlockOutcome};
        const BADGE_PAD_X: f32 = 4.0;
        const BADGE_PAD_Y: f32 = 1.0;
        const BADGE_RIGHT_MARGIN: f32 = 4.0;

        let separator_color = {
            let [r, g, b] = self.theme.ui.text_disabled;
            Color32::from_rgba_unmultiplied(r, g, b, 56)
        };
        let badge_bg = {
            let [r, g, b] = self.theme.tabbar.bg;
            Color32::from_rgba_unmultiplied(r, g, b, 220)
        };
        let badge_font = FontId::proportional(11.0);

        for entry in entries {
            let selected = self.selected_block_id.as_deref() == Some(entry.id.as_str());
            let (top_y, _) = snapped_span(content_rect.top(), entry.span.first_row, line_height);
            let (last_y, last_height) =
                snapped_span(content_rect.top(), entry.span.last_row, line_height);
            let bottom_y = last_y + last_height;

            // 1px separator on the block's first row — only when that row
            // really is the prompt row, not a clipped continuation.
            if entry.span.starts_in_viewport {
                painter.hline(
                    content_rect.left()..=content_rect.right(),
                    top_y + 0.5,
                    egui::Stroke::new(1.0, separator_color),
                );
            }

            // Gutter stripe over the block's visible rows. The live prompt
            // block gets no stripe (separator only).
            if entry.outcome != BlockOutcome::Prompt {
                let color = self.block_outcome_color(entry.outcome);
                let (width, alpha) = if selected {
                    (block_mode::GUTTER_STRIPE_SELECTED_WIDTH, 255)
                } else {
                    (block_mode::GUTTER_STRIPE_WIDTH, 170)
                };
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(content_rect.left(), top_y),
                        egui::pos2(content_rect.left() + width, bottom_y),
                    ),
                    egui::CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
                );
            }

            // Selected block: 1px outline around its visible row span.
            if selected {
                painter.rect_stroke(
                    egui::Rect::from_min_max(
                        egui::pos2(content_rect.left(), top_y),
                        egui::pos2(content_rect.right(), bottom_y),
                    ),
                    egui::CornerRadius::ZERO,
                    egui::Stroke::new(1.0, self.block_outcome_color(entry.outcome)),
                    egui::StrokeKind::Inside,
                );
            }

            // Right-aligned outcome badge on the first row, drawn only when
            // every cell it would cover is blank — never over prompt text.
            if !entry.span.starts_in_viewport {
                continue;
            }
            let Some(text) = block_mode::badge_text(entry.outcome, entry.duration_ms) else {
                continue;
            };
            // 选中块的徽章附带完成时刻(本地时间)。带后缀的徽章放不下时,
            // 先退回无后缀徽章再放弃,避免选中反而让徽章整个消失。
            let mut candidates: Vec<String> = Vec::new();
            if selected {
                if let Some(secs) = entry.finished_at.and_then(block_mode::epoch_secs) {
                    let clock = block_mode::format_local_time_of_day(
                        secs,
                        block_mode::local_utc_offset_secs(secs),
                    );
                    candidates.push(format!("{text} · {clock}"));
                }
            }
            candidates.push(text);
            let color = self.block_outcome_color(entry.outcome);
            // Map wide-char continuation cells to a non-blank marker so the
            // badge never paints over half a glyph.
            let row_chars: Vec<char> = grid
                .get(entry.span.first_row)
                .map(|row| {
                    row.iter()
                        .map(|cell| {
                            if cell.flags.wide_continuation() {
                                '\u{fffd}'
                            } else {
                                cell.character
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            for text in candidates {
                let galley = painter.layout_no_wrap(text, badge_font.clone(), color);
                let text_size = galley.size();
                let bg_size = egui::vec2(
                    text_size.x + 2.0 * BADGE_PAD_X,
                    text_size.y + 2.0 * BADGE_PAD_Y,
                );
                if bg_size.y > line_height {
                    break; // 小字号下徽章会溢出到下一行;高度与后缀无关,直接放弃。
                }
                let bg_rect = egui::Rect::from_min_size(
                    egui::pos2(
                        content_rect.right() - BADGE_RIGHT_MARGIN - bg_size.x,
                        top_y + ((line_height - bg_size.y).max(0.0)) / 2.0,
                    ),
                    bg_size,
                );
                if bg_rect.left() < content_rect.left() || char_width <= 0.0 {
                    continue; // 内容区放不下这个候选,试更短的。
                }
                let start_col =
                    ((bg_rect.left() - content_rect.left()) / char_width).floor() as usize;
                if block_mode::badge_covers_only_blank_cells(&row_chars, start_col) {
                    painter.rect_filled(bg_rect, 3.0, badge_bg);
                    painter.galley(
                        bg_rect.min + egui::vec2(BADGE_PAD_X, BADGE_PAD_Y),
                        galley,
                        color,
                    );
                    // Register a timer only after the running badge actually
                    // paints. Hidden panes are not rendered; alt-screen,
                    // reflow, an offscreen prompt, overlap and insufficient
                    // height/width all exit before this point.
                    if entry.outcome == BlockOutcome::Running {
                        if let Some(elapsed_ms) = entry.duration_ms {
                            painter.ctx().request_repaint_after(
                                block_mode::running_badge_refresh_interval(elapsed_ms),
                            );
                        }
                    }
                    break;
                }
            }
        }
    }

    /// Fractions (0 = track top/oldest retained line, 1 = bottom/newest grid
    /// line) of every FAILED block's first row, for the scrollbar markers.
    /// These are positional hints only: placement uses stable line ids over
    /// the retained buffer and is deliberately NOT gated on
    /// `viewport_buffer_mapping_is_exact` — approximate after reflow is fine.
    /// The scan is bounded by MAX_COMMAND_MARKS (1024) records and only runs
    /// while the scrollbar is actually drawn.
    fn failed_block_marker_fractions(terminal: &TerminalState) -> Vec<f32> {
        let records = terminal.command_records();
        let Some(newest) = records.len().checked_sub(1) else {
            return Vec::new();
        };
        let cols = terminal.grid.row_len();
        let oldest_line_id = terminal
            .total_lines_scrolled
            .saturating_sub(terminal.scrollback.len() as u64);
        let newest_line_id = terminal
            .total_lines_scrolled
            .saturating_add(terminal.grid.rows().saturating_sub(1) as u64);
        records
            .iter()
            .enumerate()
            .filter(|(index, record)| {
                matches!(
                    crate::block_mode::classify_outcome(
                        record.command.as_deref(),
                        record.exit_code,
                        record.state,
                        record.complete,
                        *index == newest,
                    ),
                    crate::block_mode::BlockOutcome::Failed(_)
                )
            })
            .filter_map(|(_, record)| {
                crate::block_mode::scrollbar_marker_fraction(
                    crate::block_mode::prompt_row_line_id(
                        record.prompt_start.line_id,
                        record.prompt_start.column,
                        cols,
                    ),
                    oldest_line_id,
                    newest_line_id,
                )
            })
            .collect()
    }

    fn scrollbar_thumb_height(visible_lines: usize, total_lines: usize, track_height: f32) -> f32 {
        if total_lines == 0 || !track_height.is_finite() || track_height <= 0.0 {
            return 0.0;
        }

        let natural_height = (visible_lines as f32 / total_lines as f32) * track_height;
        natural_height.clamp(Self::MIN_THUMB_HEIGHT.min(track_height), track_height)
    }

    pub fn grid_dimensions(&self, available: Vec2) -> (usize, usize) {
        let content_size = self.content_size(available);
        let usable_width = content_size.x;
        let usable_height = content_size.y;

        let cols = (usable_width / self.char_width).floor().max(1.0) as usize;
        let rows = (usable_height / self.line_height).floor().max(1.0) as usize;

        clamp_terminal_dimensions(cols, rows)
    }

    /// 在指定矩形内渲染（用于多窗格模式）
    // Rendering a pane needs the terminal model plus its transient overlays and geometry.
    #[allow(clippy::too_many_arguments)]
    pub fn render_in_rect(
        &mut self,
        ui: &mut Ui,
        terminal: &mut TerminalState,
        interaction_enabled: bool,
        focus_enabled: bool,
        cursor_visible: bool,
        search_state: &crate::search::SearchState,
        links: &[crate::link::Link],
        hovered_link: &Option<crate::link::Link>,
        target_rect: egui::Rect,
    ) -> Response {
        let rows = terminal.grid.rows();
        let cols = terminal.grid.row_len();

        let line_height = self.line_height;
        let char_width = self.char_width;

        // Allocate in the target rectangle area
        let rect = target_rect;
        let sense = if interaction_enabled {
            egui::Sense::click_and_drag().union(egui::Sense::focusable_noninteractive())
        } else {
            egui::Sense::hover()
        };
        let response = ui.allocate_rect(rect, sense);

        self.render_terminal_at_rect(
            ui,
            terminal,
            interaction_enabled,
            focus_enabled,
            cursor_visible,
            search_state,
            links,
            hovered_link,
            rect,
            response,
            cols,
            rows,
            line_height,
            char_width,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        ui: &mut Ui,
        terminal: &mut TerminalState,
        interaction_enabled: bool,
        cursor_visible: bool,
        search_state: &crate::search::SearchState,
        links: &[crate::link::Link],
        hovered_link: &Option<crate::link::Link>,
    ) -> Response {
        let rows = terminal.grid.rows();
        let cols = terminal.grid.row_len();

        // Get available space
        let available = ui.available_size();
        let available_width = available.x;
        let available_height = available.y;

        let line_height = self.line_height;
        let char_width = self.char_width;

        // eprintln!("[UI] Char size: {:.1} x {:.1}", char_width, line_height);

        // Allocate the full available space
        let sense = if interaction_enabled {
            egui::Sense::click_and_drag().union(egui::Sense::focusable_noninteractive())
        } else {
            egui::Sense::hover()
        };
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(available_width, available_height), sense);

        self.render_terminal_at_rect(
            ui,
            terminal,
            interaction_enabled,
            interaction_enabled,
            cursor_visible,
            search_state,
            links,
            hovered_link,
            rect,
            response.clone(),
            cols,
            rows,
            line_height,
            char_width,
        )
    }

    // Keep the hot render path explicit: bundling these borrowed frame inputs would add churn.
    #[allow(clippy::too_many_arguments)]
    fn render_terminal_at_rect(
        &mut self,
        ui: &mut Ui,
        terminal: &mut TerminalState,
        interaction_enabled: bool,
        focus_enabled: bool,
        cursor_visible: bool,
        search_state: &crate::search::SearchState,
        links: &[crate::link::Link],
        hovered_link: &Option<crate::link::Link>,
        rect: egui::Rect,
        response: egui::Response,
        _cols: usize,
        _rows: usize,
        line_height: f32,
        char_width: f32,
    ) -> Response {
        let pixels_per_point = ui.ctx().pixels_per_point().max(0.1);
        terminal.kitty_graphics.set_cell_size_pixels(
            (char_width * pixels_per_point).round().max(1.0) as u32,
            (line_height * pixels_per_point).round().max(1.0) as u32,
        );
        let grid = terminal.get_visible_cells();
        let rows = grid.len();
        let cols = if rows > 0 { grid[0].len() } else { 80 };

        // eprintln!("[UI] Rect: {:?}", rect);

        let painter = ui.painter_at(rect);
        // OSC 11 dynamic background wins over the theme for the whole widget.
        let bg = terminal
            .dynamic_bg
            .map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
            .unwrap_or_else(|| self.theme.terminal_background());
        let bg_with_opacity = egui::Color32::from_rgba_unmultiplied(
            bg.r(),
            bg.g(),
            bg.b(),
            (self.opacity * 255.0) as u8,
        );
        painter.rect_filled(rect, egui::CornerRadius::ZERO, bg_with_opacity);

        let (content_rect, scrollbar_rect) = self.layout_rects(rect);
        self.last_content_rect = Some(content_rect);
        // Owned per-frame snapshot of visible command blocks, shared by the
        // gutter hit test below and the chrome painting after the grid.
        // `None` while chrome is gated off (config, alt screen, reflow).
        let block_chrome = self.compute_block_chrome(terminal, rows);
        let cursor_pos = terminal.get_cursor_pos();
        let ime_rect = cursor_rect(
            content_rect,
            cursor_pos.0,
            cursor_pos.1,
            char_width,
            line_height,
        );

        let ctx = ui.ctx();
        if focus_enabled
            && (response.clicked()
                || (!self.requested_initial_focus && !ctx.memory(|mem| mem.has_focus(response.id))))
        {
            response.request_focus();
            self.requested_initial_focus = true;
        }

        let has_focus = ctx.memory(|mem| mem.has_focus(response.id));
        if focus_enabled && has_focus {
            // Tell egui that the terminal widget needs arrow keys, tab, and escape,
            // so they are NOT consumed by egui's focus navigation system.
            ctx.memory_mut(|mem| {
                mem.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });
        }
        if focus_enabled && !self.ime_enabled {
            ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::IMEPurpose(
                egui::IMEPurpose::Terminal,
            ));
            self.ime_enabled = true;
        }

        if focus_enabled {
            let ime_rect_changed = self
                .last_ime_rect
                .map(|prev| prev != ime_rect)
                .unwrap_or(true);
            if ime_rect_changed {
                ctx.send_viewport_cmd(egui::ViewportCommand::IMERect(ime_rect));
                self.last_ime_rect = Some(ime_rect);
            }
        } else {
            self.dragging_scrollbar = false;
            self.cursor_move_input.clear();
            self.cursor_move_terminal_ptr = None;
            self.ime_enabled = false;
            self.last_ime_rect = None;
        }

        // Pre-compute scrollbar geometry for hit-testing
        let scrollbar_width = scrollbar_rect.width();
        let scrollbar_x = scrollbar_rect.left();
        let scrollbar_hovered = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|pos| {
            scrollbar_rect
                .expand(Self::SCROLLBAR_HIT_EXPAND)
                .contains(pos)
        });
        let show_scrollbar = !terminal.scrollback.is_empty()
            && match self.scrollbar_visibility {
                crate::config::ScrollbarVisibility::Always => true,
                crate::config::ScrollbarVisibility::Auto => {
                    scrollbar_hovered || self.dragging_scrollbar
                }
            };
        let mouse_enabled = terminal.is_mouse_enabled();
        let rendered_terminal = terminal as *const TerminalState as usize;
        self.local_selection_terminal = local_selection_capture_after_press(
            self.local_selection_terminal,
            rendered_terminal,
            mouse_enabled,
            interaction_enabled,
            response.hovered(),
            ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary)),
            ui.input(|input| input.modifiers.shift),
        );
        let local_selection_enabled = self.local_selection_terminal == Some(rendered_terminal);

        // Compute thumb rect and related values for interaction
        let scrollbar_thumb_rect: Option<(egui::Rect, f32, f32, f32)> =
            if !terminal.scrollback.is_empty() {
                let total_lines = terminal.scrollback.len() + rows;
                let visible_lines = rows;
                if total_lines > visible_lines {
                    let scrollbar_height = scrollbar_rect.height();
                    let thumb_height =
                        Self::scrollbar_thumb_height(visible_lines, total_lines, scrollbar_height);
                    // 反转逻辑：scroll_offset=0时thumb在底部（最新内容），scroll_offset=max时thumb在顶部（历史）
                    let thumb_y = scrollbar_height
                        - thumb_height
                        - (terminal.scroll_offset as f32 / terminal.scrollback.len() as f32)
                            * (scrollbar_height - thumb_height);
                    let thumb_rect = egui::Rect::from_min_size(
                        egui::pos2(scrollbar_x, scrollbar_rect.top() + thumb_y),
                        egui::vec2(scrollbar_width, thumb_height),
                    );
                    Some((
                        thumb_rect,
                        scrollbar_height,
                        thumb_height,
                        terminal.scrollback.len() as f32,
                    ))
                } else {
                    None
                }
            } else {
                None
            };

        // Handle mouse events for text selection
        // Track selection start on initial mouse down
        // Scrollbar interaction: detect if drag started on thumb
        if interaction_enabled && response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                if pos.x >= scrollbar_x {
                    if let Some((thumb_rect, ..)) = scrollbar_thumb_rect {
                        if thumb_rect.contains(pos) {
                            self.dragging_scrollbar = true;
                        }
                    }
                }
            }
        }

        // Scrollbar drag: update scroll_offset while dragging thumb
        if interaction_enabled && self.dragging_scrollbar && response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                if let Some((_, scrollbar_height, thumb_height, scrollback_len_f)) =
                    scrollbar_thumb_rect
                {
                    let track_height = scrollbar_height - thumb_height;
                    if track_height > 0.0 {
                        // 反转逻辑：向上拖动看历史（增大scroll_offset），向下拖动看最新（减小scroll_offset）
                        let relative_y = (pos.y - scrollbar_rect.top() - thumb_height / 2.0)
                            .clamp(0.0, track_height);
                        let new_offset = (((track_height - relative_y) / track_height)
                            * scrollback_len_f)
                            .round() as usize;
                        let new_offset = new_offset.min(terminal.scrollback.len());
                        // Like wheel scrolling, dragging the scrollbar only
                        // moves the viewport. Selection endpoints are anchored
                        // in absolute buffer coordinates and remain valid.
                        terminal.scroll_offset = new_offset;
                    }
                }
            }
        }

        // Reset dragging state when mouse released
        if interaction_enabled && response.drag_stopped() {
            self.dragging_scrollbar = false;
        }

        // Click in scrollbar track (not on thumb): page up/down
        if interaction_enabled && response.drag_started() && !self.dragging_scrollbar {
            if let Some(pos) = response.interact_pointer_pos() {
                if pos.x >= scrollbar_x && !terminal.scrollback.is_empty() {
                    if let Some((thumb_rect, ..)) = scrollbar_thumb_rect {
                        if pos.y < thumb_rect.top() {
                            // Click above thumb: scroll up (see older history)
                            terminal.scroll(rows as isize);
                        } else if pos.y > thumb_rect.bottom() {
                            // Click below thumb: scroll down (see newest content)
                            terminal.scroll(-(rows as isize));
                        }
                    }
                }
            }
        }

        // A plain local click in the content area dismisses the previous
        // selection. Scrollbar navigation preserves it, and double/triple
        // clicks replace it in their dedicated handlers below.
        let click_pos = response.interact_pointer_pos();
        let pointer_in_content = click_pos.is_some_and(|pos| pos.x < scrollbar_x);
        let plain_content_click = interaction_enabled
            && should_clear_selection_on_click(
                local_selection_enabled,
                ui.input(|input| input.modifiers.ctrl),
                response.clicked(),
                response.double_clicked(),
                response.triple_clicked(),
                self.dragging_scrollbar,
                pointer_in_content,
            );
        // Block-mode gutter hit test: a plain click inside the narrow band at
        // the content left edge selects the block on that row and consumes
        // the click (no cursor move). Only stripe-bearing blocks are
        // selectable; on the live prompt block (Prompt/Editing — exactly
        // where click-to-place-cursor matters) the click falls through to
        // normal handling.
        let gutter_selected_block = if plain_content_click {
            click_pos
                .filter(|pos| {
                    pos.x >= content_rect.left()
                        && pos.x < content_rect.left() + crate::block_mode::GUTTER_CLICK_BAND_PX
                })
                .and_then(|pos| {
                    let entries = block_chrome.as_ref()?;
                    let (row, _) = grid_position_from_content(
                        pos,
                        content_rect,
                        char_width,
                        line_height,
                        cols,
                        rows,
                    );
                    entries
                        .iter()
                        .find(|entry| entry.span.first_row <= row && row <= entry.span.last_row)
                        .filter(|entry| entry.outcome != crate::block_mode::BlockOutcome::Prompt)
                        .map(|entry| entry.id.clone())
                })
        } else {
            None
        };
        if let Some(id) = gutter_selected_block {
            // A consumed gutter click still dismisses the text selection, so
            // a stale highlight cannot survive and be copied later.
            terminal.selection = None;
            self.block_click = Some(crate::block_mode::BlockClick::Select(id));
        } else if plain_content_click {
            // Any other plain content click drops the app-level block
            // selection along with the local text selection.
            self.block_click = Some(crate::block_mode::BlockClick::Clear);
            terminal.selection = None;

            if let Some(pos) = click_pos {
                let (click_row, click_col) = grid_position_from_content(
                    pos,
                    content_rect,
                    char_width,
                    line_height,
                    cols,
                    rows,
                );

                // A newer click supersedes any prior not-yet-routed synthetic
                // movement, including a click the terminal declines to act on.
                self.cursor_move_input.clear();
                self.cursor_move_terminal_ptr = None;

                let bytes =
                    terminal.click_cursor_move(click_row, click_col, self.click_moves_cursor);
                if !bytes.is_empty() {
                    self.cursor_move_terminal_ptr = Some(rendered_terminal);
                    self.cursor_move_input = bytes;
                }
            }
        }

        // Triple-click: select the whole visual line, like VTE terminals.
        if interaction_enabled
            && local_selection_enabled
            && response.triple_clicked()
            && !self.dragging_scrollbar
        {
            if let Some(pos) = response.interact_pointer_pos() {
                if pos.x < scrollbar_x {
                    let clamped_y =
                        (pos.y - content_rect.top()).clamp(0.0, content_rect.height().max(0.0));
                    let row = if line_height > 0.0 {
                        ((clamped_y / line_height) as usize).min(rows - 1)
                    } else {
                        0
                    };
                    terminal.select_line_at(row);
                    // Replacing the text selection drops the block selection
                    // too, like any other real content interaction.
                    self.block_click = Some(crate::block_mode::BlockClick::Clear);
                }
            }
        // Double-click: select word at cursor position
        } else if interaction_enabled
            && local_selection_enabled
            && response.double_clicked()
            && !self.dragging_scrollbar
        {
            if let Some(pos) = response.interact_pointer_pos() {
                if pos.x < scrollbar_x {
                    let clamped_x =
                        (pos.x - content_rect.left()).clamp(0.0, content_rect.width().max(0.0));
                    let clamped_y =
                        (pos.y - content_rect.top()).clamp(0.0, content_rect.height().max(0.0));

                    let col = if char_width > 0.0 {
                        ((clamped_x / char_width) as usize).min(cols - 1)
                    } else {
                        0
                    };
                    let row = if line_height > 0.0 {
                        ((clamped_y / line_height) as usize).min(rows - 1)
                    } else {
                        0
                    };
                    terminal.select_word_at(row, col);
                    // 同上:双击换选中也一并清掉 block 选中。
                    self.block_click = Some(crate::block_mode::BlockClick::Clear);
                }
            }
        }

        // Text selection: only when not interacting with scrollbar
        if interaction_enabled
            && local_selection_enabled
            && response.drag_started()
            && !self.dragging_scrollbar
        {
            if let Some(pos) = response.interact_pointer_pos() {
                // Only select text if NOT in scrollbar area
                if pos.x < scrollbar_x {
                    // Clamp position to rect bounds to prevent underflow
                    let clamped_x =
                        (pos.x - content_rect.left()).clamp(0.0, content_rect.width().max(0.0));
                    let clamped_y =
                        (pos.y - content_rect.top()).clamp(0.0, content_rect.height().max(0.0));

                    let col = if char_width > 0.0 {
                        ((clamped_x / char_width) as usize).min(cols - 1)
                    } else {
                        0
                    };
                    let row = if line_height > 0.0 {
                        ((clamped_y / line_height) as usize).min(rows - 1)
                    } else {
                        0
                    };
                    let alt_held = ui.input(|i| i.modifiers.alt);
                    if alt_held {
                        terminal.start_block_selection((row, col));
                    } else {
                        terminal.start_selection((row, col));
                    }
                    ui.ctx().request_repaint();
                }
            }
        }

        // Update selection end during drag
        if interaction_enabled
            && local_selection_enabled
            && response.dragged()
            && !self.dragging_scrollbar
        {
            if let Some(pos) = response.interact_pointer_pos() {
                if pos.x < scrollbar_x {
                    // Clamp position to rect bounds to prevent underflow
                    let clamped_x =
                        (pos.x - content_rect.left()).clamp(0.0, content_rect.width().max(0.0));
                    let clamped_y =
                        (pos.y - content_rect.top()).clamp(0.0, content_rect.height().max(0.0));

                    let col = if char_width > 0.0 {
                        ((clamped_x / char_width) as usize).min(cols - 1)
                    } else {
                        0
                    };
                    let row = if line_height > 0.0 {
                        ((clamped_y / line_height) as usize).min(rows - 1)
                    } else {
                        0
                    };
                    terminal.update_selection((row, col));
                    ui.ctx().request_repaint(); // Force repaint to show selection update
                }
            }
        }

        // Keep the capture through this release frame so click/drag-stopped
        // handlers above still observe the press-time routing decision.
        if !ui.input(|input| input.pointer.any_down()) {
            self.local_selection_terminal = None;
        }

        // Extremely negative z-index images sit below non-default cell
        // backgrounds. The remaining negative layer is inserted between the
        // grid's background and foreground phases below.
        self.paint_kitty_image_layer(
            ui.ctx(),
            &painter,
            terminal,
            content_rect,
            char_width,
            line_height,
            KittyImageLayer::BelowCellBackgrounds,
        );

        // GPU-accelerated grid rendering via wgpu instanced draw
        let gpu_rendered = if self.gpu_rendering {
            self.render_grid_gpu(
                ui,
                terminal,
                search_state,
                links,
                hovered_link,
                &grid,
                rows,
                cols,
                content_rect,
                char_width,
                line_height,
            )
        } else {
            false
        };

        if !gpu_rendered {
            let link_map: Vec<Vec<&crate::link::Link>> = if links.is_empty() {
                Vec::new()
            } else {
                let mut m = vec![Vec::new(); rows];
                for link in links {
                    if link.line < rows {
                        m[link.line].push(link);
                    }
                }
                m
            };
            // Fallback: CPU rendering via egui painter
            self.render_grid_cpu(
                ui,
                &painter,
                terminal,
                search_state,
                &link_map,
                hovered_link,
                &grid,
                rows,
                cols,
                content_rect,
                char_width,
                line_height,
            );
        }

        // Zero/positive z-index images are above terminal text. Keep the
        // cursor as the final UI affordance so it remains locatable.
        self.paint_kitty_image_layer(
            ui.ctx(),
            &painter,
            terminal,
            content_rect,
            char_width,
            line_height,
            KittyImageLayer::AboveText,
        );

        // Command-block chrome (gutter stripes, separators, outcome badges).
        // Painter overlays recomputed every frame, drawn after the grid so
        // they never interact with the GPU path's dirty tracking.
        if let Some(entries) = &block_chrome {
            self.draw_block_chrome(
                &painter,
                entries,
                &grid,
                content_rect,
                char_width,
                line_height,
            );
        }

        // Render cursor - direct O(1) positioning instead of full grid scan
        if cursor_visible && cursor_pos.0 < rows && cursor_pos.1 < cols {
            let (crow, ccol) = cursor_pos;
            let cell = &grid[crow][ccol];
            if !cell.flags.wide_continuation() {
                let (x, snapped_width) = snapped_span(content_rect.left(), ccol, char_width);
                let (y, snapped_height) = snapped_span(content_rect.top(), crow, line_height);

                let cell_width = if cell.flags.wide() {
                    let (_, next_width) = snapped_span(content_rect.left(), ccol + 1, char_width);
                    snapped_width + next_width
                } else {
                    snapped_width
                };
                let cell_rect = egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    Vec2::new(cell_width, snapped_height),
                );
                let block_cursor_rect =
                    cell_rect.shrink2(egui::vec2((cell_width * 0.24).clamp(1.5, 3.0), 0.5));

                let dynamic_cursor = terminal
                    .dynamic_cursor_color
                    .map(|(r, g, b)| Color32::from_rgb(r, g, b));
                match &terminal.cursor_shape {
                    crate::terminal::CursorShape::Block => {
                        let cursor_c = dynamic_cursor.unwrap_or_else(|| self.theme.cursor_color());
                        let [r, g, b, _] = cursor_c.to_srgba_unmultiplied();
                        painter.rect_filled(
                            block_cursor_rect,
                            egui::CornerRadius::ZERO,
                            Color32::from_rgba_unmultiplied(r, g, b, 56),
                        );
                        painter.rect_stroke(
                            block_cursor_rect,
                            egui::CornerRadius::ZERO,
                            egui::Stroke::new(0.8, cursor_c),
                            egui::StrokeKind::Middle,
                        );
                    }
                    crate::terminal::CursorShape::Underline => {
                        let underline_y = y + line_height - 1.25;
                        painter.line_segment(
                            [
                                egui::pos2(x, underline_y),
                                egui::pos2(x + cell_width, underline_y),
                            ],
                            egui::Stroke::new(
                                0.8,
                                dynamic_cursor.unwrap_or_else(|| self.theme.cursor_color()),
                            ),
                        );
                    }
                    crate::terminal::CursorShape::Beam => {
                        painter.line_segment(
                            [
                                egui::pos2(x + 0.25, y),
                                egui::pos2(x + 0.25, y + line_height),
                            ],
                            egui::Stroke::new(
                                0.8,
                                dynamic_cursor.unwrap_or_else(|| self.theme.cursor_color()),
                            ),
                        );
                    }
                }
            }
        }

        // Render IME preedit text below cursor
        if !terminal.preedit_text.is_empty() && terminal.ime_enabled {
            let preedit_display = format!("➜ {}", terminal.preedit_text);

            // 在光标下方显示预编辑文本
            let preedit_x = content_rect.left() + cursor_pos.1 as f32 * char_width;
            let preedit_y = content_rect.top() + cursor_pos.0 as f32 * line_height + line_height;

            // 确保不超出屏幕范围
            if preedit_y + line_height <= content_rect.bottom() {
                let font_id = FontId::monospace(self.font_size);
                let galley = ui.painter().layout_no_wrap(
                    preedit_display,
                    font_id,
                    Color32::from_rgb(200, 200, 0), // 黄色标记
                );

                painter.galley(egui::pos2(preedit_x, preedit_y), galley, Color32::WHITE);
            }
        }

        // Draw scrollbar background and thumb
        if show_scrollbar {
            let sb = &self.theme.scrollbar;
            let track_color = if self.dragging_scrollbar {
                crate::theme::Theme::rgba_to_color32(sb.track_drag)
            } else if scrollbar_hovered {
                crate::theme::Theme::rgba_to_color32(sb.track_hover)
            } else {
                crate::theme::Theme::rgba_to_color32(sb.track_normal)
            };
            painter.rect_filled(scrollbar_rect, 6.0, track_color);

            // Recompute thumb with current scroll_offset (may have changed from interaction)
            if let Some((_, scrollbar_height, _, scrollback_len_f)) = scrollbar_thumb_rect {
                let total_lines = terminal.scrollback.len() + rows;
                let visible_lines = rows;
                let thumb_height =
                    Self::scrollbar_thumb_height(visible_lines, total_lines, scrollbar_height);
                // 反转逻辑：scroll_offset=0时thumb在底部（最新内容），scroll_offset=max时thumb在顶部（历史）
                let thumb_y = scrollbar_height
                    - thumb_height
                    - (terminal.scroll_offset as f32 / scrollback_len_f)
                        * (scrollbar_height - thumb_height);
                let thumb_rect = egui::Rect::from_min_size(
                    egui::pos2(scrollbar_x, scrollbar_rect.top() + thumb_y),
                    egui::vec2(scrollbar_width, thumb_height),
                );

                // Visual feedback: thumb changes color when being dragged
                let thumb_color = if self.dragging_scrollbar {
                    crate::theme::Theme::rgba_to_color32(sb.thumb_drag)
                } else if scrollbar_hovered {
                    crate::theme::Theme::rgba_to_color32(sb.thumb_hover)
                } else {
                    crate::theme::Theme::rgba_to_color32(sb.thumb_normal)
                };
                painter.rect_filled(thumb_rect.shrink2(egui::vec2(1.0, 0.0)), 6.0, thumb_color);
            }

            // Failed-block markers, painted AFTER the thumb so a large thumb
            // never hides them (frost draws them over the thumb too). Same
            // red as the Failed gutter stripe.
            if self.block_mode && !terminal.is_alt_buffer_active() {
                let marker_color =
                    self.block_outcome_color(crate::block_mode::BlockOutcome::Failed(1));
                // ~3 physical px tall regardless of DPI scale.
                let marker_height = 3.0 / ui.ctx().pixels_per_point();
                for fraction in Self::failed_block_marker_fractions(terminal) {
                    let marker_y =
                        scrollbar_rect.top() + fraction * (scrollbar_rect.height() - marker_height);
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(scrollbar_x, marker_y),
                            egui::vec2(scrollbar_width, marker_height),
                        ),
                        0.0,
                        marker_color,
                    );
                }
            }
        }

        // Clear dirty region after rendering
        terminal.dirty_region.clear();

        response
    }

    /// GPU path: build instance buffer from grid, rasterize new glyphs, emit PaintCallback.
    /// Returns true if GPU rendering was used, false if fallback is needed.
    // The GPU boundary mirrors the complete frame state consumed by the paint callback.
    #[allow(clippy::too_many_arguments)]
    fn render_grid_gpu(
        &mut self,
        ui: &mut Ui,
        terminal: &TerminalState,
        search_state: &crate::search::SearchState,
        links: &[crate::link::Link],
        hovered_link: &Option<crate::link::Link>,
        grid: &[Vec<crate::terminal::TerminalCell>],
        rows: usize,
        cols: usize,
        content_rect: egui::Rect,
        char_width: f32,
        line_height: f32,
    ) -> bool {
        let render_state = match &self.wgpu_render_state {
            Some(rs) => rs,
            None => return false,
        };

        let ppp = ui.ctx().pixels_per_point();
        // OSC 11 dynamic background wins over the theme for the whole grid.
        let default_bg = terminal
            .dynamic_bg
            .map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
            .unwrap_or_else(|| self.theme.terminal_background());
        let has_search = !search_state.matches.is_empty() && !search_state.query.is_empty();
        let target_cell_width = char_width * ppp;
        let target_cell_height = line_height * ppp;
        let font_generation_before = {
            // Resolve a grow/compaction requested by the previous CPU build
            // before reading the generation and constructing instances. Doing
            // this only in the paint callback would let one frame use old UVs
            // with a newly reset atlas.
            let mut renderer = render_state.renderer.write();
            let Some(gpu_res) = renderer
                .callback_resources
                .get_mut::<gpu::callback::GpuResources>()
            else {
                return false;
            };
            gpu_res.prepare_atlas(&render_state.device, &render_state.queue);
            gpu_res.font_content_generation()
        };

        // --- Dirty detection: determine which rows need rebuild ---
        let terminal_ptr = terminal as *const _ as usize;
        let current_grid_version = terminal.get_grid_version();
        let current_scroll_offset = terminal.scroll_offset;
        let current_selection = terminal.selection;
        let search_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            search_state.query.hash(&mut h);
            search_state.matches.hash(&mut h);
            search_state.current_match_index.hash(&mut h);
            h.finish()
        };

        // Detect major screen changes (e.g., alternate screen switch)
        let grid_version_jumped =
            current_grid_version > self.last_rendered_grid_version + rows as u64;

        let need_full_rebuild = self.cached_instances.is_empty()
            || self.last_rendered_font_generation != font_generation_before
            || self.last_rendered_terminal_ptr != terminal_ptr
            || self.last_rendered_rows != rows
            || self.last_rendered_cols != cols
            || self.last_rendered_scroll_offset != current_scroll_offset
            || grid_version_jumped;

        // 跨帧复用 dirty_rows 缓冲:make_mut 在上一帧 callback 已 drop(refcount==1)时
        // 原地复用底层 buffer,避免每帧重新分配 Vec<bool>。
        let dirty_rows = std::sync::Arc::make_mut(&mut self.dirty_rows);
        dirty_rows.clear();
        dirty_rows.resize(rows, false);

        if need_full_rebuild {
            dirty_rows.fill(true);
        } else {
            // Grid content changes - reuse changed_rows_buffer for collection
            self.changed_rows_buffer.clear();
            terminal.get_dirty_rows(
                self.last_rendered_grid_version,
                &mut self.changed_rows_buffer,
            );
            for &r in &self.changed_rows_buffer {
                if r < rows {
                    dirty_rows[r] = true;
                }
            }

            // Selection overlay changes
            if self.last_rendered_selection != current_selection {
                // Selection is a viewport overlay whose absolute rows may span
                // live grid, scrollback and reflowed soft wraps. Rebuild every
                // visible row when it changes so the GPU cache cannot retain
                // stale unselected instances outside the hovered row.
                dirty_rows.fill(true);
            }

            // Search overlay changes - only mark matching lines dirty
            if self.last_rendered_search_hash != search_hash {
                // Mark all previously matched lines as dirty (to clear old highlights)
                for &old_line in &self.last_search_match_lines {
                    if old_line < dirty_rows.len() {
                        dirty_rows[old_line] = true;
                    }
                }

                // Mark all currently matched lines as dirty
                for m in &search_state.matches {
                    if let Some(viewport_row) = m
                        .viewport_row(terminal)
                        .filter(|viewport_row| *viewport_row < dirty_rows.len())
                    {
                        dirty_rows[viewport_row] = true;
                    }
                }
            }

            // Link hover changes
            if self.last_rendered_hovered_link != *hovered_link {
                if let Some(ref link) = self.last_rendered_hovered_link {
                    if link.line < rows {
                        dirty_rows[link.line] = true;
                    }
                }
                if let Some(ref link) = hovered_link {
                    if link.line < rows {
                        dirty_rows[link.line] = true;
                    }
                }
            }
        }

        let any_dirty = dirty_rows.iter().any(|&d| d);
        let ligatures = self.font_ligatures;
        // 记录是否在脏行打补丁阶段触发了整表重建(宽字符出现/消失导致行实例数变化)。
        // 一旦发生,后续行的偏移已整体平移,必须全量上传 GPU buffer;否则只传脏行会让
        // buffer 残留旧布局,渲染错乱。见末尾 use_partial_upload 的计算。
        let mut did_full_relayout = false;
        let mut font_generation_after = font_generation_before;
        if !any_dirty && !self.cached_instances.is_empty() {
            // Nothing changed — reuse cached instances as-is
        } else {
            {
                let mut renderer = render_state.renderer.write();
                let gpu_res = match renderer
                    .callback_resources
                    .get_mut::<gpu::callback::GpuResources>()
                {
                    Some(r) => r,
                    None => return false,
                };
                let (ascent, descent, advance) = gpu_res.atlas.font_metrics();
                let (aw, ah) = gpu_res.atlas.atlas_dimensions();
                self.cached_atlas_w = aw as f32;
                self.cached_atlas_h = ah as f32;
                let font_cell_width = advance;
                let font_cell_height = ascent - descent;
                // Round adjustments to integer pixels to prevent blur from linear filtering
                let glyph_offset_x_adjust = ((target_cell_width - font_cell_width) * 0.5)
                    .max(0.0)
                    .round();
                let glyph_offset_y_adjust = ((target_cell_height - font_cell_height) * 0.5)
                    .max(0.0)
                    .round();

                let link_map: Vec<Vec<&crate::link::Link>> = if links.is_empty() {
                    Vec::new()
                } else {
                    let mut map = vec![Vec::new(); rows];
                    for link in links {
                        if link.line < rows {
                            map[link.line].push(link);
                        }
                    }
                    map
                };

                let search_map = if !has_search {
                    Vec::new()
                } else {
                    viewport_search_map(terminal, &search_state.matches, rows)
                };

                if need_full_rebuild {
                    // Full rebuild: clear and rebuild all
                    let instances = std::sync::Arc::make_mut(&mut self.cached_instances);
                    let offsets = std::sync::Arc::make_mut(&mut self.row_instance_offsets);
                    let counts = std::sync::Arc::make_mut(&mut self.row_instance_counts);
                    instances.clear();
                    offsets.clear();
                    counts.clear();
                    instances.reserve(rows * cols);

                    for row_idx in 0..rows {
                        let offset = instances.len();
                        Self::build_row_instances(
                            instances,
                            gpu_res,
                            grid,
                            terminal,
                            search_state,
                            &link_map,
                            &search_map,
                            hovered_link,
                            &self.theme,
                            default_bg,
                            has_search,
                            glyph_offset_x_adjust,
                            glyph_offset_y_adjust,
                            ligatures,
                            row_idx,
                            cols,
                        );
                        let count = instances.len() - offset;
                        offsets.push(offset);
                        counts.push(count);
                    }
                } else {
                    // Partial rebuild: only rebuild dirty rows
                    let mut needs_relayout = false;
                    let mut row_scratch = std::mem::take(&mut self.row_instances_scratch);

                    for (row_idx, is_dirty) in dirty_rows.iter().copied().enumerate().take(rows) {
                        if !is_dirty {
                            continue;
                        }

                        // Build new instances for this row into reusable scratch buffer
                        row_scratch.clear();
                        Self::build_row_instances(
                            &mut row_scratch,
                            gpu_res,
                            grid,
                            terminal,
                            search_state,
                            &link_map,
                            &search_map,
                            hovered_link,
                            &self.theme,
                            default_bg,
                            has_search,
                            glyph_offset_x_adjust,
                            glyph_offset_y_adjust,
                            ligatures,
                            row_idx,
                            cols,
                        );

                        let old_count = self.row_instance_counts[row_idx];
                        if row_scratch.len() != old_count {
                            // Instance count changed (wide chars appeared/disappeared)
                            // Fall back to full relayout
                            needs_relayout = true;
                            break;
                        }

                        // Patch in-place
                        let offset = self.row_instance_offsets[row_idx];
                        let instances = std::sync::Arc::make_mut(&mut self.cached_instances);
                        instances[offset..offset + old_count].copy_from_slice(&row_scratch);
                    }

                    // Return scratch buffer for next frame
                    self.row_instances_scratch = row_scratch;

                    if needs_relayout {
                        did_full_relayout = true;
                        // Rebuild all from scratch
                        let instances = std::sync::Arc::make_mut(&mut self.cached_instances);
                        let offsets = std::sync::Arc::make_mut(&mut self.row_instance_offsets);
                        let counts = std::sync::Arc::make_mut(&mut self.row_instance_counts);
                        instances.clear();
                        offsets.clear();
                        counts.clear();
                        instances.reserve(rows * cols);
                        for row_idx in 0..rows {
                            let offset = instances.len();
                            Self::build_row_instances(
                                instances,
                                gpu_res,
                                grid,
                                terminal,
                                search_state,
                                &link_map,
                                &search_map,
                                hovered_link,
                                &self.theme,
                                default_bg,
                                has_search,
                                glyph_offset_x_adjust,
                                glyph_offset_y_adjust,
                                ligatures,
                                row_idx,
                                cols,
                            );
                            let count = instances.len() - offset;
                            offsets.push(offset);
                            counts.push(count);
                        }
                    }
                }
                font_generation_after = gpu_res.font_content_generation();
            } // drop renderer write lock
        }

        // Rasterizing a missing glyph can grow or compact the atlas. Earlier
        // rows in this same batch then contain UVs for the previous layout.
        // Paint the CPU fallback for this frame and rebuild every GPU instance
        // next frame against one stable generation.
        if font_generation_after != font_generation_before {
            self.invalidate_font_cache();
            ui.ctx().request_repaint();
            return false;
        }

        // Update tracking state
        self.last_rendered_terminal_ptr = terminal_ptr;
        self.last_rendered_grid_version = current_grid_version;
        self.last_rendered_scroll_offset = current_scroll_offset;
        self.last_rendered_selection = current_selection;
        self.last_rendered_search_hash = search_hash;
        // Update last_search_match_lines for next frame's dirty tracking
        self.last_search_match_lines.clear();
        for m in &search_state.matches {
            if let Some(viewport_row) = m.viewport_row(terminal) {
                self.last_search_match_lines.push(viewport_row);
            }
        }
        // 绝大多数帧 hovered_link 不变,先比较再 clone 以省去 String 堆分配。
        if self.last_rendered_hovered_link != *hovered_link {
            self.last_rendered_hovered_link = hovered_link.clone();
        }
        self.last_rendered_cols = cols;
        self.last_rendered_rows = rows;
        self.last_rendered_font_generation = font_generation_after;

        let (atlas_w, atlas_h) = (self.cached_atlas_w, self.cached_atlas_h);

        let instance_count = self.cached_instances.len() as u32;
        let frame_nr = ui.ctx().cumulative_frame_nr();
        // Mirror egui-wgpu's viewport rounding: it rounds each rect edge to the
        // physical-pixel grid before calling set_viewport. Deriving the px→NDC
        // scale from the *unrounded* size would rescale the whole grid by a
        // sub-pixel factor, drifting glyphs off the pixel grid toward the
        // right/bottom edges and defeating the shader's crispness snap.
        let vp_width_px = (content_rect.max.x * ppp).round() - (content_rect.min.x * ppp).round();
        let vp_height_px = (content_rect.max.y * ppp).round() - (content_rect.min.y * ppp).round();
        let background_uniforms = gpu::instance::GridUniforms {
            viewport_width: vp_width_px.max(1.0),
            viewport_height: vp_height_px.max(1.0),
            cell_width: target_cell_width,
            cell_height: target_cell_height,
            atlas_width: atlas_w,
            atlas_height: atlas_h,
            render_phase: 0.0,
            scroll_pixel_offset: self.scroll_pixel_offset * ppp,
        };

        let foreground_uniforms = gpu::instance::GridUniforms {
            render_phase: 1.0,
            ..background_uniforms
        };

        let background_callback = gpu::callback::GridBackgroundCallback {
            surface_id: self.gpu_surface_id,
            frame_nr,
            instances: self.cached_instances.clone(),
            uniforms: background_uniforms,
            instance_count,
            row_offsets: self.row_instance_offsets.clone(),
            row_counts: self.row_instance_counts.clone(),
            dirty_rows: self.dirty_rows.clone(),
            use_partial_upload: !need_full_rebuild && !did_full_relayout && any_dirty,
            expected_font_generation: font_generation_after,
        };

        let foreground_callback = gpu::callback::GridForegroundCallback {
            surface_id: self.gpu_surface_id,
            frame_nr,
            uniforms: foreground_uniforms,
            instance_count,
            expected_font_generation: font_generation_after,
        };

        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            content_rect,
            background_callback,
        ));
        let painter = ui.painter().clone();
        self.paint_kitty_image_layer(
            ui.ctx(),
            &painter,
            terminal,
            content_rect,
            char_width,
            line_height,
            KittyImageLayer::BelowText,
        );
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            content_rect,
            foreground_callback,
        ));
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn build_row_instances(
        instances: &mut Vec<gpu::instance::CellInstance>,
        gpu_res: &mut gpu::callback::GpuResources,
        grid: &[Vec<crate::terminal::TerminalCell>],
        terminal: &TerminalState,
        search_state: &crate::search::SearchState,
        link_map: &[Vec<&crate::link::Link>],
        search_map: &[Vec<&crate::search::SearchMatch>],
        hovered_link: &Option<crate::link::Link>,
        theme: &crate::theme::Theme,
        default_bg: Color32,
        has_search: bool,
        glyph_offset_x_adjust: f32,
        glyph_offset_y_adjust: f32,
        ligatures: bool,
        row_idx: usize,
        cols: usize,
    ) {
        let sel_cols = terminal.row_selection_cols(row_idx);

        // Ligature pass: shape contiguous printable-ASCII runs of the same weight.
        // Only override glyphs when shaping actually merges cells (a ligature
        // formed); plain text keeps the per-cell path untouched. Background fills,
        // underlines and selection remain strictly per-cell.
        let lig_map: Vec<Option<LigOverride>> = if ligatures && gpu_res.atlas.supports_shaping() {
            let mut map: Vec<Option<LigOverride>> = vec![None; cols];
            let row = &grid[row_idx];
            let is_run_char = |cell: &crate::terminal::TerminalCell| {
                let cp = cell.character as u32;
                (0x21..=0x7e).contains(&cp) && !cell.flags.wide() && !cell.flags.wide_continuation()
            };
            let mut col = 0usize;
            while col < cols {
                if !is_run_char(&row[col]) {
                    col += 1;
                    continue;
                }
                let bold = row[col].flags.bold();
                let run_start = col;
                // Pre-size to the remaining row width: the run is pure ASCII so
                // (cols - col) bytes is a tight upper bound — single allocation.
                let mut run = String::with_capacity(cols - col);
                let mut c = col;
                while c < cols && is_run_char(&row[c]) && row[c].flags.bold() == bold {
                    run.push(row[c].character);
                    c += 1;
                }
                let run_len = c - run_start;
                if run_len >= 2 {
                    // Subpixel horizontal positioning is handled by the fractional cell
                    // origin + linear sampling in the shader, so a single bin suffices.
                    let shaped = gpu_res.atlas.shape_run(&run, bold, 0);
                    // A merge happened only if fewer glyphs than input columns.
                    if shaped.len() < run_len {
                        for slot in map.iter_mut().take(c).skip(run_start) {
                            *slot = Some(LigOverride::Covered);
                        }
                        for g in shaped.iter() {
                            // Run is pure ASCII, so cluster byte offset == column offset.
                            let gcol = run_start + g.cluster as usize;
                            if gcol < cols {
                                map[gcol] = Some(LigOverride::Glyph { region: g.region });
                            }
                        }
                    }
                }
                col = c;
            }
            map
        } else {
            Vec::new()
        };

        // Precompute the active match's identity once per row to avoid an O(matches)
        // scan per highlighted cell. A match is uniquely identified by (line, col_start).
        let active_match_pos = if has_search && !search_state.matches.is_empty() {
            let idx = search_state.current_match_index % search_state.matches.len();
            search_state
                .matches
                .get(idx)
                .map(|m| (m.line_id, m.col_start))
        } else {
            None
        };

        for (col_idx, cell) in grid[row_idx].iter().enumerate().take(cols) {
            if cell.flags.wide_continuation() {
                continue;
            }

            let is_selected = sel_cols
                .map(|(start, end)| col_idx >= start && col_idx <= end)
                .unwrap_or(false);
            let is_inverse = cell.flags.inverse();

            let bold = cell.flags.bold();
            let dim = cell.flags.dim();

            let mut bg_color = if is_selected {
                theme.selection_color()
            } else if is_inverse {
                color::resolve_fg_with_palette(
                    cell.foreground,
                    theme,
                    Some(&terminal.dynamic_palette),
                    terminal.dynamic_fg,
                    bold,
                    dim,
                )
            } else {
                color::resolve_bg_with_palette(
                    cell.background,
                    theme,
                    Some(&terminal.dynamic_palette),
                    terminal.dynamic_bg,
                )
            };

            let mut is_search_match = false;
            if has_search {
                let row_matches = search_map.get(row_idx).map(Vec::as_slice).unwrap_or(&[]);
                if !row_matches.is_empty() {
                    for m in row_matches.iter() {
                        if col_idx >= m.col_start && col_idx < m.col_end {
                            is_search_match = true;
                            bg_color = color::resolve_fg_with_palette(
                                cell.foreground,
                                theme,
                                Some(&terminal.dynamic_palette),
                                terminal.dynamic_fg,
                                bold,
                                dim,
                            );
                            if active_match_pos == Some((m.line_id, m.col_start)) {
                                let [r, g, b, _a] = bg_color.to_srgba_unmultiplied();
                                bg_color = Color32::from_rgba_unmultiplied(
                                    (r as u16 * 180 / 255) as u8,
                                    (g as u16 * 180 / 255) as u8,
                                    (b as u16 * 180 / 255) as u8,
                                    255,
                                );
                            }
                            break;
                        }
                    }
                }
            }

            let is_default_background = !is_selected
                && !is_inverse
                && cell.background == crate::terminal::Color::Default
                && !is_search_match;
            if is_default_background {
                bg_color = default_bg;
            }

            let mut fg_color = if is_selected {
                theme.selection_fg_color()
            } else if is_inverse {
                color::resolve_bg_with_palette(
                    cell.background,
                    theme,
                    Some(&terminal.dynamic_palette),
                    terminal.dynamic_bg,
                )
            } else {
                color::resolve_fg_with_palette(
                    cell.foreground,
                    theme,
                    Some(&terminal.dynamic_palette),
                    terminal.dynamic_fg,
                    bold,
                    dim,
                )
            };

            let is_link = {
                let row_links = link_map.get(row_idx).map(Vec::as_slice).unwrap_or(&[]);
                let mut found = false;
                for link in row_links {
                    if col_idx >= link.col_start && col_idx < link.col_end {
                        let is_hovered_link =
                            hovered_link.as_ref().map(|l| l == *link).unwrap_or(false);
                        if is_hovered_link {
                            fg_color = hovered_link_color();
                        }
                        found = true;
                        break;
                    }
                }
                found
            };
            let has_strikethrough = cell.flags.strikethrough();
            let is_wide = cell.flags.wide();

            let mut flags: u32 = 0;
            let has_glyph = cell.character != ' ' && cell.character != '\0';
            if has_glyph {
                flags |= gpu::instance::CellInstance::FLAG_HAS_GLYPH;
            }
            if is_wide {
                flags |= gpu::instance::CellInstance::FLAG_WIDE;
            }
            // Encode underline style in bits 2-4
            let underline_style =
                if is_link && cell.flags.underline() == crate::terminal::UnderlineStyle::None {
                    crate::terminal::UnderlineStyle::Single
                } else {
                    cell.flags.underline()
                };
            match underline_style {
                crate::terminal::UnderlineStyle::None => {}
                crate::terminal::UnderlineStyle::Single => {
                    flags |= gpu::instance::CellInstance::UNDERLINE_SINGLE
                }
                crate::terminal::UnderlineStyle::Double => {
                    flags |= gpu::instance::CellInstance::UNDERLINE_DOUBLE
                }
                crate::terminal::UnderlineStyle::Curly => {
                    flags |= gpu::instance::CellInstance::UNDERLINE_CURLY
                }
                crate::terminal::UnderlineStyle::Dotted => {
                    flags |= gpu::instance::CellInstance::UNDERLINE_DOTTED
                }
                crate::terminal::UnderlineStyle::Dashed => {
                    flags |= gpu::instance::CellInstance::UNDERLINE_DASHED
                }
            }
            if has_strikethrough {
                flags |= gpu::instance::CellInstance::FLAG_STRIKETHROUGH;
            }

            let lig = lig_map.get(col_idx).and_then(|o| o.as_ref());
            let (u0, v0, u1, v1, glyph_offset_x, glyph_offset_y) = match lig {
                Some(LigOverride::Covered) => {
                    // Consumed by a ligature anchored earlier: no foreground glyph.
                    flags &= !gpu::instance::CellInstance::FLAG_HAS_GLYPH;
                    (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
                }
                Some(LigOverride::Glyph { region }) => {
                    if region.width_px > 0.0 && region.height_px > 0.0 {
                        (
                            region.u0,
                            region.v0,
                            region.u1,
                            region.v1,
                            (region.bearing_x + glyph_offset_x_adjust).round(),
                            (region.bearing_y + glyph_offset_y_adjust).round(),
                        )
                    } else {
                        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
                    }
                }
                None if has_glyph => {
                    let region = gpu_res.atlas.get_or_rasterize(cell.character, bold, 0);
                    if region.width_px > 0.0 && region.height_px > 0.0 {
                        (
                            region.u0,
                            region.v0,
                            region.u1,
                            region.v1,
                            (region.bearing_x + glyph_offset_x_adjust).round(),
                            (region.bearing_y + glyph_offset_y_adjust).round(),
                        )
                    } else {
                        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
                    }
                }
                None => (0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
            };

            let [fg_r, fg_g, fg_b, fg_a] = fg_color.to_srgba_unmultiplied();
            let [bg_r, bg_g, bg_b, _bg_a] = bg_color.to_srgba_unmultiplied();
            // The terminal base color was already painted once behind the
            // grid (with configured opacity). Default cells stay transparent
            // so negative-z Kitty images remain visible and opacity is not
            // composited twice. Explicit/inverse/selection/search backgrounds
            // intentionally cover the image layer.
            let bg_a = if is_default_background { 0 } else { 255 };

            instances.push(gpu::instance::CellInstance {
                col: col_idx as u32,
                row: row_idx as u32,
                glyph_u0: u0,
                glyph_v0: v0,
                glyph_u1: u1,
                glyph_v1: v1,
                fg_color: [fg_r, fg_g, fg_b, fg_a],
                bg_color: [bg_r, bg_g, bg_b, bg_a],
                flags,
                glyph_offset_x,
                glyph_offset_y,
                _pad: 0,
            });
        }
    }

    /// CPU fallback: render grid using egui painter API (the original path).
    // The CPU fallback intentionally mirrors the GPU renderer's frame inputs.
    #[allow(clippy::too_many_arguments)]
    fn render_grid_cpu(
        &mut self,
        ui: &mut Ui,
        painter: &egui::Painter,
        terminal: &TerminalState,
        search_state: &crate::search::SearchState,
        link_map: &[Vec<&crate::link::Link>],
        hovered_link: &Option<crate::link::Link>,
        grid: &[Vec<crate::terminal::TerminalCell>],
        rows: usize,
        cols: usize,
        content_rect: egui::Rect,
        char_width: f32,
        line_height: f32,
    ) {
        let has_search = !search_state.matches.is_empty() && !search_state.query.is_empty();
        let search_map = if has_search {
            viewport_search_map(terminal, &search_state.matches, rows)
        } else {
            Vec::new()
        };
        let active_match = if has_search {
            let index = search_state.current_match_index % search_state.matches.len();
            search_state.matches.get(index)
        } else {
            None
        };

        for (row_idx, row) in grid.iter().enumerate().take(rows) {
            let sel_cols = terminal.row_selection_cols(row_idx);

            for (col_idx, cell) in row.iter().enumerate().take(cols) {
                if cell.flags.wide_continuation() {
                    continue;
                }

                let is_selected = sel_cols
                    .map(|(start, end)| col_idx >= start && col_idx <= end)
                    .unwrap_or(false);
                let is_inverse = cell.flags.inverse();
                let bold = cell.flags.bold();
                let dim = cell.flags.dim();

                if !is_selected
                    && !is_inverse
                    && cell.background == crate::terminal::Color::Default
                    && !has_search
                {
                    continue;
                }

                let mut bg_color = if is_selected {
                    self.theme.selection_color()
                } else if is_inverse {
                    color::resolve_fg_with_palette(
                        cell.foreground,
                        &self.theme,
                        Some(&terminal.dynamic_palette),
                        terminal.dynamic_fg,
                        bold,
                        dim,
                    )
                } else {
                    color::resolve_bg_with_palette(
                        cell.background,
                        &self.theme,
                        Some(&terminal.dynamic_palette),
                        terminal.dynamic_bg,
                    )
                };

                let mut is_search_match = false;
                if has_search {
                    let row_matches = search_map.get(row_idx).map(Vec::as_slice).unwrap_or(&[]);
                    for m in row_matches {
                        if col_idx >= m.col_start && col_idx < m.col_end {
                            is_search_match = true;
                            bg_color = color::resolve_fg_with_palette(
                                cell.foreground,
                                &self.theme,
                                Some(&terminal.dynamic_palette),
                                terminal.dynamic_fg,
                                bold,
                                dim,
                            );
                            if active_match == Some(*m) {
                                let [r, g, b, _a] = bg_color.to_srgba_unmultiplied();
                                bg_color = Color32::from_rgba_unmultiplied(
                                    (r as u16 * 180 / 255) as u8,
                                    (g as u16 * 180 / 255) as u8,
                                    (b as u16 * 180 / 255) as u8,
                                    255,
                                );
                            }
                            break;
                        }
                    }
                }

                if !is_selected
                    && !is_inverse
                    && cell.background == crate::terminal::Color::Default
                    && !is_search_match
                {
                    continue;
                }

                let (x, snapped_width) = snapped_span(content_rect.left(), col_idx, char_width);
                let (y, snapped_height) = snapped_span(content_rect.top(), row_idx, line_height);
                let cell_w = if cell.flags.wide() {
                    let (_, next_width) =
                        snapped_span(content_rect.left(), col_idx + 1, char_width);
                    snapped_width + next_width
                } else {
                    snapped_width
                };
                let cell_rect =
                    egui::Rect::from_min_size(egui::pos2(x, y), Vec2::new(cell_w, snapped_height));
                painter.rect_filled(cell_rect, egui::CornerRadius::ZERO, bg_color);
            }
        }

        self.paint_kitty_image_layer(
            ui.ctx(),
            painter,
            terminal,
            content_rect,
            char_width,
            line_height,
            KittyImageLayer::BelowText,
        );

        // Phase 2: Render characters
        for (row_idx, row) in grid.iter().enumerate().take(rows) {
            let sel_cols = terminal.row_selection_cols(row_idx);
            let (_, snapped_height) = snapped_span(content_rect.top(), row_idx, line_height);
            let y = snapped_span(content_rect.top(), row_idx, line_height).0;

            let mut col_idx = 0;
            while col_idx < cols {
                let cell = &row[col_idx];
                if cell.flags.wide_continuation() || cell.character == ' ' {
                    col_idx += 1;
                    continue;
                }

                let is_selected = sel_cols
                    .map(|(start, end)| col_idx >= start && col_idx <= end)
                    .unwrap_or(false);
                let bold = cell.flags.bold();
                let dim = cell.flags.dim();
                let mut fg_color = if is_selected {
                    self.theme.selection_fg_color()
                } else if cell.flags.inverse() {
                    color::resolve_bg_with_palette(
                        cell.background,
                        &self.theme,
                        Some(&terminal.dynamic_palette),
                        terminal.dynamic_bg,
                    )
                } else {
                    color::resolve_fg_with_palette(
                        cell.foreground,
                        &self.theme,
                        Some(&terminal.dynamic_palette),
                        terminal.dynamic_fg,
                        bold,
                        dim,
                    )
                };

                let is_link = {
                    let row_links = link_map.get(row_idx).map(Vec::as_slice).unwrap_or(&[]);
                    let mut found = false;
                    for link in row_links {
                        if col_idx >= link.col_start && col_idx < link.col_end {
                            let is_hovered_link =
                                hovered_link.as_ref().map(|l| l == *link).unwrap_or(false);
                            if is_hovered_link {
                                fg_color = hovered_link_color();
                            }
                            found = true;
                            break;
                        }
                    }
                    found
                };
                let has_underline =
                    cell.flags.underline() != crate::terminal::UnderlineStyle::None || is_link;
                let has_strikethrough = cell.flags.strikethrough();
                let is_wide = cell.flags.wide();

                let mut font_id = FontId::monospace(self.font_size);
                if bold {
                    font_id.size *= 1.1;
                }

                let galley =
                    ui.painter()
                        .layout_no_wrap(cell.character.to_string(), font_id, fg_color);
                let (cx, cw) = snapped_span(content_rect.left(), col_idx, char_width);
                let text_y = y + (snapped_height - galley.size().y) / 2.0;
                let cell_w = if is_wide {
                    cw + snapped_span(content_rect.left(), col_idx + 1, char_width).1
                } else {
                    cw
                };
                let glyph_x = cx + (cell_w - galley.size().x) / 2.0;
                painter.galley(egui::pos2(glyph_x, text_y), galley, fg_color);

                col_idx += if is_wide { 2 } else { 1 };

                // Decorations
                if has_underline {
                    let (sx, sw) = snapped_span(
                        content_rect.left(),
                        col_idx - if is_wide { 2 } else { 1 },
                        char_width,
                    );
                    let ew = if is_wide {
                        sw + snapped_span(content_rect.left(), col_idx - 1, char_width).1
                    } else {
                        sw
                    };
                    let underline_y = y + line_height - 1.0;
                    painter.line_segment(
                        [
                            egui::pos2(sx, underline_y),
                            egui::pos2(sx + ew, underline_y),
                        ],
                        egui::Stroke::new(1.0, fg_color),
                    );
                }
                if has_strikethrough {
                    let (sx, sw) = snapped_span(
                        content_rect.left(),
                        col_idx - if is_wide { 2 } else { 1 },
                        char_width,
                    );
                    let ew = if is_wide {
                        sw + snapped_span(content_rect.left(), col_idx - 1, char_width).1
                    } else {
                        sw
                    };
                    let strikethrough_y = y + line_height / 2.0;
                    painter.line_segment(
                        [
                            egui::pos2(sx, strikethrough_y),
                            egui::pos2(sx + ew, strikethrough_y),
                        ],
                        egui::Stroke::new(1.0, fg_color),
                    );
                }
            }
        }
    }

    // Protocol modes are passed explicitly so keyboard encoding remains stateless and testable.
    #[allow(clippy::too_many_arguments)]
    pub fn handle_keyboard_input(
        &self,
        _ctx: &egui::Context,
        input: &mut Vec<u8>,
        consumed_keys: &std::collections::HashSet<&str>,
        suppress_text_events: bool,
        keyboard_enhancement_flags: u16,
        report_all_keys_mode: bool,
        xterm_modify_other_keys: u16,
        xterm_format_other_keys: u16,
        application_cursor_keys: bool,
        _alt_screen: bool,
        events: &[egui::Event],
    ) {
        let report_all_keys = report_all_keys_mode || (keyboard_enhancement_flags & 0b1000) != 0;
        let effective_keyboard_flags = if report_all_keys_mode {
            keyboard_enhancement_flags | 0b1000
        } else {
            keyboard_enhancement_flags
        };

        // Collect Text events to detect Caps Lock state.
        // egui doesn't expose Caps Lock in Modifiers, but Text events reflect
        // the actual character produced by the OS (including Caps Lock).
        let mut text_from_events: Option<String> = None;
        if report_all_keys {
            for evt in events {
                if let egui::Event::Text(t) = evt {
                    if !t.is_empty() && t.as_bytes()[0] >= 32 {
                        text_from_events = Some(t.clone());
                        break;
                    }
                }
            }
        }

        for event in events {
            match event {
                egui::Event::Text(text) => {
                    if suppress_text_events {
                        continue;
                    }
                    if !text.is_empty() && text.as_bytes()[0] < 32 {
                        continue;
                    }
                    // Text events already contain the correctly shifted character from the OS.
                    // Always send them - they handle Shift, Caps Lock, etc. correctly.
                    input.extend(text.as_bytes());
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let consumed_name = consumed_key_name(*key, *modifiers);
                    if !consumed_name.is_empty() && consumed_keys.contains(consumed_name.as_str()) {
                        continue;
                    }

                    // Skip Kitty encoding for alphanumeric when there's a corresponding Text event
                    // (Text event already sent the correct character with proper shift/caps handling)
                    if text_from_events
                        .as_ref()
                        .is_some_and(|t| t.len() == 1 && t.as_bytes()[0].is_ascii_alphanumeric())
                    {
                        continue;
                    }

                    // Detect Caps Lock: if Text event has an uppercase letter but
                    // Shift is not pressed, Caps Lock must be active.
                    let caps_lock = text_from_events.as_ref().is_some_and(|t| {
                        t.len() == 1 && t.as_bytes()[0].is_ascii_uppercase() && !modifiers.shift
                    });
                    let effective_modifiers = if caps_lock {
                        egui::Modifiers {
                            shift: true,
                            ..*modifiers
                        }
                    } else {
                        *modifiers
                    };

                    if let Some(encoded) =
                        kitty_encode_key_event(*key, effective_modifiers, effective_keyboard_flags)
                    {
                        input.extend(encoded.as_bytes());
                        continue;
                    }

                    if let Some(encoded) = xterm_encode_modify_other_keys(
                        *key,
                        effective_modifiers,
                        xterm_modify_other_keys,
                        xterm_format_other_keys,
                        report_all_keys_mode,
                    ) {
                        input.extend(encoded.as_bytes());
                        continue;
                    }

                    // Handle normal key sequences
                    let seq = key_to_terminal_sequence(
                        *key,
                        effective_modifiers,
                        application_cursor_keys,
                    );

                    if let Some(s) = seq {
                        input.extend(s.as_bytes());
                    }

                    // Handle Ctrl+letter combinations (send control characters)
                    if modifiers.ctrl && !modifiers.shift && !modifiers.alt && !report_all_keys {
                        match key {
                            egui::Key::A => input.push(0x01), // Ctrl+A
                            egui::Key::B => input.push(0x02), // Ctrl+B (backward page in vim)
                            egui::Key::C => input.push(0x03), // Ctrl+C (SIGINT)
                            egui::Key::D => input.push(0x04),
                            egui::Key::E => input.push(0x05), // Ctrl+E
                            egui::Key::F => input.push(0x06), // Ctrl+F (forward page in vim)
                            egui::Key::G => input.push(0x07), // Ctrl+G
                            egui::Key::H => input.push(0x08), // Ctrl+H (backspace)
                            egui::Key::I => input.push(0x09), // Ctrl+I (tab)
                            egui::Key::J => input.push(0x0a), // Ctrl+J (linefeed)
                            egui::Key::K => input.push(0x0b), // Ctrl+K
                            egui::Key::L => input.push(0x0c), // Ctrl+L (clear screen)
                            egui::Key::M => input.push(0x0d), // Ctrl+M (return)
                            egui::Key::N => input.push(0x0e), // Ctrl+N
                            egui::Key::O => input.push(0x0f), // Ctrl+O
                            egui::Key::P => input.push(0x10), // Ctrl+P
                            egui::Key::Q => input.push(0x11), // Ctrl+Q
                            egui::Key::R => input.push(0x12), // Ctrl+R
                            egui::Key::S => input.push(0x13), // Ctrl+S
                            egui::Key::T => input.push(0x14), // Ctrl+T
                            egui::Key::U => input.push(0x15), // Ctrl+U (delete line in vim)
                            egui::Key::V => input.push(0x16), // Ctrl+V (visual block in vim)
                            egui::Key::W => input.push(0x17), // Ctrl+W
                            egui::Key::X => input.push(0x18), // Ctrl+X
                            egui::Key::Y => input.push(0x19), // Ctrl+Y
                            egui::Key::Z => input.push(0x1a), // Ctrl+Z (suspend)
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_position_uses_content_origin() {
        let content_rect =
            egui::Rect::from_min_size(egui::pos2(12.0, 36.0), egui::vec2(800.0, 400.0));

        let (row, col) = grid_position_from_content(
            egui::pos2(12.0 + 4.0 * 8.0, 36.0 + 2.0 * 20.0),
            content_rect,
            8.0,
            20.0,
            100,
            20,
        );

        assert_eq!((row, col), (2, 4));
    }

    #[test]
    fn cursor_keys_follow_decckm_not_alt_screen() {
        let modifiers = egui::Modifiers::default();

        assert_eq!(
            key_to_terminal_sequence(egui::Key::ArrowUp, modifiers, false).as_deref(),
            Some("\x1b[A")
        );
        assert_eq!(
            key_to_terminal_sequence(egui::Key::ArrowDown, modifiers, true).as_deref(),
            Some("\x1bOB")
        );
    }

    #[test]
    fn modified_function_keys_keep_their_xterm_modifier_parameters() {
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let ctrl_shift = egui::Modifiers {
            shift: true,
            ..ctrl
        };
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };

        // Word-wise cursor motion: the whole point of the parameter.
        assert_eq!(
            key_to_terminal_sequence(egui::Key::ArrowLeft, ctrl, false).as_deref(),
            Some("\x1b[1;5D")
        );
        assert_eq!(
            key_to_terminal_sequence(egui::Key::ArrowRight, ctrl, false).as_deref(),
            Some("\x1b[1;5C")
        );
        // DECCKM must not change the modified form.
        assert_eq!(
            key_to_terminal_sequence(egui::Key::ArrowLeft, ctrl, true).as_deref(),
            Some("\x1b[1;5D")
        );
        assert_eq!(
            key_to_terminal_sequence(egui::Key::Home, ctrl_shift, false).as_deref(),
            Some("\x1b[1;6H")
        );
        assert_eq!(
            key_to_terminal_sequence(egui::Key::End, shift, true).as_deref(),
            Some("\x1b[1;2F")
        );
        assert_eq!(
            key_to_terminal_sequence(egui::Key::PageDown, alt, false).as_deref(),
            Some("\x1b[6;3~")
        );
        assert_eq!(
            key_to_terminal_sequence(egui::Key::Delete, ctrl, false).as_deref(),
            Some("\x1b[3;5~")
        );
        // F1-F4 leave SS3 behind as soon as a modifier is held; F5+ keeps the
        // tilde form with its own numeric code.
        assert_eq!(
            key_to_terminal_sequence(egui::Key::F1, ctrl_shift, false).as_deref(),
            Some("\x1b[1;6P")
        );
        assert_eq!(
            key_to_terminal_sequence(egui::Key::F4, alt, false).as_deref(),
            Some("\x1b[1;3S")
        );
        assert_eq!(
            key_to_terminal_sequence(egui::Key::F5, shift, false).as_deref(),
            Some("\x1b[15;2~")
        );
        assert_eq!(
            key_to_terminal_sequence(egui::Key::F12, ctrl, false).as_deref(),
            Some("\x1b[24;5~")
        );
    }

    /// The scrollback owns Shift+Page, so nothing may also reach the child —
    /// otherwise one keystroke moves both the pane and a full-screen app.
    #[test]
    fn shift_page_keys_belong_to_the_scrollback() {
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            key_to_terminal_sequence(egui::Key::PageUp, shift, false),
            None
        );
        assert_eq!(
            key_to_terminal_sequence(egui::Key::PageDown, shift, false),
            None
        );

        // Ctrl+Shift+Page is a bound command, and the unshifted keys still
        // reach the child, so neither may be swallowed here.
        let ctrl_shift = egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            key_to_terminal_sequence(egui::Key::PageUp, ctrl_shift, false).as_deref(),
            Some("\x1b[5;6~")
        );
        assert_eq!(
            key_to_terminal_sequence(egui::Key::PageUp, egui::Modifiers::default(), false)
                .as_deref(),
            Some("\x1b[5~")
        );
    }

    #[test]
    fn unmodified_function_keys_keep_their_legacy_bytes() {
        let none = egui::Modifiers::default();
        let expected: &[(egui::Key, &str, &str)] = &[
            (egui::Key::ArrowUp, "\x1b[A", "\x1bOA"),
            (egui::Key::ArrowDown, "\x1b[B", "\x1bOB"),
            (egui::Key::ArrowRight, "\x1b[C", "\x1bOC"),
            (egui::Key::ArrowLeft, "\x1b[D", "\x1bOD"),
            (egui::Key::Home, "\x1b[H", "\x1bOH"),
            (egui::Key::End, "\x1b[F", "\x1bOF"),
            (egui::Key::Insert, "\x1b[2~", "\x1b[2~"),
            (egui::Key::Delete, "\x1b[3~", "\x1b[3~"),
            (egui::Key::PageUp, "\x1b[5~", "\x1b[5~"),
            (egui::Key::PageDown, "\x1b[6~", "\x1b[6~"),
            (egui::Key::F1, "\x1bOP", "\x1bOP"),
            (egui::Key::F2, "\x1bOQ", "\x1bOQ"),
            (egui::Key::F3, "\x1bOR", "\x1bOR"),
            (egui::Key::F4, "\x1bOS", "\x1bOS"),
            (egui::Key::F5, "\x1b[15~", "\x1b[15~"),
            (egui::Key::F6, "\x1b[17~", "\x1b[17~"),
            (egui::Key::F7, "\x1b[18~", "\x1b[18~"),
            (egui::Key::F8, "\x1b[19~", "\x1b[19~"),
            (egui::Key::F9, "\x1b[20~", "\x1b[20~"),
            (egui::Key::F10, "\x1b[21~", "\x1b[21~"),
            (egui::Key::F11, "\x1b[23~", "\x1b[23~"),
            (egui::Key::F12, "\x1b[24~", "\x1b[24~"),
        ];

        for (key, normal, application) in expected {
            assert_eq!(
                key_to_terminal_sequence(*key, none, false).as_deref(),
                Some(*normal),
                "{key:?} in normal cursor mode"
            );
            assert_eq!(
                key_to_terminal_sequence(*key, none, true).as_deref(),
                Some(*application),
                "{key:?} in application cursor mode"
            );
        }

        // Keys without a modifier parameter keep the old "silent while a
        // modifier is held" contract, so Ctrl+letter/shortcut handling stays
        // the single owner of those chords.
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        assert_eq!(
            key_to_terminal_sequence(egui::Key::Enter, none, false).as_deref(),
            Some("\r")
        );
        assert!(key_to_terminal_sequence(egui::Key::Enter, ctrl, false).is_none());
        assert!(key_to_terminal_sequence(egui::Key::Backspace, ctrl, false).is_none());
        assert_eq!(
            key_to_terminal_sequence(
                egui::Key::Tab,
                egui::Modifiers {
                    shift: true,
                    ..Default::default()
                },
                false
            )
            .as_deref(),
            Some("\x1b[Z")
        );
    }

    #[test]
    fn ctrl_arrows_reach_the_pty_through_the_full_key_encoder() {
        let renderer = TerminalRenderer::new(
            14.0,
            8.0,
            1.0,
            crate::config::ScrollbarVisibility::Auto,
            crate::theme::Theme::default(),
        );
        let ctrl_left = egui::Event::Key {
            key: egui::Key::ArrowLeft,
            physical_key: Some(egui::Key::ArrowLeft),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            },
        };

        let mut encoded = Vec::new();
        // Kitty disambiguation on: arrows are not text keys, so the CSI-u path
        // must not swallow them before the legacy encoder runs.
        renderer.handle_keyboard_input(
            &egui::Context::default(),
            &mut encoded,
            &std::collections::HashSet::new(),
            false,
            0b1,
            false,
            0,
            0,
            false,
            false,
            std::slice::from_ref(&ctrl_left),
        );
        assert_eq!(encoded, b"\x1b[1;5D");
    }

    #[test]
    fn consumed_shortcuts_are_withheld_from_terminal_keyboard_encoding() {
        let renderer = TerminalRenderer::new(
            14.0,
            8.0,
            1.0,
            crate::config::ScrollbarVisibility::Auto,
            crate::theme::Theme::default(),
        );
        let ctrl_shift_c = egui::Event::Key {
            key: egui::Key::C,
            physical_key: Some(egui::Key::C),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
        };

        let mut encoded = Vec::new();
        renderer.handle_keyboard_input(
            &egui::Context::default(),
            &mut encoded,
            &std::collections::HashSet::new(),
            false,
            0b1,
            false,
            0,
            0,
            false,
            false,
            std::slice::from_ref(&ctrl_shift_c),
        );
        assert_eq!(encoded, b"\x1b[99;6u");

        encoded.clear();
        renderer.handle_keyboard_input(
            &egui::Context::default(),
            &mut encoded,
            &std::collections::HashSet::from(["Ctrl+Shift+C"]),
            false,
            0b1,
            false,
            0,
            0,
            false,
            false,
            &[ctrl_shift_c],
        );
        assert!(encoded.is_empty());

        let ctrl_d = egui::Event::Key {
            key: egui::Key::D,
            physical_key: Some(egui::Key::D),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        };
        renderer.handle_keyboard_input(
            &egui::Context::default(),
            &mut encoded,
            &std::collections::HashSet::new(),
            false,
            0,
            false,
            0,
            0,
            false,
            false,
            &[ctrl_d],
        );
        assert_eq!(encoded, [0x04]);
    }

    #[test]
    fn kitty_z_layers_follow_the_protocol_cutoff() {
        let cutoff = KittyImageLayer::BACKGROUND_CUTOFF;
        assert!(KittyImageLayer::BelowCellBackgrounds.contains(cutoff - 1));
        assert!(KittyImageLayer::BelowText.contains(cutoff));
        assert!(KittyImageLayer::BelowText.contains(-1));
        assert!(KittyImageLayer::AboveText.contains(0));
        assert!(KittyImageLayer::AboveText.contains(i32::MAX));
    }

    #[test]
    fn grid_position_clamps_to_grid_bounds() {
        let content_rect =
            egui::Rect::from_min_size(egui::pos2(12.0, 36.0), egui::vec2(800.0, 400.0));

        let (row, col) = grid_position_from_content(
            egui::pos2(2000.0, 2000.0),
            content_rect,
            8.0,
            20.0,
            100,
            20,
        );

        assert_eq!((row, col), (19, 99));
    }

    #[test]
    fn scrollbar_thumb_fits_tracks_shorter_than_its_normal_minimum() {
        let short_track = 23.15625;
        assert_eq!(
            TerminalRenderer::scrollbar_thumb_height(1, 100, short_track),
            short_track
        );
        assert_eq!(
            TerminalRenderer::scrollbar_thumb_height(1, 100, 100.0),
            TerminalRenderer::MIN_THUMB_HEIGHT
        );
        assert_eq!(TerminalRenderer::scrollbar_thumb_height(1, 100, 0.0), 0.0);
        assert_eq!(
            TerminalRenderer::scrollbar_thumb_height(1, 100, f32::NAN),
            0.0
        );
    }

    #[test]
    fn tiny_pane_layout_rects_do_not_escape_the_pane() {
        let renderer = TerminalRenderer::new(
            14.0,
            8.0,
            1.0,
            crate::config::ScrollbarVisibility::Auto,
            crate::theme::Theme::default(),
        );
        let pane = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(12.0, 10.0));
        let (content, scrollbar) = renderer.layout_rects(pane);

        for rect in [content, scrollbar] {
            assert!(rect.left() >= pane.left());
            assert!(rect.top() >= pane.top());
            assert!(rect.right() <= pane.right());
            assert!(rect.bottom() <= pane.bottom());
        }
    }

    #[test]
    fn deeply_nested_twenty_four_pane_layout_keeps_scrollbars_bounded() {
        let renderer = TerminalRenderer::new(
            14.0,
            8.0,
            1.0,
            crate::config::ScrollbarVisibility::Auto,
            crate::theme::Theme::default(),
        );
        let mut layout = crate::layout::LayoutManager::new(0);
        for session_idx in 1..24 {
            layout.split(session_idx, session_idx % 2 == 0).unwrap();
        }
        layout.compute_pane_rects(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1000.0, 800.0),
        ));

        assert_eq!(layout.panes().len(), 24);
        assert!(layout
            .panes()
            .iter()
            .any(|pane| pane.rect.height() < TerminalRenderer::MIN_THUMB_HEIGHT));
        for pane in layout.panes() {
            let (_, scrollbar) = renderer.layout_rects(pane.rect);
            let track_height = scrollbar.height();
            let thumb_height = TerminalRenderer::scrollbar_thumb_height(1, 100, track_height);
            assert!(thumb_height.is_finite());
            assert!(thumb_height >= 0.0);
            assert!(thumb_height <= track_height);
        }
    }

    #[test]
    fn shift_local_selection_is_locked_at_primary_press() {
        let terminal_a = 11;
        let captured =
            local_selection_capture_after_press(None, terminal_a, true, true, true, true, true);
        assert_eq!(captured, Some(terminal_a));
        assert_eq!(
            local_selection_capture_after_press(
                captured, terminal_a, true, true, true, false, false,
            ),
            Some(terminal_a)
        );
        assert_eq!(
            local_selection_capture_after_press(None, terminal_a, true, true, true, true, false,),
            None
        );
    }

    #[test]
    fn local_selection_capture_cannot_cross_terminal_routes() {
        let terminal_a = 11;
        let terminal_b = 22;
        assert_eq!(
            local_selection_capture_after_press(
                Some(terminal_a),
                terminal_b,
                false,
                true,
                true,
                false,
                false,
            ),
            None
        );
        assert_eq!(
            local_selection_capture_after_press(None, terminal_b, false, true, true, true, false,),
            Some(terminal_b)
        );
    }

    #[test]
    fn only_plain_local_content_click_clears_selection() {
        assert!(should_clear_selection_on_click(
            true, false, true, false, false, false, true,
        ));

        for rejected in [
            // Application-reported click: not a local selection action.
            (false, false, true, false, false, false, true),
            // Ctrl+click is reserved for link opening.
            (true, true, true, false, false, false, true),
            // Multi-click handlers replace the selection themselves.
            (true, false, true, true, false, false, true),
            (true, false, true, false, true, false, true),
            // Scrollbar interaction only moves the viewport.
            (true, false, true, false, false, true, false),
            (true, false, true, false, false, false, false),
        ] {
            assert!(!should_clear_selection_on_click(
                rejected.0, rejected.1, rejected.2, rejected.3, rejected.4, rejected.5, rejected.6,
            ));
        }
    }

    #[test]
    fn terminal_renderers_have_distinct_stable_gpu_surfaces() {
        let renderer_a = TerminalRenderer::new(
            14.0,
            4.0,
            1.0,
            crate::config::ScrollbarVisibility::Auto,
            crate::theme::Theme::default(),
        );
        let mut renderer_b = TerminalRenderer::new(
            14.0,
            4.0,
            1.0,
            crate::config::ScrollbarVisibility::Auto,
            crate::theme::Theme::default(),
        );

        let original_b = renderer_b.gpu_surface_id;
        assert_ne!(renderer_a.gpu_surface_id, original_b);

        // Cache invalidation and runtime font changes must preserve the key so
        // the surface's clean rows remain associated with the same GPU buffer.
        renderer_b.invalidate_font_cache();
        assert_eq!(renderer_b.gpu_surface_id, original_b);
    }
}
