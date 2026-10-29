//! Ember's glass presentation adapter. Shared core and terminal authority stay unchanged.
pub use crate::glass_shell_tokens as token;
use egui::{Color32, Rect as EguiRect, Stroke};
pub use jterm_core::reference_shell::*;

pub const TITLEBAR_HEIGHT: f32 = token::TOPBAR_HEIGHT;
pub const RAIL_WIDTH: f32 = token::RAIL_WIDTH;
pub const NAVIGATION_WIDTH: f32 = token::NAVIGATION_WIDTH;
pub const INSPECTOR_WIDTH: f32 = token::INSPECTOR_WIDTH;
pub const CANVAS_HEADER_HEIGHT: f32 = token::HEADER_HEIGHT;
pub const COMPOSER_HEIGHT: f32 = token::TOOLBAR_HEIGHT;
pub const COMPOSER_INSET: f32 = token::TOOLBAR_INSET_LEFT;
pub const COMPOSER_BOTTOM: f32 = 0.0;
pub const COMPOSER_RADIUS: f32 = token::PANEL_RADIUS;
pub const CARD_RADIUS: f32 = token::PANEL_RADIUS;
pub const CONTROL_RADIUS: f32 = token::CONTROL_RADIUS;
pub const CONTROL_HEIGHT: f32 = token::TOOL_BUTTON_HEIGHT;
pub const METADATA_GAP: f32 = token::TOOL_GAP;
pub const COMPOSER_CONTROL_BOTTOM: f32 = 22.0;
pub const COMPOSER_CONTROL_LEFT: f32 = 20.0;
pub const COMPOSER_CONTROL_RIGHT: f32 = 20.0;
pub const COMPOSER_PLUS_WIDTH: f32 = 57.0;
pub const COMPOSER_ATTACH_WIDTH: f32 = 91.0;
pub const COMPOSER_IMAGE_WIDTH: f32 = 89.0;
pub const COMPOSER_CODE_WIDTH: f32 = 89.0;
pub const COMPOSER_TERMINAL_WIDTH: f32 = 107.0;
pub const COMPOSER_TOOLS_WIDTH: f32 = 111.0;
pub const COMPOSER_SEND_WIDTH: f32 = 54.0;
pub const COMPOSER_MODEL_MIN_WIDTH: f32 = 180.0;
pub const COMPOSER_SECONDARY_MIN_WIDTH: f32 = 850.0;
pub const COMPOSER_MODEL_MIN_AVAILABLE: f32 = 560.0;
pub const COMPOSER_TERMINAL_MIN_AVAILABLE: f32 = 280.0;
pub const MODE_CONTROL_HEIGHT: f32 = 42.0;
pub const MODE_CONTROL_TOP: f32 = 16.0;
pub const MODE_CONTROL_MIN_WIDTH: f32 = 90.0;
pub const SELECTED_RAIL_SIZE: f32 = 49.0;
pub const PROFILE_DIAMETER: f32 = 50.0;
pub const NAVIGATION_PADDING: f32 = 17.0;
pub const NAVIGATION_ROW_HEIGHT: f32 = 38.0;
pub const NAVIGATION_ROW_GAP: f32 = 6.0;
pub const WORKSPACE_SELECTOR_TOP: f32 = 108.0;
pub const WORKSPACE_SELECTOR_HEIGHT: f32 = 36.0;
pub const NAVIGATION_ITEMS_TOP: f32 = 153.0;
pub const REFERENCE_RECENT_TOP: f32 = 398.0;
pub const REFERENCE_SPACES_TOP: f32 = 620.0;
pub const RECENT_ROW_HEIGHT: f32 = 42.0;
pub const SPACES_ROW_HEIGHT: f32 = 46.0;
pub const NAVIGATION_STATUS_HEIGHT: f32 = 60.0;
pub const NAVIGATION_STATUS_TEXT_BOTTOM: f32 = 28.0;
pub const HEADER_ACTION_SIZE: f32 = 44.0;
pub const HEADER_ACTION_GAP: f32 = 6.0;
pub const INSPECTOR_TAB_WIDTH: f32 = 80.0;
pub const INSPECTOR_INSET: f32 = 10.0;
pub const INSPECTOR_METADATA_ROW_HEIGHT: f32 = 28.0;
pub const INSPECTOR_CURRENT_HEIGHT: f32 = 58.0;
pub const INSPECTOR_PROGRESS_TARGET_Y: f32 = 238.0;
pub const INSPECTOR_METADATA_TARGET_Y: f32 = 282.0;
pub const INSPECTOR_CURRENT_TARGET_Y: f32 = 453.0;
pub const INSPECTOR_OUTPUT_HEIGHT: f32 = 170.0;

pub mod type_size {
    pub use jterm_core::reference_shell::type_size::*;
    pub const PAGE_TITLE: f32 = 22.0;
    pub const CONTROL: f32 = 14.0;
    pub const SECTION: f32 = 13.0;
}

pub fn rgba([r, g, b, a]: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Semantic palette adaptation, including background alpha only. Foregrounds stay opaque.
pub fn paint_color(value: [u8; 3]) -> Color32 {
    use jterm_core::reference_shell::color as c;
    let mapped = if value == c::TITLEBAR {
        token::TOP
    } else if value == c::RAIL {
        token::RAIL
    } else if value == c::NAVIGATION {
        token::NAVIGATION
    } else if value == c::INSPECTOR {
        token::INSPECTOR
    } else if value == c::INSPECTOR_CARD {
        token::INSPECTOR_CARD
    } else if value == c::CANVAS || value == c::CANVAS_COOL {
        token::CENTER
    } else if value == c::COMPOSER {
        token::COMPOSER
    } else if value == c::TERMINAL {
        token::TERMINAL
    } else if value == c::CONTROL_SURFACE || value == c::ACCENT_PALE {
        token::BUTTON
    } else if value == c::NAVIGATION_SELECTED || value == c::USER_BUBBLE {
        token::SELECTED_NAV
    } else if value == c::DARK_BORDER || value == c::LIGHT_BORDER || value == c::CONTROL_BORDER {
        token::BORDER
    } else if value == c::CHROME_TEXT || value == c::CANVAS_TEXT || value == c::MESSAGE_USER_TEXT {
        token::TEXT
    } else if value == c::CHROME_MUTED
        || value == c::CANVAS_MUTED
        || value == c::MESSAGE_BODY
        || value == c::MESSAGE_STEP_TEXT
    {
        token::MUTED_TEXT
    } else if value == c::DISABLED_TEXT {
        token::DISABLED_TEXT
    } else if value == c::ACCENT {
        token::ACCENT_TOP
    } else {
        return Color32::from_rgb(value[0], value[1], value[2]);
    };
    rgba(mapped)
}

pub fn paint_panel(painter: &egui::Painter, rect: EguiRect, fill: [u8; 4]) {
    if !rect.is_positive() {
        return;
    }
    painter.rect_stroke(
        rect,
        token::PANEL_RADIUS,
        Stroke::new(4.0, rgba([65, 106, 255, 22])),
        egui::StrokeKind::Inside,
    );
    painter.rect_filled(rect, token::PANEL_RADIUS, rgba(fill));
    painter.rect_stroke(
        rect,
        token::PANEL_RADIUS,
        Stroke::new(token::BORDER_WIDTH, rgba(token::BORDER)),
        egui::StrokeKind::Inside,
    );
    painter.rect_stroke(
        rect.shrink(1.0),
        token::PANEL_RADIUS - 1.0,
        Stroke::new(1.0, rgba(token::INNER_HIGHLIGHT)),
        egui::StrokeKind::Inside,
    );
}

/// The projection reserves this one band, including the gap above real input.
/// This is presentation chrome only; native cells/PTY bytes remain untouched.
pub fn footer_rows(padding: f32, line_height: f32) -> usize {
    let reserve = (CONTROL_HEIGHT + COMPOSER_CONTROL_BOTTOM + 6.0 - padding).max(CONTROL_HEIGHT);
    (reserve / line_height.max(1.0)).ceil().max(1.0) as usize
}

/// Keep55% of the native reading pane and at least three rows on short windows.
/// Beyond this cap the caller keeps the whole real input in the native viewport.
pub fn editable_input_limit(outer_height: f32, line_height: f32) -> f32 {
    let raw = (outer_height - COMPOSER_HEIGHT - token::TERMINAL_FOOTER_GAP).max(0.0);
    180.0_f32
        .min(raw * 0.45)
        .min((raw - 3.0 * line_height).max(0.0))
}

#[derive(Clone, Copy, Debug)]
pub struct DockGeometry {
    pub frame: EguiRect,
    pub controls: EguiRect,
    pub history_bottom: f32,
}

/// Both the native input clip and canvas use the same actual editor boundary.
pub fn editor_dock(outer: EguiRect, native_editor_top: f32) -> DockGeometry {
    let top = (outer.bottom() - COMPOSER_HEIGHT)
        .min(native_editor_top - 6.0)
        .max(outer.top());
    let frame = EguiRect::from_min_max(egui::pos2(outer.left(), top), outer.max);
    let controls = EguiRect::from_min_max(
        egui::pos2(
            frame.left() + COMPOSER_CONTROL_LEFT,
            frame.bottom() - COMPOSER_CONTROL_BOTTOM - CONTROL_HEIGHT,
        ),
        frame.max - egui::vec2(COMPOSER_CONTROL_RIGHT, COMPOSER_CONTROL_BOTTOM),
    );
    DockGeometry {
        frame,
        controls,
        history_bottom: (top - token::TERMINAL_FOOTER_GAP).max(outer.top()),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub titlebar: Rect,
    pub rail: Rect,
    pub navigation: Option<Rect>,
    pub center: Rect,
    pub header: Rect,
    pub inspector: Option<Rect>,
    pub composer: Rect,
    viewport_height: f32,
}
impl Layout {
    pub fn for_size(width: f32, height: f32) -> Self {
        Self::build(width, height, true)
    }
    fn build(width: f32, height: f32, inspector_open: bool) -> Self {
        let width = if width.is_finite() {
            width.max(0.0)
        } else {
            1612.0
        };
        let height = if height.is_finite() {
            height.max(0.0)
        } else {
            976.0
        };
        let (rail, nav) = if width < 960.0 {
            (56.0, 220.0)
        } else if width < 1280.0 {
            (64.0, 232.0)
        } else {
            (RAIL_WIDTH, NAVIGATION_WIDTH)
        };
        let nav = if width - token::OUTER_LEFT - rail - nav - token::LEFT_GAP - token::OUTER_RIGHT
            >= 300.0
        {
            nav
        } else {
            0.0
        };
        let bottom = if height < token::SHORT_HEIGHT {
            token::SHORT_BOTTOM_GAP
        } else {
            token::MAIN_BOTTOM_GAP
        };
        let nav_bottom = if height < token::SHORT_HEIGHT {
            token::SHORT_BOTTOM_GAP
        } else {
            token::NAVIGATION_BOTTOM_GAP
        };
        let inspector = inspector_open && width >= token::INSPECTOR_DRAWER_BREAKPOINT;
        let center_x = token::OUTER_LEFT + rail + nav + token::LEFT_GAP;
        let right = width
            - token::OUTER_RIGHT
            - if inspector {
                INSPECTOR_WIDTH + token::INSPECTOR_GAP
            } else {
                0.0
            };
        let center = Rect {
            x: center_x,
            y: token::MAIN_TOP,
            width: (right - center_x).max(0.0),
            height: (height - token::MAIN_TOP - bottom).max(0.0),
        };
        let header = Rect {
            height: if height < token::SHORT_HEIGHT {
                token::SHORT_HEADER_HEIGHT
            } else {
                CANVAS_HEADER_HEIGHT
            },
            ..center
        };
        Self {
            titlebar: Rect {
                x: 0.0,
                y: 0.0,
                width,
                height: TITLEBAR_HEIGHT.min(height),
            },
            rail: Rect {
                x: token::OUTER_LEFT,
                y: TITLEBAR_HEIGHT,
                width: rail,
                height: (height - TITLEBAR_HEIGHT - nav_bottom).max(0.0),
            },
            navigation: (nav > 0.0).then_some(Rect {
                x: token::OUTER_LEFT + rail,
                y: TITLEBAR_HEIGHT,
                width: nav,
                height: (height - TITLEBAR_HEIGHT - nav_bottom).max(0.0),
            }),
            center,
            header,
            viewport_height: height,
            inspector: inspector.then_some(Rect {
                x: right + token::INSPECTOR_GAP,
                width: INSPECTOR_WIDTH,
                ..center
            }),
            composer: Rect {
                x: center.x + token::TOOLBAR_INSET_LEFT,
                y: (center.y + center.height - COMPOSER_HEIGHT).max(center.y),
                width: (center.width - token::TOOLBAR_INSET_LEFT - token::TOOLBAR_INSET_RIGHT)
                    .max(0.0),
                height: COMPOSER_HEIGHT.min(center.height),
            },
        }
    }
    pub fn without_inspector(self) -> Self {
        Self::build(self.titlebar.width, self.viewport_height, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn glass_reference_geometry_and_hidden_inspector_budget() {
        let l = Layout::for_size(1612.0, 976.0);
        assert_eq!(
            (
                l.rail.x,
                l.navigation.unwrap().x,
                l.center.x,
                l.inspector.unwrap().x
            ),
            (6.0, 79.0, 342.0, 1254.0)
        );
        assert_eq!(
            (
                l.composer.x,
                l.composer.y,
                l.composer.width,
                l.composer.height
            ),
            (349.0, 832.0, 892.0, 98.0)
        );
        let hidden = l.without_inspector();
        assert_eq!(hidden.center.width - l.center.width, 360.0);
        assert_eq!(hidden.composer.y, l.composer.y);
        assert!(Layout::for_size(800.0, 640.0).inspector.is_none());
    }

    #[test]
    fn real_input_and_tool_band_share_one_nonoverlapping_budget() {
        for height in [759.0, 477.0, 350.0] {
            let outer =
                EguiRect::from_min_size(egui::pos2(349.0, 171.0), egui::vec2(892.0, height));
            for line in [12.0, 18.0, 23.0, 32.0, 72.0] {
                let padding = 20.0;
                let editor_bottom =
                    outer.bottom() - padding - footer_rows(padding, line) as f32 * line;
                for editor_height in [line, 4.0 * line, 180.0] {
                    let editor_top = editor_bottom - editor_height;
                    if editor_height > editable_input_limit(height, line) {
                        let raw_bottom =
                            outer.bottom() - COMPOSER_HEIGHT - token::TERMINAL_FOOTER_GAP;
                        assert!(raw_bottom > outer.top());
                        assert_eq!(
                            outer.bottom() - COMPOSER_HEIGHT + COMPOSER_HEIGHT,
                            outer.bottom()
                        );
                        continue; // Whole native viewport owns the long input; no dock clipping.
                    }
                    assert!(editor_top >= outer.top() + 6.0);
                    let dock = editor_dock(outer, editor_top);
                    assert!(dock.history_bottom <= editor_top);
                    assert!(editor_bottom + 6.0 <= dock.controls.top());
                    assert_eq!(dock.controls.height(), 54.0);
                    assert_eq!(dock.frame.bottom(), outer.bottom());
                    assert!(dock.frame.height() >= 98.0);
                }
            }
        }
    }
}
