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

/// Floating controls belong to the glass workspace, independently of the
/// terminal's ANSI/custom theme. Keep their surface opaque so an open menu
/// remains readable above terminal output and translucent navigation panels.
#[allow(dead_code)] // Also consumed by binary-only workspace builders.
pub(crate) fn workspace_visuals() -> egui::Visuals {
    use crate::glass_shell::{rgba, token};

    let mut visuals = egui::Visuals::dark();
    let surface = rgba(token::INSPECTOR).to_opaque();
    let text = rgba(token::TEXT);
    let border = egui::Stroke::new(token::BORDER_WIDTH, rgba(token::BORDER));
    visuals.window_fill = surface;
    visuals.panel_fill = surface;
    visuals.window_stroke = border;
    visuals.window_corner_radius = egui::CornerRadius::same(token::PANEL_RADIUS as u8);
    visuals.menu_corner_radius = egui::CornerRadius::same(token::CONTROL_RADIUS as u8);
    visuals.override_text_color = Some(text);
    visuals.weak_text_color = Some(rgba(token::MUTED_TEXT));
    visuals.extreme_bg_color = rgba(token::TERMINAL).to_opaque();
    visuals.faint_bg_color = rgba(token::INSPECTOR_CARD);
    visuals.code_bg_color = rgba(token::RAIL).to_opaque();
    visuals.selection.bg_fill = rgba(token::SELECTED_NAV);
    visuals.selection.stroke = egui::Stroke::new(1.0, text);
    visuals.hyperlink_color = rgba(token::MUTED_TEXT);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.fg_stroke = egui::Stroke::new(1.0, text);
        widget.bg_stroke = border;
        widget.corner_radius = egui::CornerRadius::same(token::CONTROL_RADIUS as u8);
    }
    visuals.widgets.noninteractive.bg_fill = surface;
    visuals.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_fill = rgba(token::BUTTON);
    visuals.widgets.inactive.weak_bg_fill = rgba(token::BUTTON);
    visuals.widgets.hovered.bg_fill = rgba(token::BUTTON_HOVER);
    visuals.widgets.hovered.weak_bg_fill = rgba(token::BUTTON_HOVER);
    visuals.widgets.active.bg_fill = rgba(token::SELECTED_NAV);
    visuals.widgets.active.weak_bg_fill = rgba(token::SELECTED_NAV);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, rgba(token::ACCENT_TOP));
    visuals.widgets.open = visuals.widgets.active;
    visuals
}

/// Shared frame for settings, pickers and review panels above the workspace.
#[allow(dead_code)] // Several consumers are binary-only modules.
pub(crate) fn workspace_frame(inner_margin: i8) -> egui::Frame {
    let visuals = workspace_visuals();
    egui::Frame {
        fill: visuals.window_fill,
        stroke: visuals.window_stroke,
        corner_radius: visuals.window_corner_radius,
        shadow: visuals.window_shadow,
        inner_margin: egui::Margin::same(inner_margin),
        ..Default::default()
    }
}

/// Reassert the workspace palette at every popup boundary: chrome deliberately
/// uses transparent panels and can override the global egui style locally.
#[allow(dead_code)] // Shared with the binary popup builders.
pub(crate) fn configure_popup_style(style: &mut egui::Style) {
    style.visuals = workspace_visuals();
    egui::containers::menu::menu_style(style);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
}

/// Apply one font group in egui logical points, scoped to the supplied style.
/// Physical pixel scaling remains egui's responsibility; do not multiply by DPI.
#[allow(dead_code)]
pub(crate) fn configure_workspace_text(style: &mut egui::Style, size: f32) {
    let size = crate::config::Config::clamp_font_size(size);
    for font in style.text_styles.values_mut() {
        font.size = size;
    }
    if let Some(font) = style.override_font_id.as_mut() {
        font.size = size;
    }
    // Explicit-height sidebar rows must accommodate the same labels as buttons.
    style.spacing.interact_size.y = workspace_control_height(size);
}

#[allow(dead_code)]
pub(crate) fn workspace_control_height(size: f32) -> f32 {
    crate::config::Config::clamp_font_size(size) * 1.5 + 8.0
}

#[allow(dead_code)]
pub(crate) fn workspace_font_size(ui: &egui::Ui) -> f32 {
    egui::TextStyle::Body.resolve(ui.style()).size
}

/// Popup labels follow their owning font group without changing global UI typography.
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
    fn workspace_font_and_control_height_follow_zoom_in_both_directions() {
        let mut style = egui::Style::default();
        for size in [8.0, 14.0, 32.0, 72.0, 14.0, 8.0] {
            configure_workspace_text(&mut style, size);
            assert!(style.text_styles.values().all(|font| font.size == size));
            assert_eq!(
                style.spacing.interact_size.y,
                workspace_control_height(size)
            );
        }
    }

    #[test]
    fn workspace_labels_fit_scaled_rows_at_native_and_high_dpi() {
        for dpi in [1.0, 2.0] {
            let context = egui::Context::default();
            context.set_pixels_per_point(dpi);
            for size in [8.0, 14.0, 32.0, 72.0] {
                let output = context.run_ui(Default::default(), |ui| {
                    configure_workspace_text(ui.style_mut(), size);
                    for text_style in [
                        egui::TextStyle::Body,
                        egui::TextStyle::Button,
                        egui::TextStyle::Small,
                        egui::TextStyle::Monospace,
                    ] {
                        let font = text_style.resolve(ui.style());
                        assert_eq!(font.size, size);
                        let galley = ui.painter().layout_no_wrap(
                            "Sessions / project".into(),
                            font,
                            egui::Color32::WHITE,
                        );
                        assert!(galley.size().y <= ui.spacing().interact_size.y);
                    }
                    assert_eq!(workspace_font_size(ui), size);
                    let plus = ui.painter().layout_no_wrap(
                        "+".into(),
                        egui::FontId::monospace(size.min(18.0)),
                        egui::Color32::WHITE,
                    );
                    assert!(
                        plus.size().x <= 25.0,
                        "new-tab glyph must fit its exact hit width"
                    );
                });
                output.drop_without_applying_deltas();
            }
        }
    }

    #[test]
    fn popup_and_nested_menu_keep_glass_surface_above_transparent_chrome() {
        for parent_visuals in [egui::Visuals::light(), egui::Visuals::dark()] {
            let context = egui::Context::default();
            context.set_visuals(parent_visuals);
            context.all_styles_mut(|style| {
                style.visuals.panel_fill = Color32::TRANSPARENT;
                style.visuals.window_fill = Color32::MAGENTA;
                style.override_font_id = Some(egui::FontId::proportional(19.0));
            });
            let mut submenu_rect = egui::Rect::NOTHING;
            let mut rendered = false;
            for pass in 0..4 {
                let mut input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640.0, 480.0),
                    )),
                    ..Default::default()
                };
                if pass > 0 {
                    input
                        .events
                        .push(egui::Event::PointerMoved(submenu_rect.center()));
                }
                let output = context.run_ui(input, |ui| {
                    let button = ui.button("Actions");
                    egui::Popup::menu(&button)
                        .open(true)
                        .style(configure_popup_style)
                        .show(|ui| {
                            let expected = workspace_visuals();
                            assert_eq!(ui.visuals().window_fill, expected.window_fill);
                            assert_eq!(ui.visuals().window_fill.a(), 255);
                            assert_eq!(ui.visuals().window_stroke, expected.window_stroke);
                            assert!(ui.visuals().disabled_alpha < 1.0);
                            assert_eq!(
                                ui.style().override_font_id,
                                Some(egui::FontId::proportional(19.0))
                            );
                            ui.add_enabled(false, egui::Button::new("Unavailable"));
                            let parent_open_visuals = ui.visuals().widgets.open;
                            let advanced = ui.menu_button("Advanced", |ui| {
                                // Hover opens an actual SubMenuButton, which
                                // must carry the parent menu's style modifier.
                                assert_eq!(ui.visuals().window_fill, expected.window_fill);
                                assert_eq!(ui.visuals().widgets.open, parent_open_visuals);
                                let _ = ui.selectable_label(true, "Selected action");
                                rendered = true;
                            });
                            submenu_rect = advanced.response.rect;
                        });
                    assert_eq!(ui.visuals().panel_fill, Color32::TRANSPARENT);
                    assert_eq!(ui.visuals().window_fill, Color32::MAGENTA);
                });
                output.drop_without_applying_deltas();
            }
            assert!(rendered, "pointer hover must open the real submenu");
        }
    }

    #[test]
    fn combobox_opened_in_transparent_chrome_uses_workspace_popup_palette() {
        let context = egui::Context::default();
        let mut button_rect = egui::Rect::NOTHING;
        let mut rendered = false;
        for pass in 0..4 {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 480.0),
                )),
                ..Default::default()
            };
            if pass == 1 || pass == 2 {
                input
                    .events
                    .push(egui::Event::PointerMoved(button_rect.center()));
                input.events.push(egui::Event::PointerButton {
                    pos: button_rect.center(),
                    button: egui::PointerButton::Primary,
                    pressed: pass == 1,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            let output = context.run_ui(input, |ui| {
                *ui.visuals_mut() = egui::Visuals::light();
                ui.visuals_mut().panel_fill = Color32::TRANSPARENT;
                let response = egui::ComboBox::from_id_salt("glass-theme-picker")
                    .popup_style(configure_popup_style.into())
                    .selected_text("Theme")
                    .show_ui(ui, |ui| {
                        let expected = workspace_visuals();
                        assert_eq!(ui.visuals().window_fill, expected.window_fill);
                        assert_eq!(ui.visuals().text_color(), expected.text_color());
                        assert_eq!(ui.visuals().selection, expected.selection);
                        let _ = ui.selectable_label(true, "Current theme");
                        rendered = true;
                    });
                button_rect = response.response.rect;
                assert_eq!(ui.visuals().panel_fill, Color32::TRANSPARENT);
            });
            output.drop_without_applying_deltas();
        }
        assert!(
            rendered,
            "the pointer interaction must open the real ComboBox"
        );
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
