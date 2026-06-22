// Application state management

use crate::clipboard::ClipboardManager;
use crate::command_palette;
use crate::config;
use crate::config_panel;
use crate::debug_panel;
use crate::help;
use crate::keybindings;
use crate::layout;
use crate::link;
use crate::search;
use crate::search_replace_panel;
use crate::session_manager::SessionManager;
use crate::sidebar;
use crate::ui::TerminalRenderer;
use parking_lot::Mutex as ParkingMutex;
use std::sync::Arc;

/// Text from the clipboard waiting for the user to confirm before being
/// written to the active session. Created when a paste contains a newline or
/// exceeds [`PASTE_CONFIRM_THRESHOLD_BYTES`]: pasting `rm -rf …` followed by
/// a newline would otherwise execute immediately.
pub struct PendingPasteConfirm {
    pub text: String,
    /// Session that initiated the paste; we only deliver if it is still the
    /// active session at confirm time (otherwise the user has switched tabs
    /// and probably no longer intends the paste).
    pub session_idx: usize,
    pub bracketed: bool,
}

/// Show the confirmation dialog when the paste contains a newline (the most
/// common foot-gun: pasting a multi-line block that runs commands without
/// review) or when the paste is large enough that the user probably wants
/// to verify what's about to enter the shell.
pub const PASTE_CONFIRM_THRESHOLD_BYTES: usize = 4 * 1024;

/// Main application state
pub struct TerminalApp {
    pub session_manager: SessionManager,
    pub renderer: TerminalRenderer,
    pub input_queue: Arc<ParkingMutex<Vec<u8>>>,
    pub clipboard: Option<ClipboardManager>,
    pub cols: usize,
    pub rows: usize,
    pub next_cursor_blink_time: std::time::Instant,
    pub cursor_visible: bool,
    pub last_activity_time: std::time::Instant,
    pub status_message: String,
    pub last_window_title: String,

    // Tab UI state
    pub hovered_tab_index: Option<usize>,
    pub dragging_tab: Option<usize>,
    pub drag_start_pos: Option<f32>,
    pub current_mouse_x: f32,
    pub tab_scroll_offset: f32,

    // Search state
    pub search_state: search::SearchState,

    // File-tree sidebar
    pub sidebar: sidebar::Sidebar,

    // Find & Replace (operates on selection)
    pub search_replace_panel: search_replace_panel::SearchReplacePanel,

    // Link detection
    pub link_detector: link::LinkDetector,
    pub hovered_link: Option<link::Link>,
    pub cached_links: Vec<link::Link>,
    pub cached_links_grid_version: u64,
    pub cached_links_scroll_offset: usize,
    /// 缓存所属的活跃会话索引;切换 pane/会话时需失效,
    /// 否则不同终端碰巧相同的 grid_version 会复用上一个会话的过期链接。
    pub cached_links_session_idx: usize,

    // Keybindings
    pub keybindings: keybindings::KeyBindings,

    // Command palette
    pub command_palette: command_palette::CommandPalette,

    // Force resize flag for new sessions
    pub force_resize_session: bool,

    // Theme system
    pub current_theme: crate::theme::Theme,

    // Layout system (split panes)
    pub layout_manager: layout::LayoutManager,

    // Pane renderers (one per pane)
    pub pane_renderers: Vec<TerminalRenderer>,

    // Divider drag state
    pub dragging_divider: bool,

    // Help panel
    pub help_panel: help::HelpPanel,

    // Config panel
    pub config_panel: config_panel::ConfigPanel,

    // Debug overlay panel
    pub debug_panel: debug_panel::DebugPanel,

    // Config system
    pub config: config::Config,
    pub config_save_pending: bool,
    pub config_save_deadline: std::time::Instant,

    // Session persistence
    pub session_save_pending: bool,
    pub session_save_deadline: std::time::Instant,

    // Lock file to detect running instances
    pub _lock_file: Option<std::fs::File>,

    // 每帧字节限制溢出的缓冲区，下一帧继续处理
    pub pending_output: Vec<u8>,

    // 鼠标报告模式下的滚轮累积器
    pub mouse_scroll_accumulator: f32,

    // Ctrl+滚轮字体缩放累积器
    pub font_size_accumulator: f32,

    // 上一帧是否有Ctrl+滚轮事件
    pub had_ctrl_scroll_last_frame: bool,

    // 每帧事件缓存，避免多次克隆
    pub frame_events: Vec<egui::Event>,

    // 键盘输入缓冲区，复用以减少内存分配
    pub keyboard_input_buffer: Vec<u8>,

    // 自适应帧预算：根据帧时间动态调整每帧处理的字节数
    pub adaptive_frame_budget: usize,

    // Config hot-reload
    pub config_last_mtime: Option<std::time::SystemTime>,
    pub config_last_check: std::time::Instant,

    // Smooth scrolling
    pub smooth_scroll_velocity: f32,
    pub smooth_scroll_pixel_offset: f32,

    /// Holds a paste payload waiting for user confirmation. Populated by the
    /// paste handlers when [`should_confirm_paste`] returns true; cleared by
    /// the modal renderer on confirm/cancel.
    pub pending_paste_confirm: Option<PendingPasteConfirm>,

    /// 粘贴确认对话框里"不再询问"复选框的临时状态(跨帧保留,直到对话框关闭)。
    pub paste_dont_ask_again: bool,
}
