//! Theme data (structs, builtins, custom-theme persistence) lives in
//! `jterm_core::theme`. This module re-exports it and adds the egui color
//! conversions as an extension trait.

use egui::Color32;

pub use jterm_core::theme::*;

/// egui color views over the shared RGB theme data.
pub trait ThemeExt {
    fn rgb_to_color32(rgb: [u8; 3]) -> Color32;
    fn rgba_to_color32(rgba: [u8; 4]) -> Color32;
    fn terminal_foreground(&self) -> Color32;
    fn terminal_background(&self) -> Color32;
    fn cursor_color(&self) -> Color32;
    fn selection_color(&self) -> Color32;
    fn selection_fg_color(&self) -> Color32;
    fn ansi_color(&self, index: usize) -> Color32;
}

impl ThemeExt for Theme {
    /// 将 RGB 数组转换为 Color32
    fn rgb_to_color32(rgb: [u8; 3]) -> Color32 {
        Color32::from_rgb(rgb[0], rgb[1], rgb[2])
    }

    /// 将 RGBA 数组转换为 Color32
    fn rgba_to_color32(rgba: [u8; 4]) -> Color32 {
        Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
    }

    /// 获取终端前景色
    fn terminal_foreground(&self) -> Color32 {
        Self::rgb_to_color32(self.terminal.foreground)
    }

    /// 获取终端背景色
    fn terminal_background(&self) -> Color32 {
        Self::rgb_to_color32(self.terminal.background)
    }

    /// 获取光标颜色
    fn cursor_color(&self) -> Color32 {
        Self::rgb_to_color32(self.terminal.cursor)
    }

    /// 获取选择背景色 - 基于前景色计算，确保与任意主题的高对比度
    fn selection_color(&self) -> Color32 {
        let fg = self.terminal.foreground;
        Color32::from_rgba_unmultiplied(fg[0], fg[1], fg[2], 90)
    }

    /// 获取选中文本的前景色 - 使用背景色确保与选择背景的对比度
    fn selection_fg_color(&self) -> Color32 {
        Self::rgb_to_color32(self.terminal.background)
    }

    /// 获取 ANSI 颜色
    fn ansi_color(&self, index: usize) -> Color32 {
        if index < 16 {
            Self::rgb_to_color32(self.terminal.ansi_colors[index])
        } else {
            Self::rgb_to_color32(self.terminal.foreground)
        }
    }
}
