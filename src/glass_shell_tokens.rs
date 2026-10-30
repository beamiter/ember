//! Ember/Frost compact glass profile revision3. Native adapters consume straight RGBA.
//! Original shared core profile and Anvil/Forge are unchanged.
#![allow(dead_code)]

pub const TOPBAR_HEIGHT: f32 = 42.0;
pub const MAIN_TOP: f32 = 42.0;
pub const OUTER_LEFT: f32 = 0.0;
pub const OUTER_RIGHT: f32 = 0.0;
pub const RAIL_WIDTH: f32 = 48.0;
pub const NAVIGATION_WIDTH: f32 = 216.0;
pub const LEFT_GAP: f32 = 2.0;
pub const INSPECTOR_WIDTH: f32 = 288.0;
pub const INSPECTOR_GAP: f32 = 2.0;
pub const MAIN_BOTTOM_GAP: f32 = 10.0;
pub const NAVIGATION_BOTTOM_GAP: f32 = 0.0;
pub const HEADER_HEIGHT: f32 = 49.0;
pub const TERMINAL_INSET_LEFT: f32 = 28.0;
pub const TERMINAL_INSET_RIGHT: f32 = 28.0;
pub const TERMINAL_TOP_GAP: f32 = 10.0;
pub const TERMINAL_FOOTER_GAP: f32 = 8.0;
pub const TOOLBAR_INSET_LEFT: f32 = 28.0;
pub const TOOLBAR_INSET_RIGHT: f32 = 28.0;
pub const TOOLBAR_HEIGHT: f32 = 36.0;
pub const TOOL_BUTTON_HEIGHT: f32 = 26.0;
pub const TOOL_GAP: f32 = 4.0;
pub const PANEL_RADIUS: f32 = 8.0;
pub const CONTROL_RADIUS: f32 = 6.0;
pub const BORDER_WIDTH: f32 = 1.0;

pub const INSPECTOR_DRAWER_BREAKPOINT: f32 = 1180.0;
pub const SHORT_HEIGHT: f32 = 760.0;
pub const SHORT_HEADER_HEIGHT: f32 = 49.0;
pub const SHORT_BOTTOM_GAP: f32 = 10.0;

pub const TOP: [u8; 4] = [15, 23, 38, 235];
pub const RAIL: [u8; 4] = [13, 21, 34, 235];
pub const NAVIGATION: [u8; 4] = [22, 33, 51, 230];
pub const CENTER: [u8; 4] = [9, 19, 35, 225];
pub const INSPECTOR: [u8; 4] = [15, 24, 37, 230];
pub const INSPECTOR_CARD: [u8; 4] = [28, 41, 58, 230];
pub const TERMINAL: [u8; 4] = [4, 10, 21, 214];
pub const COMPOSER: [u8; 4] = [23, 35, 56, 240];
pub const BUTTON: [u8; 4] = [25, 40, 60, 210];
pub const BUTTON_HOVER: [u8; 4] = [38, 53, 84, 230];
pub const BORDER: [u8; 4] = [52, 72, 100, 150];
pub const INNER_HIGHLIGHT: [u8; 4] = [205, 222, 255, 0];
pub const TEXT: [u8; 4] = [213, 222, 236, 255];
pub const MUTED_TEXT: [u8; 4] = [145, 164, 191, 255];
pub const DISABLED_TEXT: [u8; 4] = [99, 125, 156, 255];
pub const ACCENT_TOP: [u8; 4] = [62, 108, 255, 255];
pub const ACCENT_BOTTOM: [u8; 4] = [36, 76, 235, 255];
pub const SELECTED_NAV: [u8; 4] = [40, 61, 111, 220];
