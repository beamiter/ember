use crate::color;
use crate::gpu;
use crate::terminal::{clamp_terminal_dimensions, TerminalState};
use egui::{Color32, FontId, Response, Ui, Vec2};
use lru::LruCache;
use std::num::NonZeroUsize;

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

fn key_to_terminal_sequence(
    key: egui::Key,
    modifiers: egui::Modifiers,
    application_cursor_keys: bool,
) -> Option<&'static str> {
    if modifiers.ctrl || modifiers.alt || modifiers.mac_cmd || modifiers.command_only() {
        return None;
    }

    match key {
        egui::Key::Enter => Some("\r"),
        egui::Key::Escape => Some("\x1b"),
        egui::Key::Backspace => Some("\x7f"), // Send DEL (0x7f)
        egui::Key::Tab => {
            if modifiers.shift {
                Some("\x1b[Z") // Shift+Tab -> backtab (CSI Z)
            } else {
                Some("\t")
            }
        }
        egui::Key::ArrowUp => {
            if application_cursor_keys {
                Some("\x1bOA")
            } else {
                Some("\x1b[A")
            }
        }
        egui::Key::ArrowDown => {
            if application_cursor_keys {
                Some("\x1bOB")
            } else {
                Some("\x1b[B")
            }
        }
        egui::Key::ArrowRight => {
            if application_cursor_keys {
                Some("\x1bOC")
            } else {
                Some("\x1b[C")
            }
        }
        egui::Key::ArrowLeft => {
            if application_cursor_keys {
                Some("\x1bOD")
            } else {
                Some("\x1b[D")
            }
        }
        egui::Key::Home => {
            if application_cursor_keys {
                Some("\x1bOH")
            } else {
                Some("\x1b[H")
            }
        }
        egui::Key::End => {
            if application_cursor_keys {
                Some("\x1bOF")
            } else {
                Some("\x1b[F")
            }
        }
        egui::Key::Insert => Some("\x1b[2~"),
        egui::Key::Delete => Some("\x1b[3~"),
        egui::Key::PageUp => Some("\x1b[5~"),
        egui::Key::PageDown => Some("\x1b[6~"),
        egui::Key::F1 => Some("\x1bOP"),
        egui::Key::F2 => Some("\x1bOQ"),
        egui::Key::F3 => Some("\x1bOR"),
        egui::Key::F4 => Some("\x1bOS"),
        egui::Key::F5 => Some("\x1b[15~"),
        egui::Key::F6 => Some("\x1b[17~"),
        egui::Key::F7 => Some("\x1b[18~"),
        egui::Key::F8 => Some("\x1b[19~"),
        egui::Key::F9 => Some("\x1b[20~"),
        egui::Key::F10 => Some("\x1b[21~"),
        egui::Key::F11 => Some("\x1b[23~"),
        egui::Key::F12 => Some("\x1b[24~"),
        _ => None,
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
    let should_encode = report_all_keys
        || modifiers.ctrl
        || modifiers.alt
        || (modifiers.command && !modifiers.ctrl);
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
    let has_non_shift_modifier =
        modifiers.ctrl || modifiers.alt || (modifiers.command && !modifiers.ctrl);
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

pub struct TerminalRenderer {
    pub font_size: f32,
    pub char_width: f32,
    pub line_height: f32,
    pub padding: f32,
    pub line_spacing: f32,
    pub dragging_scrollbar: bool,
    pub scrollbar_visibility: crate::config::ScrollbarVisibility,
    pub theme: crate::theme::Theme,
    requested_initial_focus: bool,
    ime_enabled: bool,
    last_ime_rect: Option<egui::Rect>,
    // Kitty graphics texture cache with LRU eviction (max 100 images)
    // Prevents unbounded memory growth from image sequences
    texture_cache: LruCache<u32, (egui::TextureHandle, u32, u32)>,
    /// The content rect from the last render, used for mouse-to-grid coordinate conversion
    pub last_content_rect: Option<egui::Rect>,
    pub opacity: f32,
    /// Whether to enable font ligatures (rustybuzz shaping of ASCII runs)
    pub font_ligatures: bool,
    /// Whether to use GPU-accelerated grid rendering
    pub gpu_rendering: bool,
    /// wgpu render state for GPU-accelerated grid rendering
    pub wgpu_render_state: Option<egui_wgpu::RenderState>,
    /// Pending cursor movement input (arrow keys) from mouse clicks
    pub cursor_move_input: Vec<u8>,
    /// Sub-line pixel offset for smooth scrolling animation
    pub scroll_pixel_offset: f32,

    /// Per-pane 链接检测缓存(多窗格路径用)。单窗格走 App 上的缓存,多窗格每个
    /// pane 一份,仅当 grid_version/scroll_offset 变化时重建,避免每帧重做检测+String 分配。
    /// 用 Arc 以便渲染前 O(1) clone 出来,规避 &mut self 与字段 & 的借用冲突。
    pub cached_links: std::sync::Arc<Vec<crate::link::Link>>,
    pub cached_links_grid_version: u64,
    pub cached_links_scroll_offset: usize,

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
}

impl TerminalRenderer {
    const SCROLLBAR_WIDTH: f32 = 8.0;
    const SCROLLBAR_GAP: f32 = 2.0;
    const MIN_THUMB_HEIGHT: f32 = 24.0;
    const SCROLLBAR_HIT_EXPAND: f32 = 8.0;

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
            scrollbar_visibility,
            theme,
            requested_initial_focus: false,
            ime_enabled: false,
            last_content_rect: None,
            last_ime_rect: None,
            opacity: 1.0,
            font_ligatures: true,
            gpu_rendering: true,
            texture_cache: LruCache::new(NonZeroUsize::new(100).unwrap()),
            wgpu_render_state: None,
            cursor_move_input: Vec::new(),
            scroll_pixel_offset: 0.0,
            cached_links: std::sync::Arc::new(Vec::new()),
            // u64::MAX 作哨兵,保证首帧必定重建链接缓存。
            cached_links_grid_version: u64::MAX,
            cached_links_scroll_offset: usize::MAX,
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
        }
    }

    /// 重置 renderer 的 IME 状态缓存，使下一帧重新同步 IME 状态
    pub fn reset_ime_state(&mut self) {
        self.ime_enabled = false;
        self.last_ime_rect = None;
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
        if let Some((handle, _w, _h)) = self.texture_cache.get(&image_id) {
            return Some(handle.clone());
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
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [image.width as usize, image.height as usize],
            &image.data,
        );
        let handle = ctx.load_texture(
            format!("kitty_{}", image_id),
            color_image,
            Default::default(),
        );

        // Cache it (LRU will auto-evict oldest if at capacity)
        let result = handle.clone();
        self.texture_cache
            .put(image_id, (handle, image.width, image.height));
        Some(result)
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

    fn layout_rects(&self, rect: egui::Rect) -> (egui::Rect, egui::Rect) {
        let outer_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + self.padding, rect.top() + self.padding),
            egui::pos2(
                (rect.right() - self.padding).max(rect.left() + self.char_width),
                (rect.bottom() - self.padding).max(rect.top() + self.line_height),
            ),
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

    pub fn grid_dimensions(&self, available: Vec2) -> (usize, usize) {
        let content_size = self.content_size(available);
        let usable_width = content_size.x;
        let usable_height = content_size.y;

        let cols = (usable_width / self.char_width).floor().max(1.0) as usize;
        let rows = (usable_height / self.line_height).floor().max(1.0) as usize;

        clamp_terminal_dimensions(cols, rows)
    }

    /// 在指定矩形内渲染（用于多窗格模式）
    pub fn render_in_rect(
        &mut self,
        ui: &mut Ui,
        terminal: &mut TerminalState,
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
        let response = ui
            .allocate_exact_size(
                egui::vec2(rect.width(), rect.height()),
                egui::Sense::click_and_drag().union(egui::Sense::focusable_noninteractive()),
            )
            .1;

        self.render_terminal_at_rect(
            ui,
            terminal,
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

    pub fn render(
        &mut self,
        ui: &mut Ui,
        terminal: &mut TerminalState,
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
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(available_width, available_height),
            egui::Sense::click_and_drag().union(egui::Sense::focusable_noninteractive()),
        );

        self.render_terminal_at_rect(
            ui,
            terminal,
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

    fn render_terminal_at_rect(
        &mut self,
        ui: &mut Ui,
        terminal: &mut TerminalState,
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
        let grid = terminal.get_visible_cells();
        let rows = grid.len();
        let cols = if rows > 0 { grid[0].len() } else { 80 };

        // eprintln!("[UI] Rect: {:?}", rect);

        let painter = ui.painter_at(rect);
        let bg = self.theme.terminal_background();
        let bg_with_opacity = egui::Color32::from_rgba_unmultiplied(
            bg.r(),
            bg.g(),
            bg.b(),
            (self.opacity * 255.0) as u8,
        );
        painter.rect_filled(rect, egui::CornerRadius::ZERO, bg_with_opacity);

        let (content_rect, scrollbar_rect) = self.layout_rects(rect);
        self.last_content_rect = Some(content_rect);
        let cursor_pos = terminal.get_cursor_pos();
        let ime_rect = cursor_rect(
            content_rect,
            cursor_pos.0,
            cursor_pos.1,
            char_width,
            line_height,
        );

        let ctx = ui.ctx();
        if response.clicked()
            || (!self.requested_initial_focus && !ctx.memory(|mem| mem.has_focus(response.id)))
        {
            response.request_focus();
            self.requested_initial_focus = true;
        }

        let has_focus = ctx.memory(|mem| mem.has_focus(response.id));
        if has_focus {
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
        if !self.ime_enabled {
            ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::IMEPurpose(
                egui::IMEPurpose::Terminal,
            ));
            self.ime_enabled = true;
        }

        {
            let ime_rect_changed = self
                .last_ime_rect
                .map(|prev| prev != ime_rect)
                .unwrap_or(true);
            if ime_rect_changed {
                ctx.send_viewport_cmd(egui::ViewportCommand::IMERect(ime_rect));
                self.last_ime_rect = Some(ime_rect);
            }
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

        // Compute thumb rect and related values for interaction
        let scrollbar_thumb_rect: Option<(egui::Rect, f32, f32, f32)> =
            if !terminal.scrollback.is_empty() {
                let total_lines = terminal.scrollback.len() + rows;
                let visible_lines = rows;
                if total_lines > visible_lines {
                    let scrollbar_height = scrollbar_rect.height();
                    let thumb_height = ((visible_lines as f32 / total_lines as f32)
                        * scrollbar_height)
                        .clamp(Self::MIN_THUMB_HEIGHT, scrollbar_height);
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
        if response.drag_started() {
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
        if self.dragging_scrollbar && response.dragged() {
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
                        terminal.scroll_offset = new_offset.min(terminal.scrollback.len());
                    }
                }
            }
        }

        // Reset dragging state when mouse released
        if response.drag_stopped() {
            self.dragging_scrollbar = false;
        }

        // Click in scrollbar track (not on thumb): page up/down
        if response.drag_started() && !self.dragging_scrollbar {
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

        // Single click: move cursor to click position (when mouse reporting is disabled)
        if response.clicked() && !response.double_clicked() && !self.dragging_scrollbar {
            terminal.selection = None;

            if let Some(pos) = response.interact_pointer_pos() {
                if pos.x < scrollbar_x && !terminal.is_mouse_enabled() {
                    let (click_row, click_col) = grid_position_from_content(
                        pos,
                        content_rect,
                        char_width,
                        line_height,
                        cols,
                        rows,
                    );

                    let (cursor_row, cursor_col) = terminal.get_cursor_pos();

                    if click_row == cursor_row {
                        let col_diff = click_col as isize - cursor_col as isize;

                        if col_diff != 0 {
                            self.cursor_move_input.clear();
                            let app_cursor_keys = terminal.is_application_cursor_keys();
                            let (right, left): (&[u8], &[u8]) = if app_cursor_keys {
                                (b"\x1bOC", b"\x1bOD")
                            } else {
                                (b"\x1b[C", b"\x1b[D")
                            };
                            let arrow_seq = if col_diff > 0 { right } else { left };

                            for _ in 0..col_diff.unsigned_abs() {
                                self.cursor_move_input.extend_from_slice(arrow_seq);
                            }
                        }
                    }
                }
            }
        }

        // Double-click: select word at cursor position
        if response.double_clicked() && !self.dragging_scrollbar {
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
                }
            }
        }

        // Text selection: only when not interacting with scrollbar
        if response.drag_started() && !self.dragging_scrollbar {
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
        if response.dragged() && !self.dragging_scrollbar {
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

        // Render Kitty graphics images
        let placements = terminal.kitty_graphics.get_placements();
        for placement in placements {
            if let Some(image) = terminal.kitty_graphics.get_image(placement.image_id) {
                // Calculate pixel position from grid coordinates
                let img_x = content_rect.left() + placement.x as f32 * char_width;
                let img_y = content_rect.top() + placement.y as f32 * line_height;
                let img_width = placement.width as f32 * char_width;
                let img_height = placement.height as f32 * line_height;

                let rect = egui::Rect::from_min_size(
                    egui::pos2(img_x, img_y),
                    Vec2::new(img_width, img_height),
                );

                // Render the image using GPU texture
                if let Some(texture) = self.get_image_texture(ui.ctx(), image.id, image) {
                    // Render the texture
                    let mesh = egui::Mesh {
                        indices: vec![0, 1, 2, 0, 2, 3],
                        vertices: vec![
                            egui::epaint::Vertex {
                                pos: rect.left_top(),
                                uv: egui::pos2(0.0, 0.0),
                                color: Color32::WHITE,
                            },
                            egui::epaint::Vertex {
                                pos: rect.right_top(),
                                uv: egui::pos2(1.0, 0.0),
                                color: Color32::WHITE,
                            },
                            egui::epaint::Vertex {
                                pos: rect.right_bottom(),
                                uv: egui::pos2(1.0, 1.0),
                                color: Color32::WHITE,
                            },
                            egui::epaint::Vertex {
                                pos: rect.left_bottom(),
                                uv: egui::pos2(0.0, 1.0),
                                color: Color32::WHITE,
                            },
                        ],
                        texture_id: texture.id(),
                    };
                    painter.add(egui::Shape::mesh(mesh));

                    // Draw border and info
                    painter.rect_stroke(
                        rect,
                        egui::CornerRadius::ZERO,
                        egui::Stroke::new(1.0, Color32::from_rgb(100, 150, 200)),
                        egui::StrokeKind::Middle,
                    );

                    let info = format!("#{} ({}×{})", image.id, image.width, image.height);
                    let font_id = FontId::monospace(self.font_size * 0.6);
                    let galley = ui.painter().layout_no_wrap(
                        info,
                        font_id,
                        Color32::from_rgb(100, 150, 200),
                    );

                    painter.galley(
                        egui::pos2(img_x + 2.0, img_y + 2.0),
                        galley,
                        Color32::from_rgb(100, 150, 200),
                    );

                    crate::debug_log!(
                        "[KITTY_RENDER] Rendered image #{} at ({},{}) size {}x{} placement {}x{}",
                        image.id,
                        placement.x,
                        placement.y,
                        image.width,
                        image.height,
                        placement.width,
                        placement.height
                    );
                } else {
                    // Render placeholder if image preparation failed
                    painter.rect_filled(
                        rect,
                        egui::CornerRadius::ZERO,
                        Color32::from_rgba_unmultiplied(50, 50, 50, 100),
                    );

                    painter.rect_stroke(
                        rect,
                        egui::CornerRadius::ZERO,
                        egui::Stroke::new(1.0, Color32::from_rgb(100, 100, 100)),
                        egui::StrokeKind::Middle,
                    );

                    let text = "Invalid Image";
                    let font_id = FontId::monospace(self.font_size * 0.6);
                    let galley = ui.painter().layout_no_wrap(
                        text.to_string(),
                        font_id,
                        Color32::from_rgb(100, 100, 100),
                    );

                    painter.galley(
                        egui::pos2(img_x + 2.0, img_y + 2.0),
                        galley,
                        Color32::from_rgb(100, 100, 100),
                    );
                }
            }
        }

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

                match &terminal.cursor_shape {
                    crate::terminal::CursorShape::Block => {
                        let cursor_c = self.theme.cursor_color();
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
                            egui::Stroke::new(0.8, self.theme.cursor_color()),
                        );
                    }
                    crate::terminal::CursorShape::Beam => {
                        painter.line_segment(
                            [
                                egui::pos2(x + 0.25, y),
                                egui::pos2(x + 0.25, y + line_height),
                            ],
                            egui::Stroke::new(0.8, self.theme.cursor_color()),
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
                let thumb_height = ((visible_lines as f32 / total_lines as f32) * scrollbar_height)
                    .clamp(Self::MIN_THUMB_HEIGHT, scrollbar_height);
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
        }

        // Clear dirty region after rendering
        terminal.dirty_region.clear();

        response
    }

    /// GPU path: build instance buffer from grid, rasterize new glyphs, emit PaintCallback.
    /// Returns true if GPU rendering was used, false if fallback is needed.
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
        let default_bg = self.theme.terminal_background();
        let has_search = !search_state.matches.is_empty() && !search_state.query.is_empty();
        let target_cell_width = char_width * ppp;
        let target_cell_height = line_height * ppp;

        // --- Dirty detection: determine which rows need rebuild ---
        let terminal_ptr = terminal as *const _ as usize;
        let current_grid_version = terminal.get_grid_version();
        let current_scroll_offset = terminal.scroll_offset;
        let current_selection = terminal.selection;
        let search_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            search_state.query.hash(&mut h);
            search_state.matches.len().hash(&mut h);
            search_state.current_match_index.hash(&mut h);
            h.finish()
        };

        // Detect major screen changes (e.g., alternate screen switch)
        let grid_version_jumped =
            current_grid_version > self.last_rendered_grid_version + rows as u64;

        let need_full_rebuild = self.cached_instances.is_empty()
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
                // Mark rows affected by old and new selection
                Self::mark_selection_rows(
                    &self.last_rendered_selection,
                    rows,
                    dirty_rows.as_mut_slice(),
                    terminal,
                );
                Self::mark_selection_rows(
                    &current_selection,
                    rows,
                    dirty_rows.as_mut_slice(),
                    terminal,
                );
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
                    if m.line < dirty_rows.len() {
                        dirty_rows[m.line] = true;
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

                let search_map: Vec<Vec<&crate::search::SearchMatch>> = if !has_search {
                    Vec::new()
                } else {
                    let mut map = vec![Vec::new(); rows];
                    for m in &search_state.matches {
                        if m.line < rows {
                            map[m.line].push(m);
                        }
                    }
                    map
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
                            self.opacity,
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

                    for row_idx in 0..rows {
                        if !dirty_rows[row_idx] {
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
                            self.opacity,
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
                                self.opacity,
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
            } // drop renderer write lock
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
            self.last_search_match_lines.push(m.line);
        }
        // 绝大多数帧 hovered_link 不变,先比较再 clone 以省去 String 堆分配。
        if self.last_rendered_hovered_link != *hovered_link {
            self.last_rendered_hovered_link = hovered_link.clone();
        }
        self.last_rendered_cols = cols;
        self.last_rendered_rows = rows;

        let (atlas_w, atlas_h) = (self.cached_atlas_w, self.cached_atlas_h);

        let instance_count = self.cached_instances.len() as u32;
        let background_uniforms = gpu::instance::GridUniforms {
            viewport_width: content_rect.width() * ppp,
            viewport_height: content_rect.height() * ppp,
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
            instances: self.cached_instances.clone(),
            uniforms: background_uniforms,
            instance_count,
            row_offsets: self.row_instance_offsets.clone(),
            row_counts: self.row_instance_counts.clone(),
            dirty_rows: self.dirty_rows.clone(),
            use_partial_upload: !need_full_rebuild && !did_full_relayout && any_dirty,
        };

        let foreground_callback = gpu::callback::GridForegroundCallback {
            uniforms: foreground_uniforms,
            instance_count,
        };

        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            content_rect,
            background_callback,
        ));
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            content_rect,
            foreground_callback,
        ));
        true
    }

    fn mark_selection_rows(
        selection: &Option<crate::terminal::Selection>,
        rows: usize,
        dirty_rows: &mut [bool],
        terminal: &TerminalState,
    ) {
        if let Some(sel) = selection {
            let min_abs = sel.anchor.0.min(sel.active.0);
            let max_abs = sel.anchor.0.max(sel.active.0);
            let scrollback_offset = terminal
                .scrollback
                .len()
                .saturating_sub(terminal.scroll_offset);
            let start = min_abs.max(scrollback_offset) - scrollback_offset;
            let end = max_abs.saturating_sub(scrollback_offset);
            for r in start..=end.min(rows.saturating_sub(1)) {
                dirty_rows[r] = true;
            }
        }
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
        opacity: f32,
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
                        for col2 in run_start..c {
                            map[col2] = Some(LigOverride::Covered);
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
            search_state.matches.get(idx).map(|m| (m.line, m.col_start))
        } else {
            None
        };

        for col_idx in 0..cols {
            let cell = &grid[row_idx][col_idx];
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
                color::resolve_fg(cell.foreground, theme, bold, dim)
            } else {
                color::resolve_bg(cell.background, theme)
            };

            if has_search {
                let row_matches = search_map.get(row_idx).map(Vec::as_slice).unwrap_or(&[]);
                if !row_matches.is_empty() {
                    for m in row_matches.iter() {
                        if col_idx >= m.col_start && col_idx < m.col_end {
                            bg_color = color::resolve_fg(cell.foreground, theme, bold, dim);
                            if active_match_pos == Some((m.line, m.col_start)) {
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

            if !is_selected
                && !is_inverse
                && cell.background == crate::terminal::Color::Default
                && !has_search
            {
                bg_color = default_bg;
            }

            let mut fg_color = if is_selected {
                theme.selection_fg_color()
            } else if is_inverse {
                color::resolve_bg(cell.background, theme)
            } else {
                color::resolve_fg(cell.foreground, theme, bold, dim)
            };

            let is_link = {
                let row_links = link_map.get(row_idx).map(Vec::as_slice).unwrap_or(&[]);
                let mut found = false;
                for link in row_links {
                    if col_idx >= link.col_start && col_idx < link.col_end {
                        let is_hovered_link =
                            hovered_link.as_ref().map(|l| l == *link).unwrap_or(false);
                        fg_color = if is_hovered_link {
                            Color32::from_rgb(100, 200, 255)
                        } else {
                            Color32::from_rgb(50, 150, 255)
                        };
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
            let bg_a = if bg_color == default_bg {
                (opacity * 255.0) as u8
            } else {
                255u8
            };

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
    fn render_grid_cpu(
        &self,
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
        let default_bg = self.theme.terminal_background();
        let has_search = !search_state.matches.is_empty() && !search_state.query.is_empty();

        for row_idx in 0..rows {
            let sel_cols = terminal.row_selection_cols(row_idx);

            for col_idx in 0..cols {
                let cell = &grid[row_idx][col_idx];
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
                    color::resolve_fg(cell.foreground, &self.theme, bold, dim)
                } else {
                    color::resolve_bg(cell.background, &self.theme)
                };

                if has_search {
                    for (match_idx, m) in search_state.matches.iter().enumerate() {
                        if m.line == row_idx && col_idx >= m.col_start && col_idx < m.col_end {
                            bg_color = color::resolve_fg(cell.foreground, &self.theme, bold, dim);
                            if match_idx
                                == search_state.current_match_index % search_state.matches.len()
                            {
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
                } else if bg_color == default_bg {
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

        // Phase 2: Render characters
        for row_idx in 0..rows {
            let sel_cols = terminal.row_selection_cols(row_idx);
            let (_, snapped_height) = snapped_span(content_rect.top(), row_idx, line_height);
            let y = snapped_span(content_rect.top(), row_idx, line_height).0;

            let mut col_idx = 0;
            while col_idx < cols {
                let cell = &grid[row_idx][col_idx];
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
                    color::resolve_bg(cell.background, &self.theme)
                } else {
                    color::resolve_fg(cell.foreground, &self.theme, bold, dim)
                };

                let is_link = {
                    let row_links = link_map.get(row_idx).map(Vec::as_slice).unwrap_or(&[]);
                    let mut found = false;
                    for link in row_links {
                        if col_idx >= link.col_start && col_idx < link.col_end {
                            let is_hovered_link =
                                hovered_link.as_ref().map(|l| l == *link).unwrap_or(false);
                            fg_color = if is_hovered_link {
                                Color32::from_rgb(100, 200, 255)
                            } else {
                                Color32::from_rgb(50, 150, 255)
                            };
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

    pub fn handle_keyboard_input(
        &self,
        _ctx: &egui::Context,
        input: &mut Vec<u8>,
        _consumed_keys: &std::collections::HashSet<&str>,
        suppress_text_events: bool,
        keyboard_enhancement_flags: u16,
        report_all_keys_mode: bool,
        xterm_modify_other_keys: u16,
        xterm_format_other_keys: u16,
        application_cursor_keys: bool,
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
                    // Skip Ctrl+Shift+C/V/X - these will be handled in main.rs for copy/paste
                    if modifiers.ctrl && modifiers.shift && !modifiers.alt {
                        match key {
                            egui::Key::C | egui::Key::V | egui::Key::X => {
                                continue;
                            }
                            _ => {}
                        }
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
                            egui::Key::D => {} // Ctrl+D (handled by keybindings system - close session/EOF)
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
