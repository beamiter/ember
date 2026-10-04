//! Theme data (structs, builtins, custom-theme persistence) lives in
//! `jterm_core::theme`. This module re-exports it and adds the egui color
//! conversions as an extension trait.

use egui::Color32;

pub use jterm_core::theme::*;

/// Matches core's custom-theme filename envelope so the editor cannot hold
/// more than `Theme::validate_custom_theme_name` will accept.
#[allow(dead_code)] // consumed by the binary-only settings theme editor
pub(crate) const MAX_CUSTOM_THEME_NAME_BYTES: usize = 160;

#[allow(dead_code)] // consumed by the binary-only settings theme editor
pub(crate) fn bound_custom_theme_name(name: impl Into<String>) -> String {
    let mut bounded = String::new();
    for ch in name.into().chars() {
        if ch.is_control() || matches!(ch, '/' | '\\') {
            continue;
        }
        let ch = if jterm_core::review_input::is_visual_spoofing_character(ch) {
            '\u{fffd}'
        } else {
            ch
        };
        if bounded.len().saturating_add(ch.len_utf8()) > MAX_CUSTOM_THEME_NAME_BYTES {
            break;
        }
        bounded.push(ch);
    }
    bounded
}

/// Persist-time check: the editor may show U+FFFD after ingest, but a
/// replacement character must not become a theme filename.
#[allow(dead_code)] // consumed by the binary-only settings theme editor
pub(crate) fn validate_saved_custom_theme_name(name: &str) -> Result<(), String> {
    if name.contains('\u{fffd}') {
        return Err(
            "Name cannot contain path separators, controls, or invisible formatting characters"
                .to_string(),
        );
    }
    Theme::validate_custom_theme_name(name)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_theme_name_draft_drops_path_syntax_and_stays_inside_the_filename_envelope() {
        assert_eq!(bound_custom_theme_name("dusk\n\u{1b}/night\\"), "dusknight");
        let spoofed = bound_custom_theme_name("ok\u{202e}");
        assert_eq!(spoofed, "ok\u{fffd}");
        assert!(!spoofed.contains('\u{202e}'));
        let filled =
            bound_custom_theme_name(format!("{}y", "x".repeat(MAX_CUSTOM_THEME_NAME_BYTES)));
        assert_eq!(filled.len(), MAX_CUSTOM_THEME_NAME_BYTES);
        assert!(!filled.contains('y'));
        assert!(Theme::validate_custom_theme_name(&filled).is_ok());
        assert!(validate_saved_custom_theme_name(&filled).is_ok());
        assert!(validate_saved_custom_theme_name("ok\u{fffd}").is_err());
        assert!(validate_saved_custom_theme_name("ok\u{202e}").is_err());
    }
}
