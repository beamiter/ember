use crate::terminal::{Color, DynamicColorPalette};
use crate::theme::Theme;
use crate::theme::ThemeExt as _;
use egui::Color32;

/// Map a Color enum variant to an index into the theme's 16-color ANSI palette.
fn ansi_index(color: Color) -> Option<usize> {
    match color {
        Color::Black => Some(0),
        Color::Red => Some(1),
        Color::Green => Some(2),
        Color::Yellow => Some(3),
        Color::Blue => Some(4),
        Color::Magenta => Some(5),
        Color::Cyan => Some(6),
        Color::White => Some(7),
        Color::BrightBlack => Some(8),
        Color::BrightRed => Some(9),
        Color::BrightGreen => Some(10),
        Color::BrightYellow => Some(11),
        Color::BrightBlue => Some(12),
        Color::BrightMagenta => Some(13),
        Color::BrightCyan => Some(14),
        Color::BrightWhite => Some(15),
        _ => None,
    }
}

/// Resolve a foreground color using the theme palette, with VTE4-compatible
/// bold-brightening and dim attenuation.
#[allow(dead_code)]
pub fn resolve_fg(color: Color, theme: &Theme, bold: bool, dim: bool) -> Color32 {
    resolve_fg_with_palette(color, theme, None, None, bold, dim)
}

/// Like [`resolve_fg`], but OSC 4 palette overrides win over the theme for
/// indexed and named ANSI colors.
pub fn resolve_fg_with_palette(
    color: Color,
    theme: &Theme,
    palette: Option<&DynamicColorPalette>,
    dynamic_fg: Option<(u8, u8, u8)>,
    bold: bool,
    dim: bool,
) -> Color32 {
    let base = match color {
        // OSC 10 dynamic foreground overrides the theme default.
        Color::Default => dynamic_fg
            .map(|(r, g, b)| Color32::from_rgb(r, g, b))
            .unwrap_or_else(|| theme.terminal_foreground()),
        Color::Indexed(idx) => color_256_with_palette(idx, theme, palette),
        Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        _ => {
            // 具名 ANSI 色必命中 ansi_index;兜底 7(white) 仅防未来新增 Color 变体漏配。
            let idx = ansi_index(color).unwrap_or(7);
            // VTE4: bold + standard color (0-7) promotes to bright variant (8-15)
            let idx = if bold && idx < 8 { idx + 8 } else { idx };
            palette
                .and_then(|p| p[idx])
                .map(|(r, g, b)| Color32::from_rgb(r, g, b))
                .unwrap_or_else(|| theme.ansi_color(idx))
        }
    };
    if dim {
        let [r, g, b, a] = base.to_srgba_unmultiplied();
        Color32::from_rgba_unmultiplied(
            (r as u16 * 2 / 3) as u8,
            (g as u16 * 2 / 3) as u8,
            (b as u16 * 2 / 3) as u8,
            a,
        )
    } else {
        base
    }
}

/// Resolve a background color using the theme palette.
#[allow(dead_code)]
pub fn resolve_bg(color: Color, theme: &Theme) -> Color32 {
    resolve_bg_with_palette(color, theme, None, None)
}

/// Like [`resolve_bg`], but OSC 4 palette overrides win over the theme.
pub fn resolve_bg_with_palette(
    color: Color,
    theme: &Theme,
    palette: Option<&DynamicColorPalette>,
    dynamic_bg: Option<(u8, u8, u8)>,
) -> Color32 {
    match color {
        // OSC 11 dynamic background overrides the theme default.
        Color::Default => dynamic_bg
            .map(|(r, g, b)| Color32::from_rgb(r, g, b))
            .unwrap_or_else(|| theme.terminal_background()),
        Color::Indexed(idx) => color_256_with_palette(idx, theme, palette),
        Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        _ => {
            // 具名 ANSI 色必命中;兜底 0(black) 仅防未来新增 Color 变体漏配。
            let idx = ansi_index(color).unwrap_or(0);
            palette
                .and_then(|p| p[idx])
                .map(|(r, g, b)| Color32::from_rgb(r, g, b))
                .unwrap_or_else(|| theme.ansi_color(idx))
        }
    }
}

/// 256-color palette resolution using theme colors for indices 0-15.
#[allow(dead_code)]
pub fn color_256(idx: u8, theme: &Theme) -> Color32 {
    color_256_with_palette(idx, theme, None)
}

/// Like [`color_256`], but an OSC 4 override wins for any index.
pub fn color_256_with_palette(
    idx: u8,
    theme: &Theme,
    palette: Option<&DynamicColorPalette>,
) -> Color32 {
    if let Some((r, g, b)) = palette.and_then(|p| p[idx as usize]) {
        return Color32::from_rgb(r, g, b);
    }
    match idx {
        0..=15 => theme.ansi_color(idx as usize),
        16..=231 => {
            let idx = idx - 16;
            let r_idx = idx / 36;
            let g_idx = (idx % 36) / 6;
            let b_idx = idx % 6;
            let r = if r_idx == 0 { 0 } else { 55 + r_idx * 40 };
            let g = if g_idx == 0 { 0 } else { 55 + g_idx * 40 };
            let b = if b_idx == 0 { 0 } else { 55 + b_idx * 40 };
            Color32::from_rgb(r, g, b)
        }
        232..=255 => {
            let gray = 8 + (idx - 232) * 10;
            Color32::from_rgb(gray, gray, gray)
        }
    }
}
