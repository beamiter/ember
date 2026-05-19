use crate::terminal::Color;
use egui::Color32;

pub fn to_egui_color32(color: Color) -> Color32 {
    match color {
        Color::Black => Color32::from_rgb(50, 50, 50),
        Color::Red => Color32::from_rgb(255, 85, 85),
        Color::Green => Color32::from_rgb(85, 255, 85),
        Color::Yellow => Color32::from_rgb(255, 255, 85),
        Color::Blue => Color32::from_rgb(85, 85, 255),
        Color::Magenta => Color32::from_rgb(255, 85, 255),
        Color::Cyan => Color32::from_rgb(85, 255, 255),
        Color::White => Color32::from_rgb(255, 255, 255),
        Color::BrightBlack => Color32::from_rgb(127, 127, 127),
        Color::BrightRed => Color32::from_rgb(255, 85, 85),
        Color::BrightGreen => Color32::from_rgb(85, 255, 85),
        Color::BrightYellow => Color32::from_rgb(255, 255, 85),
        Color::BrightBlue => Color32::from_rgb(85, 85, 255),
        Color::BrightMagenta => Color32::from_rgb(255, 85, 255),
        Color::BrightCyan => Color32::from_rgb(85, 255, 255),
        Color::BrightWhite => Color32::from_rgb(255, 255, 255),
        Color::Indexed(idx) => color_256(idx),
        Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        Color::Default => Color32::from_rgb(229, 229, 229),
    }
}

pub fn color_256(idx: u8) -> Color32 {
    match idx {
        0..=15 => {
            let colors = [
                Color32::from_rgb(0, 0, 0),
                Color32::from_rgb(205, 49, 49),
                Color32::from_rgb(13, 188, 121),
                Color32::from_rgb(229, 229, 16),
                Color32::from_rgb(36, 114, 200),
                Color32::from_rgb(188, 63, 60),
                Color32::from_rgb(17, 168, 205),
                Color32::from_rgb(229, 229, 229),
                Color32::from_rgb(127, 127, 127),
                Color32::from_rgb(255, 85, 85),
                Color32::from_rgb(85, 255, 85),
                Color32::from_rgb(255, 255, 85),
                Color32::from_rgb(85, 85, 255),
                Color32::from_rgb(255, 85, 255),
                Color32::from_rgb(85, 255, 255),
                Color32::from_rgb(255, 255, 255),
            ];
            colors[idx as usize]
        }
        16..=231 => {
            let idx = idx - 16;
            let r = (idx / 36) * 51;
            let g = ((idx % 36) / 6) * 51;
            let b = (idx % 6) * 51;
            Color32::from_rgb(r, g, b)
        }
        232..=255 => {
            let gray = 8 + (idx - 232) * 10;
            Color32::from_rgb(gray, gray, gray)
        }
    }
}

#[allow(dead_code)]
pub mod defaults {
    use egui::Color32;

    pub const FOREGROUND: Color32 = Color32::from_rgb(229, 229, 229);
    pub const BACKGROUND: Color32 = Color32::from_rgb(29, 29, 29);
    pub const CURSOR: Color32 = Color32::from_rgb(127, 127, 127);
    pub fn selection() -> Color32 {
        Color32::from_rgba_unmultiplied(200, 200, 200, 100)
    }
}
