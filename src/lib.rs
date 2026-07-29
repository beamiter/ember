// Library export for benchmarks and testing

pub(crate) use jterm_core::atomic_file;
pub use jterm_core::char_width; // P5：字符宽度缓存（已抽取到 jterm_core）
pub mod clipboard;
pub mod color;
pub mod command_palette;
pub mod config;
pub mod debug;
pub mod gpu;
pub mod keybindings;
pub mod kitty_graphics;
pub mod layout;
pub mod link;
pub mod pane_header;
pub mod pty;
pub mod search;
pub mod search_replace;
pub mod search_replace_panel;
pub mod session;
pub mod session_manager;
pub mod session_persistence;
pub mod shell;
pub mod sidebar;
pub mod tab_manager;
pub mod terminal;
pub mod theme;
pub mod ui;
pub mod windows_compat;
