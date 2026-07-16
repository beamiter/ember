use crate::pty::Pty;
use crate::terminal::clamp_terminal_dimensions;
use crossbeam::channel::{bounded, Receiver};
use eframe::egui;
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

#[derive(Clone, Debug)]
pub enum ShellEvent {
    Output(Vec<u8>),
    Exit(i32),
    Error(String),
}

/// 事件 channel 容量上限。每个事件最多 ~128KB(BATCH_SIZE_THRESHOLD),
/// 256 * 128KB ≈ 32MB 为内存上界,既能吸收突发又能阻止无限堆积。
const EVENT_CHANNEL_CAP: usize = 256;

/// PTY writer 的硬上限包含等待队列和正在执行的 OS write。OSC 5522 会把
/// 32 MiB 剪贴板内容编码为约 43 MiB 的单条响应，因此单消息上限需留足空间。
const WRITE_QUEUE_BYTE_CAP: usize = 64 * 1024 * 1024;
const WRITE_MESSAGE_BYTE_CAP: usize = 48 * 1024 * 1024;
const WRITE_QUEUE_MESSAGE_CAP: usize = 8192;

/// bulk 写入不能占用最后这部分容量，尽量保证它阻塞时短按键、DSR 回复仍可入队。
const WRITE_INTERACTIVE_RESERVE_BYTES: usize = 64 * 1024;
const WRITE_INTERACTIVE_RESERVE_MESSAGES: usize = 256;
const WRITE_INTERACTIVE_MAX_BYTES: usize = 64;

#[derive(Clone, Copy)]
struct WriteQueueLimits {
    byte_capacity: usize,
    message_capacity: usize,
    max_message_bytes: usize,
    interactive_reserve_bytes: usize,
    interactive_reserve_messages: usize,
    interactive_max_bytes: usize,
}

impl WriteQueueLimits {
    const PRODUCTION: Self = Self {
        byte_capacity: WRITE_QUEUE_BYTE_CAP,
        message_capacity: WRITE_QUEUE_MESSAGE_CAP,
        max_message_bytes: WRITE_MESSAGE_BYTE_CAP,
        interactive_reserve_bytes: WRITE_INTERACTIVE_RESERVE_BYTES,
        interactive_reserve_messages: WRITE_INTERACTIVE_RESERVE_MESSAGES,
        interactive_max_bytes: WRITE_INTERACTIVE_MAX_BYTES,
    };
}

/// 非阻塞 PTY 写入的可判别失败原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellWriteError {
    /// 当前按字节或消息计数的队列已满；调用方可保留输入并在后续帧重试。
    Full {
        requested_bytes: usize,
        queued_bytes: usize,
        byte_capacity: usize,
        queued_messages: usize,
        message_capacity: usize,
    },
    /// 单条消息永远不可能进入队列；拆分协议消息或拒绝该操作。
    TooLarge {
        requested_bytes: usize,
        max_message_bytes: usize,
    },
    /// writer 已停止，后续数据不会被接受。
    Closed,
}

impl ShellWriteError {
    /// Full is transient and guarantees zero-byte enqueue, so callers can
    /// retain their exact input and retry. TooLarge/Closed cannot recover
    /// merely by waiting for queue capacity.
    pub fn is_backpressure(&self) -> bool {
        matches!(self, Self::Full { .. })
    }
}

impl fmt::Display for ShellWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full {
                requested_bytes,
                queued_bytes,
                byte_capacity,
                queued_messages,
                message_capacity,
            } => write!(
                f,
                "PTY writer queue full: requested {requested_bytes} bytes, \
                 {queued_bytes}/{byte_capacity} bytes and \
                 {queued_messages}/{message_capacity} messages are accounted"
            ),
            Self::TooLarge {
                requested_bytes,
                max_message_bytes,
            } => write!(
                f,
                "PTY write message is too large: {requested_bytes} bytes exceeds the \
                 {max_message_bytes}-byte limit"
            ),
            Self::Closed => f.write_str("PTY writer has stopped"),
        }
    }
}

impl std::error::Error for ShellWriteError {}

struct WriteQueueState {
    pending: VecDeque<Vec<u8>>,
    /// 包含 pending 和已 pop、尚未完成/失败的 in-flight 消息。
    accounted_bytes: usize,
    accounted_messages: usize,
    closed: bool,
}

struct WriteQueue {
    state: Mutex<WriteQueueState>,
    ready: Condvar,
    limits: WriteQueueLimits,
}

impl WriteQueue {
    fn new(limits: WriteQueueLimits) -> Self {
        debug_assert!(limits.max_message_bytes <= limits.byte_capacity);
        debug_assert!(limits.interactive_reserve_bytes < limits.byte_capacity);
        debug_assert!(limits.interactive_reserve_messages < limits.message_capacity);
        Self {
            state: Mutex::new(WriteQueueState {
                pending: VecDeque::new(),
                accounted_bytes: 0,
                accounted_messages: 0,
                closed: false,
            }),
            ready: Condvar::new(),
            limits,
        }
    }

    fn effective_capacity(&self, message_bytes: usize) -> (usize, usize) {
        if message_bytes <= self.limits.interactive_max_bytes {
            (self.limits.byte_capacity, self.limits.message_capacity)
        } else {
            (
                self.limits
                    .byte_capacity
                    .saturating_sub(self.limits.interactive_reserve_bytes),
                self.limits
                    .message_capacity
                    .saturating_sub(self.limits.interactive_reserve_messages),
            )
        }
    }

    fn validate_message(&self, requested_bytes: usize) -> Result<(), ShellWriteError> {
        if requested_bytes > self.limits.max_message_bytes {
            Err(ShellWriteError::TooLarge {
                requested_bytes,
                max_message_bytes: self.limits.max_message_bytes,
            })
        } else {
            Ok(())
        }
    }

    fn has_capacity(&self, state: &WriteQueueState, requested_bytes: usize) -> bool {
        let (byte_capacity, message_capacity) = self.effective_capacity(requested_bytes);
        state
            .accounted_bytes
            .checked_add(requested_bytes)
            .is_some_and(|bytes| bytes <= byte_capacity)
            && state.accounted_messages < message_capacity
    }

    fn full_error(&self, state: &WriteQueueState, requested_bytes: usize) -> ShellWriteError {
        let (byte_capacity, message_capacity) = self.effective_capacity(requested_bytes);
        ShellWriteError::Full {
            requested_bytes,
            queued_bytes: state.accounted_bytes,
            byte_capacity,
            queued_messages: state.accounted_messages,
            message_capacity,
        }
    }

    fn enqueue_locked(&self, state: &mut WriteQueueState, data: Vec<u8>) {
        state.accounted_bytes += data.len();
        state.accounted_messages += 1;
        state.pending.push_back(data);
        self.ready.notify_one();
    }

    fn try_enqueue(&self, data: Vec<u8>) -> Result<(), ShellWriteError> {
        if data.is_empty() {
            return Ok(());
        }
        self.validate_message(data.len())?;
        let mut state = self.state.lock();
        if state.closed {
            return Err(ShellWriteError::Closed);
        }
        if !self.has_capacity(&state, data.len()) {
            return Err(self.full_error(&state, data.len()));
        }
        self.enqueue_locked(&mut state, data);
        Ok(())
    }

    fn try_enqueue_slice(&self, data: &[u8]) -> Result<(), ShellWriteError> {
        if data.is_empty() {
            return Ok(());
        }
        self.validate_message(data.len())?;
        let mut state = self.state.lock();
        if state.closed {
            return Err(ShellWriteError::Closed);
        }
        if !self.has_capacity(&state, data.len()) {
            return Err(self.full_error(&state, data.len()));
        }
        // Clone only after capacity succeeds, so rejected large pastes do not
        // transiently duplicate their full allocation on the UI thread.
        self.enqueue_locked(&mut state, data.to_vec());
        Ok(())
    }

    fn take(&self) -> Option<Vec<u8>> {
        let mut state = self.state.lock();
        loop {
            if let Some(data) = state.pending.pop_front() {
                // accounted_* 在 OS write 完成或失败前保持不变。
                return Some(data);
            }
            if state.closed {
                return None;
            }
            self.ready.wait(&mut state);
        }
    }

    fn finish(&self, bytes: usize) {
        let mut state = self.state.lock();
        debug_assert!(state.accounted_bytes >= bytes);
        debug_assert!(state.accounted_messages > 0);
        state.accounted_bytes = state.accounted_bytes.saturating_sub(bytes);
        state.accounted_messages = state.accounted_messages.saturating_sub(1);
    }

    fn close(&self) {
        let mut state = self.state.lock();
        if state.closed {
            return;
        }
        state.closed = true;
        let dropped_bytes = state.pending.iter().map(Vec::len).sum::<usize>();
        let dropped_messages = state.pending.len();
        state.pending.clear();
        state.accounted_bytes = state.accounted_bytes.saturating_sub(dropped_bytes);
        state.accounted_messages = state.accounted_messages.saturating_sub(dropped_messages);
        self.ready.notify_all();
    }
}

/// 可克隆的统一 PTY 写入口。成功返回表示整条消息已按字节计入 FIFO；
/// 不表示内核已经写完。任何错误都保证消息没有部分入队。
#[derive(Clone)]
pub struct ShellWriteSender {
    queue: Arc<WriteQueue>,
}

impl ShellWriteSender {
    /// UI/输入路径使用：从不等待容量。
    pub fn try_send(&self, data: Vec<u8>) -> Result<(), ShellWriteError> {
        self.queue.try_enqueue(data)
    }
}

/// ShellSession 管理 PTY 和后台 I/O 线程
pub struct ShellSession {
    pty: Arc<Mutex<Pty>>,
    event_rx: Receiver<ShellEvent>,
    child_pid: i32,            // 存储 shell 子进程的 PID
    shutdown: Arc<AtomicBool>, // 通知 IO 线程退出
    // 所有 PTY 写入都经此 channel 交给单一 writer 线程串行执行,保证每条消息原子写入、
    // 互不交错(否则键盘输入与异步粘贴会逐块交错,劈开括号粘贴标记 ESC[200~..201~)。
    write_tx: ShellWriteSender,
}

impl ShellSession {
    /// 启动新的 shell session
    #[allow(dead_code)]
    pub fn new(
        cols: usize,
        rows: usize,
        configured_shell: Option<&str>,
        repaint_ctx: egui::Context,
    ) -> std::result::Result<Self, String> {
        Self::new_with_cwd(cols, rows, None, None, configured_shell, repaint_ctx)
    }

    /// 启动新的 shell session，指定初始工作目录和 session ID
    pub fn new_with_cwd(
        cols: usize,
        rows: usize,
        cwd: Option<&str>,
        session_id: Option<&str>,
        configured_shell: Option<&str>,
        repaint_ctx: egui::Context,
    ) -> std::result::Result<Self, String> {
        let (cols, rows) = clamp_terminal_dimensions(cols, rows);
        match Pty::new_with_cwd(cols, rows, cwd, session_id, configured_shell) {
            Ok(pty) => {
                // 在把 pty 放入 Arc<Mutex> 前获取 child_pid
                let child_pid = pty.get_child_pid();

                // 有界 channel 提供背压:UI 消费不及时,IO 线程会阻塞在 send 上,
                // 停止读取 PTY → 内核 PTY 缓冲填满 → 高产出进程(如 yes)自身阻塞,
                // 避免 unbounded channel 无限增长导致 OOM。
                let (event_tx, event_rx) = bounded::<ShellEvent>(EVENT_CHANNEL_CAP);
                let shutdown = Arc::new(AtomicBool::new(false));

                let pty = Arc::new(Mutex::new(pty));
                let pty_clone = Arc::clone(&pty);
                let repaint_ctx_clone = repaint_ctx.clone();
                let shutdown_clone = Arc::clone(&shutdown);
                let writer_event_tx = event_tx.clone();
                let writer_repaint_ctx = repaint_ctx.clone();

                thread::spawn(move || {
                    Self::io_loop(pty_clone, event_tx, repaint_ctx_clone, shutdown_clone);
                });

                // 单一 writer 线程串行消费按字节和消息数双重有界的 FIFO。
                // accounted_bytes 在 OS write 完成前不会释放，避免一个 43 MiB
                // in-flight 消息之外又堆满 64 MiB pending 数据。
                let write_queue = Arc::new(WriteQueue::new(WriteQueueLimits::PRODUCTION));
                let write_tx = ShellWriteSender {
                    queue: Arc::clone(&write_queue),
                };
                let pty_writer = Arc::clone(&pty);
                let shutdown_writer = Arc::clone(&shutdown);
                let writer_result =
                    thread::Builder::new()
                        .name("pty-writer".to_string())
                        .spawn(move || {
                            while let Some(data) = write_queue.take() {
                                let message_bytes = data.len();
                                if shutdown_writer.load(Ordering::Relaxed) {
                                    // Mark closed before releasing in-flight capacity;
                                    // otherwise a waking producer could enqueue an Ok
                                    // message in the gap and have it silently discarded.
                                    write_queue.close();
                                    write_queue.finish(message_bytes);
                                    break;
                                }
                                if let Err(e) = Self::write_to_pty(&pty_writer, &data) {
                                    eprintln!("[ERROR] Failed to write to PTY: {}", e);
                                    // Partial/timeout 后继续下一条会把协议尾部接到残缺消息后。
                                    // 关闭队列，让所有生产者获得 Closed，而不是继续损坏字节流。
                                    write_queue.close();
                                    write_queue.finish(message_bytes);
                                    drop(data);
                                    let _ = Self::send_event(
                                        &writer_event_tx,
                                        &writer_repaint_ctx,
                                        ShellEvent::Error(format!(
                                        "PTY writer stopped after a partial or failed write: {e}"
                                    )),
                                    );
                                    return;
                                }
                                write_queue.finish(message_bytes);
                            }
                            write_queue.close();
                        });

                if let Err(error) = writer_result {
                    write_tx.queue.close();
                    shutdown.store(true, Ordering::Relaxed);
                    return Err(format!("Failed to spawn PTY writer: {error}"));
                }

                Ok(ShellSession {
                    pty,
                    event_rx,
                    child_pid,
                    shutdown,
                    write_tx,
                })
            }
            Err(e) => Err(format!("Failed to create shell session: {}", e)),
        }
    }

    /// 获取事件接收器（用于读取 shell 事件）
    pub fn events(&self) -> &Receiver<ShellEvent> {
        &self.event_rx
    }

    fn send_event(
        event_tx: &crossbeam::channel::Sender<ShellEvent>,
        repaint_ctx: &egui::Context,
        event: ShellEvent,
    ) -> bool {
        if event_tx.send(event).is_err() {
            return false;
        }
        repaint_ctx.request_repaint();
        true
    }

    /// 后台 I/O 循环 - 阻塞等待 PTY 可读，避免忙轮询
    /// P3 优化：批量读取 PTY 数据，累积后一次性发送事件
    fn io_loop(
        pty: Arc<Mutex<Pty>>,
        event_tx: crossbeam::channel::Sender<ShellEvent>,
        repaint_ctx: egui::Context,
        shutdown: Arc<AtomicBool>,
    ) {
        const BUFFER_SIZE: usize = 65536;
        const BATCH_SIZE_THRESHOLD: usize = 131072;
        const BATCH_TIMEOUT_MS: u64 = 2;
        const ALIVE_CHECK_MS: u64 = 250;

        let master_fd = pty.lock().master_fd();

        let mut buf = vec![0u8; BUFFER_SIZE];
        let mut accumulated = Vec::with_capacity(BATCH_SIZE_THRESHOLD);
        let mut last_alive_check = std::time::Instant::now();
        let mut last_batch_time = std::time::Instant::now();
        // EOF/EIO means the slave stream is closed, not necessarily that the
        // child has exited. Once set, stop polling the permanently-ready master
        // fd and reap at a bounded cadence instead of emitting a fabricated exit.
        let mut read_closed = false;
        let mut hangup_probe_delay = Duration::from_millis(50);
        // 非 hangup 的重复读取错误只报告一次，直到再次读到数据。
        let mut read_error_reported = false;

        crate::debug_log!("[IOLoop] 后台 I/O 线程启动 (P3 批处理优化)");

        loop {
            // 检查 shutdown 标志
            if shutdown.load(Ordering::Relaxed) {
                crate::debug_log!("[IOLoop] 收到 shutdown 信号，退出 IO 线程");
                return;
            }

            if read_closed {
                if !accumulated.is_empty() {
                    let data = std::mem::take(&mut accumulated);
                    if !Self::send_event(&event_tx, &repaint_ctx, ShellEvent::Output(data)) {
                        return;
                    }
                }
                // Hangup can be transient: a live child may later reopen
                // /dev/tty. Probe non-blockingly before reaping so that Data or
                // WouldBlock returns the loop to normal poll-based reading.
                let recovery_probe = {
                    let mut pty_guard = pty.lock();
                    pty_guard.read(&mut buf)
                };
                match recovery_probe {
                    Ok(crate::pty::ReadOutcome::Data(n)) => {
                        accumulated.extend_from_slice(&buf[..n]);
                        read_error_reported = false;
                        read_closed = false;
                        hangup_probe_delay = Duration::from_millis(50);
                        continue;
                    }
                    Ok(crate::pty::ReadOutcome::WouldBlock) => {
                        read_error_reported = false;
                        read_closed = false;
                        hangup_probe_delay = Duration::from_millis(50);
                        continue;
                    }
                    Ok(crate::pty::ReadOutcome::Eof | crate::pty::ReadOutcome::Hangup) => {}
                    Err(error) if !read_error_reported => {
                        read_error_reported = true;
                        if !Self::send_event(
                            &event_tx,
                            &repaint_ctx,
                            ShellEvent::Error(format!(
                                "Read error while waiting for PTY recovery: {error}"
                            )),
                        ) {
                            return;
                        }
                    }
                    Err(_) => {}
                }
                if let Some(exit_code) = pty.lock().try_reap() {
                    let _ = Self::send_event(&event_tx, &repaint_ctx, ShellEvent::Exit(exit_code));
                    return;
                }
                // POLLHUP/EIO remains immediately ready forever. Back off to
                // four probes/second for a child that intentionally keeps the
                // slave closed, while retaining fast transient recovery.
                std::thread::sleep(hangup_probe_delay);
                hangup_probe_delay = std::cmp::min(
                    hangup_probe_delay.saturating_mul(2),
                    Duration::from_millis(250),
                );
                continue;
            }

            // 动态计算超时：累积数据时快速超时，空闲时正常超时
            let timeout_ms = if !accumulated.is_empty() {
                // 已有累积数据，快速超时以便发送
                let elapsed_ms = last_batch_time.elapsed().as_millis() as i32;
                let remaining_ms = BATCH_TIMEOUT_MS as i32 - elapsed_ms;
                remaining_ms.clamp(1, 100)
            } else {
                // 无累积数据，使用标准超时（减去 alive_check 耗时）
                (ALIVE_CHECK_MS as i32)
                    .saturating_sub(last_alive_check.elapsed().as_millis() as i32)
                    .max(1)
            };

            match Pty::wait_fd_readable(master_fd, timeout_ms) {
                Ok(true) => {
                    // 锁内仅读取/累积数据并捕获终止动作;所有 send 移到锁外,
                    // 避免有界 channel 满时持锁阻塞,与 UI 线程 write() 形成死锁。
                    enum After {
                        Continue,
                        ContinueWith(ShellEvent),
                        Stop(ShellEvent),
                        // 流关闭与进程退出是两件事；锁外进入低频 try_reap 状态。
                        ReadClosed,
                    }
                    let after = {
                        let mut pty_guard = pty.lock();
                        let mut after = After::Continue;
                        loop {
                            match pty_guard.read(&mut buf) {
                                Ok(crate::pty::ReadOutcome::Data(n)) => {
                                    read_error_reported = false;
                                    accumulated.extend_from_slice(&buf[..n]);
                                    if accumulated.len() >= BATCH_SIZE_THRESHOLD {
                                        break;
                                    }
                                }
                                Ok(crate::pty::ReadOutcome::WouldBlock) => break,
                                Ok(crate::pty::ReadOutcome::Eof) => {
                                    after = After::ReadClosed;
                                    break;
                                }
                                Ok(crate::pty::ReadOutcome::Hangup) => {
                                    // Linux PTY master 的 EIO 表示当前没有 slave fd。
                                    // 子进程仍可能活着，绝不能在这里伪造 Exit(-1)。
                                    after = After::ReadClosed;
                                    break;
                                }
                                Err(e) => {
                                    crate::debug_log!("[IOLoop] 读取错误: {}", e);
                                    if !pty_guard.is_alive() {
                                        after = After::Stop(match pty_guard.wait_timeout(0) {
                                            Ok(exit_code) => ShellEvent::Exit(exit_code),
                                            Err(e) => ShellEvent::Error(format!(
                                                "Process exit error: {}",
                                                e
                                            )),
                                        });
                                    } else if !read_error_reported {
                                        // 仅首次报告，避免异常 fd 向 UI 持续刷屏。
                                        read_error_reported = true;
                                        after = After::ContinueWith(ShellEvent::Error(format!(
                                            "Read error: {}",
                                            e
                                        )));
                                    }
                                    break;
                                }
                            }
                        }
                        after
                    };
                    // 锁已释放,以下 send 可安全阻塞(背压生效点)
                    if !accumulated.is_empty()
                        && (accumulated.len() >= BATCH_SIZE_THRESHOLD
                            || last_batch_time.elapsed().as_millis() >= BATCH_TIMEOUT_MS as u128
                            || !matches!(after, After::Continue))
                    {
                        let data = std::mem::take(&mut accumulated);
                        if !Self::send_event(&event_tx, &repaint_ctx, ShellEvent::Output(data)) {
                            return;
                        }
                        last_batch_time = std::time::Instant::now();
                    }
                    match after {
                        After::Continue => {}
                        After::ContinueWith(ev) => {
                            if !Self::send_event(&event_tx, &repaint_ctx, ev) {
                                return;
                            }
                        }
                        After::Stop(ev) => {
                            let _ = Self::send_event(&event_tx, &repaint_ctx, ev);
                            return;
                        }
                        After::ReadClosed => {
                            read_closed = true;
                        }
                    }
                }
                Ok(false) => {
                    // 超时，检查是否需要发送累积的数据
                    if !accumulated.is_empty()
                        && last_batch_time.elapsed().as_millis() >= BATCH_TIMEOUT_MS as u128
                    {
                        let data = std::mem::take(&mut accumulated);
                        if !Self::send_event(&event_tx, &repaint_ctx, ShellEvent::Output(data)) {
                            crate::debug_log!("[IOLoop] 接收者已断开，退出循环");
                            return;
                        }
                        last_batch_time = std::time::Instant::now();
                    }
                }
                Err(e) => {
                    if !Self::send_event(
                        &event_tx,
                        &repaint_ctx,
                        ShellEvent::Error(format!("Poll error: {}", e)),
                    ) {
                        return;
                    }
                }
            }

            // 检查进程是否存活（250ms 检查一次，空闲时降低唤醒频率）
            if last_alive_check.elapsed() >= Duration::from_millis(ALIVE_CHECK_MS) {
                // 在锁内判定退出并取退出码,锁外再发送事件(同样为避免持锁阻塞)
                let exit_event = {
                    let mut pty_guard = pty.lock();
                    if !pty_guard.is_alive() {
                        crate::debug_log!("[IOLoop] 检测到子进程已退出");
                        Some(match pty_guard.wait_timeout(0) {
                            Ok(exit_code) => {
                                crate::debug_log!("[IOLoop] 子进程退出码: {}", exit_code);
                                ShellEvent::Exit(exit_code)
                            }
                            Err(e) => ShellEvent::Error(format!("Process exit error: {}", e)),
                        })
                    } else {
                        None
                    }
                };
                if let Some(exit_event) = exit_event {
                    if !accumulated.is_empty() {
                        let data = std::mem::take(&mut accumulated);
                        let _ = Self::send_event(&event_tx, &repaint_ctx, ShellEvent::Output(data));
                    }
                    let _ = Self::send_event(&event_tx, &repaint_ctx, exit_event);
                    return;
                }
                last_alive_check = std::time::Instant::now();
            }
        }
    }

    /// 处理大数据写入：循环写入并在 poll 等待时释放锁，避免与 io_loop 死锁。
    /// 仅由 writer 线程调用,保证全局只有一个写者 —— 字节流不会与其他写入交错。
    fn write_to_pty(pty: &Arc<Mutex<Pty>>, data: &[u8]) -> std::result::Result<(), String> {
        let mut offset = 0;
        let mut last_progress = std::time::Instant::now();
        let stall_timeout = std::time::Duration::from_secs(10);

        // 先获取 master_fd（不需要长期持锁）
        let master_fd = {
            let pty = pty.lock();
            pty.master_fd()
        };

        while offset < data.len() {
            if last_progress.elapsed() > stall_timeout {
                return Err(format!(
                    "PTY write stalled for 10 seconds, wrote {}/{} bytes",
                    offset,
                    data.len()
                ));
            }

            // 获取锁，尝试写入，然后立即释放锁
            {
                let mut pty = pty.lock();
                match pty.write(&data[offset..]) {
                    Ok(crate::pty::WriteOutcome::Written(n)) if n > 0 => {
                        offset += n;
                        last_progress = std::time::Instant::now();
                        continue; // 写成功(可能 partial)，立刻尝试写剩余部分
                    }
                    Ok(crate::pty::WriteOutcome::Written(_)) => {
                        return Err(format!(
                            "PTY write returned zero after {offset}/{} bytes",
                            data.len()
                        ));
                    }
                    Ok(crate::pty::WriteOutcome::WouldBlock) => {
                        // 缓冲区满，需要 poll 等待可写 — 释放锁（落到下面）
                    }
                    Err(e) => {
                        return Err(format!("Write error: {}", e));
                    }
                }
            }
            // 锁已释放！io_loop 可以读取 PTY 输出，vim 可以排空缓冲区
            // poll 等待 PTY 可写
            let mut pfd = libc::pollfd {
                fd: master_fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            loop {
                // SAFETY: pfd 是有效的栈变量，fd 在 Pty Arc 存活期间保持打开。
                let result = unsafe { libc::poll(&mut pfd, 1, 50) };
                if result >= 0 {
                    break;
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(format!("poll(POLLOUT) failed: {error}"));
                }
            }
            if pfd.revents & libc::POLLOUT == 0
                && pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
            {
                return Err(format!(
                    "PTY became unavailable during write after {offset}/{} bytes (revents={:#x})",
                    data.len(),
                    pfd.revents
                ));
            }
        }
        Ok(())
    }

    /// 向 shell 非阻塞发送输入。Ok 表示整条消息已入 FIFO；Full/TooLarge/Closed
    /// 都保证一个字节也没有入队，调用方可以安全保留原输入后重试。
    pub fn write(&self, data: &[u8]) -> Result<(), ShellWriteError> {
        self.write_tx.queue.try_enqueue_slice(data)
    }

    /// 返回写入队列的发送端,供需要先做阻塞 I/O(如读剪贴板)再写 PTY 的后台线程使用。
    /// 经由它入队的数据同样被 writer 线程串行写入,保证不与其他写入交错。
    pub fn write_sender(&self) -> ShellWriteSender {
        self.write_tx.clone()
    }

    pub fn resize(&self, cols: usize, rows: usize) -> std::result::Result<(), String> {
        let mut pty = self.pty.lock();
        pty.resize(cols, rows)
            .map_err(|e| format!("Resize error: {}", e))
    }

    /// 获取 shell 子进程的 PID
    pub fn get_child_pid(&self) -> i32 {
        self.child_pid
    }
}

impl Drop for ShellSession {
    /// 清理shell进程及其子进程
    ///
    /// 多层保护机制确保rsh进程在jterm2退出时被清理：
    /// 1. 正常退出：Drop被调用，发送SIGHUP/SIGTERM/SIGKILL到进程组
    /// 2. SIGINT/SIGTERM：信号处理器触发正常退出，Drop被调用
    /// 3. SIGKILL或panic：PR_SET_PDEATHSIG确保子进程收到SIGTERM
    ///
    /// 进程组杀死：使用负PID向整个进程组发送信号，因为shell通过
    /// setsid()创建了新会话，所以child_pid就是进程组ID
    fn drop(&mut self) {
        // 先关闭生产入口，保证任何并发 sender 的 Ok 都不会在 shutdown
        // 窗口内被丢弃；再唤醒/通知两个后台线程退出。
        self.write_tx.queue.close();
        self.shutdown.store(true, Ordering::Relaxed);

        // 所有发信号/回收都经由 Pty 的互斥锁,与 io_loop 的回收路径串行化;
        // 并且仅在子进程尚未被回收(exit_code_cached 为空)时才 kill。
        // 这样杜绝了"先被 io_loop reap、PID 被复用、随后 kill 误杀无辜进程"的竞争。
        {
            let mut pty = self.pty.lock();
            pty.signal_terminate(); // 瞬时:SIGHUP 进程组 + SIGTERM
        }

        // 把 "等待 → SIGKILL → 回收僵尸" 放到独立线程,避免在 UI 线程 sleep。
        // 关闭多个会话时,这能把累计阻塞降到接近 0。reaper 持有 Pty 的 Arc,
        // 保证 Pty 在回收完成前不被释放。
        let pty = Arc::clone(&self.pty);
        let _ = thread::Builder::new()
            .name("pty-reaper".to_string())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(30));
                pty.lock().force_kill_and_reap();
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_limits() -> WriteQueueLimits {
        WriteQueueLimits {
            byte_capacity: 10,
            message_capacity: 4,
            max_message_bytes: 8,
            interactive_reserve_bytes: 2,
            interactive_reserve_messages: 1,
            interactive_max_bytes: 2,
        }
    }

    fn test_sender(limits: WriteQueueLimits) -> (Arc<WriteQueue>, ShellWriteSender) {
        let queue = Arc::new(WriteQueue::new(limits));
        let sender = ShellWriteSender {
            queue: Arc::clone(&queue),
        };
        (queue, sender)
    }

    #[test]
    fn write_queue_is_fifo_byte_bounded_and_keeps_inflight_accounted() {
        let (queue, sender) = test_sender(tiny_limits());

        sender.try_send(vec![1; 8]).unwrap();
        let first = queue.take().unwrap();
        assert_eq!(first, vec![1; 8]);

        // pop 后仍计入 8 bytes；bulk 保留的 2-byte 交互区仍可接收短输入。
        sender.try_send(vec![2; 2]).unwrap();
        let error = sender.try_send(vec![3]).unwrap_err();
        assert!(matches!(
            error,
            ShellWriteError::Full {
                requested_bytes: 1,
                queued_bytes: 10,
                ..
            }
        ));

        queue.finish(first.len());
        let second = queue.take().unwrap();
        assert_eq!(second, vec![2; 2]);
        queue.finish(second.len());

        let state = queue.state.lock();
        assert_eq!(state.accounted_bytes, 0);
        assert_eq!(state.accounted_messages, 0);
    }

    #[test]
    fn write_queue_distinguishes_too_large_full_and_closed() {
        let (queue, sender) = test_sender(tiny_limits());

        assert!(matches!(
            sender.try_send(vec![0; 9]),
            Err(ShellWriteError::TooLarge {
                requested_bytes: 9,
                max_message_bytes: 8
            })
        ));

        sender.try_send(vec![0; 8]).unwrap();
        assert!(matches!(
            sender.try_send(vec![0; 3]),
            Err(ShellWriteError::Full { .. })
        ));
        queue.close();
        assert!(matches!(
            sender.try_send(vec![1]),
            Err(ShellWriteError::Closed)
        ));
        // Empty writes never allocate or enqueue and are harmless even after close.
        sender.try_send(Vec::new()).unwrap();
    }

    #[test]
    fn closing_queue_drops_pending_but_keeps_inflight_accounted_until_finish() {
        let limits = WriteQueueLimits {
            byte_capacity: 5,
            message_capacity: 2,
            max_message_bytes: 4,
            interactive_reserve_bytes: 0,
            interactive_reserve_messages: 0,
            interactive_max_bytes: 4,
        };
        let (queue, sender) = test_sender(limits);
        sender.try_send(vec![1; 4]).unwrap();
        let in_flight = queue.take().unwrap();

        sender.try_send(vec![2]).unwrap();
        queue.close();
        assert!(matches!(
            sender.try_send(vec![3]),
            Err(ShellWriteError::Closed)
        ));

        {
            let state = queue.state.lock();
            assert_eq!(state.accounted_bytes, in_flight.len());
            assert_eq!(state.accounted_messages, 1);
            assert!(state.pending.is_empty());
        }
        queue.finish(in_flight.len());
        let state = queue.state.lock();
        assert_eq!(state.accounted_bytes, 0);
        assert_eq!(state.accounted_messages, 0);
    }

    #[test]
    fn write_queue_caps_message_count_as_well_as_bytes() {
        let limits = WriteQueueLimits {
            byte_capacity: 100,
            message_capacity: 2,
            max_message_bytes: 50,
            interactive_reserve_bytes: 0,
            interactive_reserve_messages: 0,
            interactive_max_bytes: 50,
        };
        let (_queue, sender) = test_sender(limits);
        sender.try_send(vec![1]).unwrap();
        sender.try_send(vec![2]).unwrap();
        assert!(matches!(
            sender.try_send(vec![3]),
            Err(ShellWriteError::Full {
                queued_messages: 2,
                message_capacity: 2,
                ..
            })
        ));
    }

    #[test]
    fn osc5522_maximum_legal_response_fits_single_message_limit() {
        const RAW_BYTES: usize = 32 * 1024 * 1024;
        const CHUNK_BYTES: usize = 4096;
        // Base64 plus a deliberately generous 256 bytes of OSC framing per chunk.
        let base64_bytes = RAW_BYTES.div_ceil(3) * 4;
        let chunks = RAW_BYTES.div_ceil(CHUNK_BYTES);
        let conservative_response_bytes = base64_bytes + chunks * 256 + 1024;
        assert!(conservative_response_bytes <= WRITE_MESSAGE_BYTE_CAP);
    }

    #[cfg(unix)]
    #[test]
    fn transient_pty_hangup_waits_for_real_exit_and_recovers_output() {
        use crossbeam::channel::RecvTimeoutError;
        use std::os::unix::fs::PermissionsExt;

        let unique = format!(
            "jterm2-pty-hangup-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let script_path = std::env::temp_dir().join(unique);
        std::fs::write(
            &script_path,
            b"#!/bin/sh\nexec 0>&- 1>&- 2>&-\nsleep 0.25\nexec 0</dev/tty 1>/dev/tty 2>/dev/tty\nprintf recovered\nsleep 0.05\nexit 7\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script_path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script_path, permissions).unwrap();

        let script = script_path.to_string_lossy().into_owned();
        let session = ShellSession::new(80, 24, Some(&script), egui::Context::default()).unwrap();

        // The old implementation emitted Exit(-1) after a 30 ms reap window,
        // causing the live child to be dropped and killed before it could reopen.
        assert!(matches!(
            session.events().recv_timeout(Duration::from_millis(100)),
            Err(RecvTimeoutError::Timeout)
        ));

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut output = Vec::new();
        let exit_code = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for the test shell");
            match session.events().recv_timeout(remaining).unwrap() {
                ShellEvent::Output(bytes) => output.extend(bytes),
                ShellEvent::Exit(code) => break code,
                ShellEvent::Error(error) => panic!("unexpected shell error: {error}"),
            }
        };
        assert_eq!(output, b"recovered");
        assert_eq!(exit_code, 7);

        drop(session);
        let _ = std::fs::remove_file(script_path);
    }
}
