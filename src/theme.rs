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

/// Keep semantic popup surfaces on the shared panel palette.
#[allow(dead_code)] // Shared with the binary popup builders.
pub(crate) fn configure_popup_style(style: &mut egui::Style) {
    egui::containers::menu::menu_style(style);
    style.visuals.window_fill = style.visuals.panel_fill;
}

/// Popup labels follow the configured terminal font without changing global UI typography.
#[allow(dead_code)] // Shared with the binary popup builders.
pub(crate) fn configure_popup_text(ui: &mut egui::Ui, size: f32) {
    let font = egui::FontId::monospace(crate::config::Config::clamp_font_size(size));
    let style = ui.style_mut();
    style.override_font_id = Some(font.clone());
    for text_font in style.text_styles.values_mut() {
        *text_font = font.clone();
    }
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
    fn popup_surface_uses_effective_panel_color_instead_of_window_color() {
        for panel in [
            egui::Color32::from_rgb(30, 31, 33),
            egui::Color32::from_rgb(245, 246, 248),
            egui::Color32::from_rgb(27, 49, 71),
        ] {
            let mut style = egui::Style::default();
            style.visuals.window_fill = egui::Color32::MAGENTA;
            style.visuals.panel_fill = panel;
            configure_popup_style(&mut style);
            assert_eq!(style.visuals.window_fill, panel);
            assert_eq!(style.visuals.window_fill.a(), 255);
        }
    }

    #[test]
    fn popup_font_uses_configured_monospace_size_without_changing_parent_style() {
        let context = egui::Context::default();
        let output = context.run_ui(Default::default(), |ui| {
            let before = ui.style().override_font_id.clone();
            ui.scope(|ui| {
                configure_popup_text(ui, 19.0);
                let expected = egui::FontId::monospace(19.0);
                assert_eq!(ui.style().override_font_id, Some(expected.clone()));
                assert!(ui
                    .style()
                    .text_styles
                    .values()
                    .all(|font| *font == expected));
            });
            assert_eq!(ui.style().override_font_id, before);
        });
        output.drop_without_applying_deltas();
    }

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
