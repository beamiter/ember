// Application state management

use super::commands::CommandSidebarState;
use super::events::PasteKeyState;
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
use std::sync::{atomic::AtomicBool, Arc};

/// Text from the clipboard waiting for the user to confirm before being
/// written to the active session. Created when a paste contains a newline,
/// exceeds [`PASTE_CONFIRM_THRESHOLD_BYTES`], carries an embedded frame marker,
/// or contains visual-spoof Unicode that requires an explicit one-shot review.
pub struct PendingPasteConfirm {
    /// A dialog created during this OS input batch must not consume the same
    /// batch's trailing Enter/Escape/click as approval. Its first render only
    /// presents the preview and arms decisions for the following frame.
    pub decision_armed: bool,
    /// The payload as the child will receive it: line endings folded and any
    /// embedded paste marker already removed, so the preview cannot show
    /// something other than what gets written. Re-encoding it on accept is a
    /// no-op pass, which is what makes the backpressure retry exact.
    pub text: String,
    /// Stable identity of the session that initiated the paste. Indices can
    /// be reused after a tab exits, which could otherwise paste into a
    /// completely different PTY after confirmation.
    pub session_id: String,
    /// What `jterm_core::pty_input::classify_paste` found in the *clipboard*,
    /// captured once when this became pending. Kept for
    /// `had_embedded_paste_marker`: that flag is a bracketed-paste injection
    /// attempt, and the dialog names it even though the encoder already removed
    /// the marker.
    pub risk: jterm_core::pty_input::PasteRisk,
    /// Current core pins cannot classify Unicode display spoofing. A true value
    /// forces this dialog even when routine paste confirmation is disabled,
    /// and makes the preview render every hidden scalar as an ASCII escape.
    pub had_visual_spoofing: bool,
    /// Trusted UI commands (currently the Files sidebar's quoted `cd`) keep
    /// the final Enter outside bracketed-paste mode so Readline executes them
    /// after confirmation instead of merely inserting them.
    pub submit_after_paste: bool,
}

/// Which tab list started the in-flight tab drag. The horizontal top bar and
/// the vertical sidebar list can be on screen at the same time and share the
/// drag fields, so each one ignores a drag the other owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabDragOrigin {
    TopBar,
    Sidebar,
}

/// A pane header press that may become a rearrange drag.
///
/// The source is tracked by stable session ID rather than index: a background
/// session can exit mid-drag, which shifts every later index and would
/// otherwise make the release swap two unrelated panes.
pub struct PaneDrag {
    pub session_id: String,
    /// Where the press landed, for the movement threshold.
    pub origin: egui::Pos2,
    /// True once the pointer moved far enough to mean "rearrange" rather than
    /// "focus this pane".
    pub active: bool,
}

/// Show the confirmation dialog when the paste contains a newline (the most
/// common foot-gun: pasting a multi-line block that runs commands without
/// review) or when the paste is large enough that the user probably wants
/// to verify what's about to enter the shell.
pub const PASTE_CONFIRM_THRESHOLD_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingMouseControlKind {
    Press,
    Release,
}

#[derive(Clone, Debug)]
pub struct PendingMouseControl {
    pub kind: PendingMouseControlKind,
    pub bytes: Vec<u8>,
}

pub struct TerminalMouseCapture {
    pub session_id: String,
    /// False for a local selection (normal mode or Shift bypass). Locking this
    /// decision at press time prevents an orphan motion/release if modifiers
    /// change mid-drag.
    pub reported_to_app: bool,
    pub button: u8,
    /// Keep the original terminal and writer route alive across focus/tab
    /// changes so release can never land in a different PTY.
    pub terminal: Arc<ParkingMutex<crate::terminal::TerminalState>>,
    pub write_tx: crate::shell::ShellWriteSender,
    /// Stable protocol FIFO paired with `write_tx`. Tab indices can change
    /// during a drag, but a paste response for this PTY must remain ahead of
    /// its mouse reports.
    pub protocol_responses: crate::session_manager::ProtocolResponseSender,
    /// Press-time pane geometry plus the most recent cell. Some window
    /// systems deliver an outside-window release without a pointer position.
    pub content_rect: egui::Rect,
    pub char_width: f32,
    pub line_height: f32,
    pub last_col: usize,
    pub last_row: usize,
    /// Press/release are state transitions and cannot be dropped like motion
    /// or wheel reports. Keep them ordered across bounded-writer backpressure.
    pub pending_controls: std::collections::VecDeque<PendingMouseControl>,
    pub press_accepted: bool,
    pub release_observed: bool,
    /// A local text selection was cancelled because its pane/tab route was
    /// replaced before button-up. Retain the capture only to absorb that
    /// release without copying a different session's selection to PRIMARY.
    pub local_selection_cancelled: bool,
}

/// Main application state
pub struct TerminalApp {
    pub session_manager: SessionManager,
    pub renderer: TerminalRenderer,
    pub clipboard: Option<ClipboardManager>,
    /// Serializes OSC 5522 clipboard reads. Clipboard owners are external
    /// processes, so allowing the PTY to spawn overlapping reads would turn a
    /// harmless MIME-list query into an unbounded thread/process DoS.
    pub clipboard_request_in_flight: Arc<AtomicBool>,
    /// Holds user input for the originating session until an asynchronous OSC
    /// 5522 paste notification has entered that session's protocol-response
    /// FIFO. Stable IDs keep tab switches and background pumping correctly
    /// routed; mouse-edge barriers remain independent.
    pub(crate) osc_paste_input_barriers: crate::session_manager::SessionInputBarriers,
    /// Bounded background writer for OSC 52. PTY output is untrusted and must
    /// never synchronously wait for external clipboard-owner processes on the
    /// UI thread.
    pub osc52_clipboard_write_tx: Option<crossbeam_channel::Sender<String>>,
    pub osc52_write_window_started: std::time::Instant,
    pub osc52_writes_in_window: usize,
    /// OSC 52 clipboard reads are opt-in but still originate from untrusted
    /// PTY output. These counters bound accepted queries independently of
    /// frame rate; `clipboard_request_in_flight` serializes the actual helper.
    pub osc52_read_window_started: std::time::Instant,
    pub osc52_reads_in_window: usize,
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
    /// Presentation cache for the currently dragged tab. Structural drop
    /// operations never trust this shifting index; `dragging_tab_session_id`
    /// is the authoritative payload and is resolved again at drop time.
    pub dragging_tab: Option<usize>,
    /// Stable identity carried by a tab drag. For a split tab this is its
    /// focused session; an ordinary tab has exactly this one session.
    pub dragging_tab_session_id: Option<String>,
    /// Full pointer origin used for the cross-axis drag threshold. The legacy
    /// scalar origin below is retained for the top-bar slide animation.
    pub tab_drag_pointer_origin: Option<egui::Pos2>,
    /// Stable identity and dwell start for delayed drag-hover tab activation.
    /// This lets an active source tab expose another page before it is dropped
    /// into that page, without turning a quick reorder gesture into a switch.
    pub tab_drag_hover_session_id: Option<String>,
    pub tab_drag_hover_started_at: Option<std::time::Instant>,
    /// Stable identity of the tab that was active when a tab drag began.
    /// Hover activation is only a preview: cancellation, an invalid drop, or
    /// loss of window focus restores this tab even if indices moved meanwhile.
    pub tab_drag_origin_active_session_id: Option<String>,
    /// 拖拽起点在拖拽方向上的坐标:顶部栏取 x,侧边栏取 y。两处列表现在可以
    /// 同帧显示,所以必须配合 [`Self::tab_drag_origin`] 才有意义。
    pub drag_start_pos: Option<f32>,
    /// 哪个列表拥有当前的 tab 拖拽。Top 模式下顶部 tab 栏与侧边栏 Sessions
    /// 列表并存,若不区分来源,一次横向拖拽会被纵向列表按 y 轴误判成重排。
    pub tab_drag_origin: Option<TabDragOrigin>,
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

    // Semantic command timeline sidebar
    pub command_sidebar: CommandSidebarState,

    /// Block mode 当前选中的命令块范围。范围持有 session、固定 anchor、
    /// active edge 与 terminal-order ids；record 被淘汰或 session 关闭时一律
    /// 按“无选中”处理（绝不 panic）。真实键盘输入送往 PTY 时清空。
    pub block_selection: Option<crate::block_mode::BlockSelection>,

    /// `block:search` 跨块搜索选择器（palette 式浮层）。打开期间拥有键盘
    /// 输入,终端事件被拦截,因此不会顺带清掉 block_selection。
    pub block_search: crate::block_search::BlockSearchState,

    // Find & Replace (operates on selection)
    pub search_replace_panel: search_replace_panel::SearchReplacePanel,

    // Link detection
    pub link_detector: link::LinkDetector,
    pub hovered_link: Option<link::Link>,
    pub cached_links: Vec<link::Link>,
    pub cached_links_grid_version: u64,
    pub cached_links_scroll_offset: usize,
    /// Cache identity must follow the terminal object, not a reusable session
    /// vector index. A closed tab can be replaced at the same index with the
    /// same grid version and scroll offset.
    pub cached_links_terminal_ptr: usize,

    // Keybindings
    pub keybindings: keybindings::KeyBindings,

    // Command palette
    pub command_palette: command_palette::CommandPalette,

    // Force resize flag for new sessions
    pub force_resize_session: bool,

    // Theme system
    pub current_theme: crate::theme::Theme,

    /// Split panes, owned per tab. Panes live and die with their tab; the tab
    /// bar renders one entry per tab, never one per pane.
    pub tabs: crate::tab_manager::TabManager,

    // Pane renderers (one per pane)
    pub pane_renderers: Vec<TerminalRenderer>,

    // Divider drag state
    pub dragging_divider: Option<layout::DividerId>,

    /// Cached per-pane header status (title / cwd / running command).
    pub pane_status_cache: crate::pane_header::PaneStatusCache,

    /// Cached per-session git branch/dirty strip. Refreshed on anvil's
    /// triggers (new session, cwd change, command finished), never per frame.
    pub git_strip_cache: crate::pane_header::GitStripCache,

    /// In-flight header drag used to rearrange panes.
    pub pane_drag: Option<PaneDrag>,

    /// Tab-bar regions registered earlier in the current frame. Central pane
    /// rendering runs after top/sidebar bars and consumes these regions when a
    /// split-pane header is released over a bar.
    pub tab_bar_drop_rects: Vec<egui::Rect>,

    // Help panel
    pub help_panel: help::HelpPanel,
    pub remote_picker: crate::remote_picker::RemotePicker,

    // Config panel
    pub config_panel: config_panel::ConfigPanel,

    // Debug overlay panel
    pub debug_panel: debug_panel::DebugPanel,

    // AI agent panel (per-command approval agent over jterm_core)
    pub agent_panel: crate::agent_panel::AgentPanel,
    /// Native, read-only Git review surface requested by a structured Agent
    /// task. Git probes run on its bounded background worker.
    pub agent_diff: crate::agent::AgentDiffPanel,
    /// Background "is a newer jsh published?" check and the offer it produced.
    pub jsh_notice: crate::jsh_ui::JshNotice,

    // Config system
    pub config: config::Config,
    pub config_save_pending: bool,
    pub config_save_deadline: std::time::Instant,

    // Session persistence
    pub session_save_pending: bool,
    pub session_save_deadline: std::time::Instant,
    /// A corrupt snapshot that could not be moved aside must never be
    /// overwritten by the fresh fallback session on debounce or Drop.
    pub session_persistence_blocked: bool,

    // Lock file to detect running instances
    pub _lock_file: Option<crate::session_persistence::InstanceLock>,

    // 鼠标报告模式下的滚轮累积器
    pub mouse_scroll_accumulator: f32,
    /// Stable session identity and routing decision for the current drag.
    pub terminal_mouse_capture: Option<TerminalMouseCapture>,
    pub last_terminal_mouse_motion: Option<(String, usize, usize)>,

    // Ctrl+滚轮字体缩放累积器
    pub font_size_accumulator: f32,

    // 上一帧是否有Ctrl+滚轮事件
    pub had_ctrl_scroll_last_frame: bool,

    // 每帧事件缓存，避免多次克隆
    pub frame_events: Vec<egui::Event>,

    /// Matches V presses (including semantic text-paste events) with releases
    /// that may arrive in a later raw-input batch.
    pub paste_key_state: PasteKeyState,

    // 键盘输入缓冲区，复用以减少内存分配
    pub keyboard_input_buffer: Vec<u8>,

    // 自适应帧预算：根据帧时间动态调整每帧处理的字节数
    pub adaptive_frame_budget: usize,

    // Config hot-reload
    pub config_last_check: std::time::Instant,

    // Smooth scrolling
    pub smooth_scroll_velocity: f32,
    pub smooth_scroll_pixel_offset: f32,

    /// Holds a paste payload waiting for user confirmation. Populated by the
    /// paste handlers when `jterm_core::pty_input::should_confirm` returns true
    /// for the clipboard's classification; cleared by the modal renderer on
    /// confirm/cancel.
    pub pending_paste_confirm: Option<PendingPasteConfirm>,

    /// 粘贴确认对话框里"不再询问"复选框的临时状态(跨帧保留,直到对话框关闭)。
    pub paste_dont_ask_again: bool,

    /// Global rate limit for OSC 9/777. The queue inside each terminal is
    /// bounded, but draining it every frame without a time budget still lets
    /// untrusted PTY output fork hundreds of `notify-send` processes.
    pub notification_tx: Option<crossbeam_channel::Sender<crate::DesktopNotification>>,
    pub notification_window_started: std::time::Instant,
    pub notifications_in_window: usize,
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
