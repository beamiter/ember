// Library export for benchmarks and testing

pub use jterm_core::char_width; // P5：字符宽度缓存（已抽取到 jterm_core）
pub mod agent;
mod agent_cli;
pub mod block_mode;
pub mod bottom_bar;
pub mod clipboard;
pub mod color;
pub mod command_palette;
pub mod config;
pub mod debug;
mod fold_preview;
#[cfg(test)]
mod font_zoom;
pub mod glass_shell;
pub mod glass_shell_tokens;
pub mod gpu;
pub mod keybindings;
pub mod kitty_graphics;
pub mod layout;
pub mod link;
pub mod organism;
pub mod output_batch;
pub mod pane_header;
pub mod persistence_file;
pub mod pty;
pub mod remote_fs;
pub mod remote_picker;
pub mod review_text;
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
pub mod transcript_canvas;
pub mod ui;
mod vim9_buffer;
pub mod watch_adapter;
pub mod windows_compat;
pub mod workflows;
