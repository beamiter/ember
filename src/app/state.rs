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
    /// 状态消息过期时间。None 表示没有待显示的提示;Some 表示在该时刻之前
    /// 应作为 toast 显示。过期后由渲染端读取并清空 `status_message`,避免
    /// 早先"每帧清空"的写法把瞬时反馈吞掉。
    pub status_expires_at: Option<std::time::Instant>,
    pub last_window_title: String,

    // Tab UI state
    pub hovered_tab_index: Option<usize>,
    pub dragging_tab: Option<usize>,
    pub drag_start_pos: Option<f32>,
    pub current_mouse_x: f32,
    pub tab_scroll_offset: f32,
    /// 双击 tab 进入重命名:(会话索引, 编辑中的名称缓冲)。提交时写入
    /// session.metadata.name 并触发持久化;Esc 放弃。重排/关闭等结构性
    /// 操作会顺手清空,避免索引漂移后继续提交到错误的会话。
    pub renaming_tab: Option<(usize, String)>,

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

impl TerminalApp {
    /// 设置短暂的状态提示(默认 2.5 秒后自动隐藏)。多次调用会重置计时器。
    /// 主线程的所有反馈消息(分屏、跳转命令、复制等)都应走这里,避免再
    /// 出现"写了 status_message 却没人显示"的悄悄丢弃。
    pub fn set_status<S: Into<String>>(&mut self, msg: S) {
        self.set_status_for(msg, std::time::Duration::from_millis(2500));
    }

    pub fn set_status_for<S: Into<String>>(&mut self, msg: S, dur: std::time::Duration) {
        self.status_message = msg.into();
        self.status_expires_at = Some(std::time::Instant::now() + dur);
    }

    /// 取当前应当显示的提示(若仍在有效期);到期则清空。
    pub fn current_status_for_display(&mut self) -> Option<&str> {
        if let Some(deadline) = self.status_expires_at {
            if std::time::Instant::now() >= deadline {
                self.status_message.clear();
                self.status_expires_at = None;
            }
        }
        if self.status_message.is_empty() {
            None
        } else {
            Some(self.status_message.as_str())
        }
    }
}
