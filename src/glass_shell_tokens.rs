//! Ember/Frost compact glass profile revision3. Native adapters consume straight RGBA.
//! Original shared core profile and Anvil/Forge are unchanged.
#![allow(dead_code)]

pub const TOPBAR_HEIGHT: f32 = 40.0;
pub const MAIN_TOP: f32 = 44.0;
pub const OUTER_LEFT: f32 = 4.0;
pub const OUTER_RIGHT: f32 = 4.0;
pub const RAIL_WIDTH: f32 = 44.0;
pub const NAVIGATION_WIDTH: f32 = 216.0;
pub const LEFT_GAP: f32 = 6.0;
pub const INSPECTOR_WIDTH: f32 = 288.0;
pub const INSPECTOR_GAP: f32 = 6.0;
pub const MAIN_BOTTOM_GAP: f32 = 6.0;
pub const NAVIGATION_BOTTOM_GAP: f32 = 6.0;
pub const HEADER_HEIGHT: f32 = 36.0;
pub const TERMINAL_INSET_LEFT: f32 = 8.0;
pub const TERMINAL_INSET_RIGHT: f32 = 8.0;
pub const TERMINAL_TOP_GAP: f32 = 6.0;
pub const TERMINAL_FOOTER_GAP: f32 = 6.0;
pub const TOOLBAR_INSET_LEFT: f32 = 0.0;
pub const TOOLBAR_INSET_RIGHT: f32 = 0.0;
pub const TOOLBAR_HEIGHT: f32 = 42.0;
pub const TOOL_BUTTON_HEIGHT: f32 = 32.0;
pub const TOOL_GAP: f32 = 6.0;
pub const PANEL_RADIUS: f32 = 8.0;
pub const CONTROL_RADIUS: f32 = 6.0;
pub const BORDER_WIDTH: f32 = 1.0;

pub const INSPECTOR_DRAWER_BREAKPOINT: f32 = 1180.0;
pub const SHORT_HEIGHT: f32 = 760.0;
pub const SHORT_HEADER_HEIGHT: f32 = 36.0;
pub const SHORT_BOTTOM_GAP: f32 = 6.0;

pub const TOP: [u8; 4] = [27, 45, 91, 190];
pub const RAIL: [u8; 4] = [10, 22, 46, 218];
pub const NAVIGATION: [u8; 4] = [23, 40, 75, 214];
pub const CENTER: [u8; 4] = [36, 55, 102, 145];
pub const INSPECTOR: [u8; 4] = [22, 39, 76, 214];
pub const INSPECTOR_CARD: [u8; 4] = [42, 59, 96, 155];
pub const TERMINAL: [u8; 4] = [4, 10, 21, 214];
pub const COMPOSER: [u8; 4] = [75, 94, 145, 174];
pub const BUTTON: [u8; 4] = [96, 120, 180, 67];
pub const BUTTON_HOVER: [u8; 4] = [104, 136, 213, 108];
pub const BORDER: [u8; 4] = [134, 168, 247, 48];
pub const INNER_HIGHLIGHT: [u8; 4] = [205, 222, 255, 0];
pub const TEXT: [u8; 4] = [239, 245, 255, 255];
pub const MUTED_TEXT: [u8; 4] = [184, 204, 238, 255];
pub const DISABLED_TEXT: [u8; 4] = [153, 177, 217, 255];
pub const ACCENT_TOP: [u8; 4] = [62, 108, 255, 255];
pub const ACCENT_BOTTOM: [u8; 4] = [36, 76, 235, 255];
pub const SELECTED_NAV: [u8; 4] = [57, 91, 213, 166];
