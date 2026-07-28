use crate::session::Session;
use crate::session_persistence;
use crate::shell::{ShellEvent, ShellSession, ShellWriteError};
use crate::terminal::{
    clamp_terminal_dimensions, ClipboardReadRequest, CompletedCommandOutput, TerminalState,
};
use eframe::egui;
use parking_lot::{Condvar, Mutex as ParkingMutex};
use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;

/// Protocol replies must survive transient PTY-writer backpressure. Keep a
/// second, per-session queue so draining `TerminalState::output_buffer` never
/// turns a temporary `Full` into a permanently missing DSR/DA/OSC reply.
///
/// The largest supported OSC 5522 reply is below the shell writer's 48 MiB
/// single-message limit. A 56 MiB pending budget leaves room for that reply and
/// several MiB of control traffic while remaining strictly bounded per PTY.
const PROTOCOL_RESPONSE_BYTE_CAP: usize = 56 * 1024 * 1024;
const PROTOCOL_RESPONSE_MESSAGE_CAP: usize = 4096;
const PROTOCOL_RESPONSE_MAX_MESSAGE_BYTES: usize = 48 * 1024 * 1024;
const PROTOCOL_CRITICAL_RESERVE_BYTES: usize = 256 * 1024;
const PROTOCOL_CRITICAL_RESERVE_MESSAGES: usize = 128;

#[derive(Clone, Copy)]
struct ProtocolResponseLimits {
    byte_capacity: usize,
    message_capacity: usize,
    max_message_bytes: usize,
    critical_reserve_bytes: usize,
    critical_reserve_messages: usize,
}

impl ProtocolResponseLimits {
    const PRODUCTION: Self = Self {
        byte_capacity: PROTOCOL_RESPONSE_BYTE_CAP,
        message_capacity: PROTOCOL_RESPONSE_MESSAGE_CAP,
        max_message_bytes: PROTOCOL_RESPONSE_MAX_MESSAGE_BYTES,
        critical_reserve_bytes: PROTOCOL_CRITICAL_RESERVE_BYTES,
        critical_reserve_messages: PROTOCOL_CRITICAL_RESERVE_MESSAGES,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolResponseQueueError {
    Full,
    TooLarge {
        requested_bytes: usize,
        max_message_bytes: usize,
    },
    Closed,
}

impl fmt::Display for ProtocolResponseQueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => f.write_str("protocol response queue is full"),
            Self::TooLarge {
                requested_bytes,
                max_message_bytes,
            } => write!(
                f,
                "protocol response is too large: {requested_bytes} bytes exceeds {max_message_bytes}"
            ),
            Self::Closed => f.write_str("protocol response queue is closed"),
        }
    }
}

struct ProtocolResponseState {
    /// One FIFO preserves request/response ordering. Capacity reservation lets
    /// small replies enter behind a bulk response without jumping ahead of it.
    pending: VecDeque<Vec<u8>>,
    accounted_bytes: usize,
    accounted_messages: usize,
    closed: bool,
}

struct ProtocolResponseQueue {
    state: ParkingMutex<ProtocolResponseState>,
    capacity_available: Condvar,
    limits: ProtocolResponseLimits,
    repaint_ctx: egui::Context,
}

/// Cloneable producer handle used by OSC worker threads. Producers only enter
/// this bounded queue; the UI/background pump remains the sole owner that
/// forwards queued bytes to the session's `ShellSession`.
#[derive(Clone)]
pub struct ProtocolResponseSender {
    queue: Arc<ProtocolResponseQueue>,
}

impl ProtocolResponseSender {
    fn new(repaint_ctx: egui::Context) -> Self {
        Self::new_with_limits(repaint_ctx, ProtocolResponseLimits::PRODUCTION)
    }

    fn new_with_limits(repaint_ctx: egui::Context, limits: ProtocolResponseLimits) -> Self {
        debug_assert!(limits.max_message_bytes <= limits.byte_capacity);
        debug_assert!(limits.critical_reserve_bytes < limits.byte_capacity);
        debug_assert!(limits.critical_reserve_messages < limits.message_capacity);
        Self {
            queue: Arc::new(ProtocolResponseQueue {
                state: ParkingMutex::new(ProtocolResponseState {
                    pending: VecDeque::new(),
                    accounted_bytes: 0,
                    accounted_messages: 0,
                    closed: false,
                }),
                capacity_available: Condvar::new(),
                limits,
                repaint_ctx,
            }),
        }
    }

    fn effective_capacity(&self, critical: bool) -> (usize, usize) {
        if critical {
            (
                self.queue.limits.byte_capacity,
                self.queue.limits.message_capacity,
            )
        } else {
            (
                self.queue
                    .limits
                    .byte_capacity
                    .saturating_sub(self.queue.limits.critical_reserve_bytes),
                self.queue
                    .limits
                    .message_capacity
                    .saturating_sub(self.queue.limits.critical_reserve_messages),
            )
        }
    }

    fn has_capacity(&self, state: &ProtocolResponseState, bytes: usize, critical: bool) -> bool {
        let (byte_capacity, message_capacity) = self.effective_capacity(critical);
        state
            .accounted_bytes
            .checked_add(bytes)
            .is_some_and(|total| total <= byte_capacity)
            && state.accounted_messages < message_capacity
    }

    fn validate(&self, bytes: usize) -> Result<(), ProtocolResponseQueueError> {
        if bytes > self.queue.limits.max_message_bytes {
            Err(ProtocolResponseQueueError::TooLarge {
                requested_bytes: bytes,
                max_message_bytes: self.queue.limits.max_message_bytes,
            })
        } else {
            Ok(())
        }
    }

    fn enqueue_locked(&self, state: &mut ProtocolResponseState, response: Vec<u8>) {
        state.accounted_bytes += response.len();
        state.accounted_messages += 1;
        state.pending.push_back(response);
        self.queue.repaint_ctx.request_repaint();
    }

    /// Non-blocking UI/parser entry. On failure the exact response is returned
    /// so a caller can restore it ahead of newer terminal output.
    fn try_enqueue_with_priority(
        &self,
        response: Vec<u8>,
        critical: bool,
    ) -> Result<(), (ProtocolResponseQueueError, Vec<u8>)> {
        if response.is_empty() {
            return Ok(());
        }
        if let Err(error) = self.validate(response.len()) {
            return Err((error, response));
        }
        let mut state = self.queue.state.lock();
        if state.closed {
            return Err((ProtocolResponseQueueError::Closed, response));
        }
        if !self.has_capacity(&state, response.len(), critical) {
            return Err((ProtocolResponseQueueError::Full, response));
        }
        self.enqueue_locked(&mut state, response);
        Ok(())
    }

    pub fn try_enqueue(
        &self,
        response: Vec<u8>,
    ) -> Result<(), (ProtocolResponseQueueError, Vec<u8>)> {
        self.try_enqueue_with_priority(response, false)
    }

    /// Critical replies (busy/denied/empty query results) may use the reserved
    /// tail of the queue, but still append to the same FIFO.
    pub fn try_enqueue_critical(
        &self,
        response: Vec<u8>,
    ) -> Result<(), (ProtocolResponseQueueError, Vec<u8>)> {
        self.try_enqueue_with_priority(response, true)
    }

    /// Bounded-memory worker entry. Waiting releases the queue lock and is
    /// woken by a successful flush or session close. Callers must themselves
    /// be single-flight or hold only a small, bounded response while waiting.
    pub fn enqueue_blocking(&self, response: Vec<u8>) -> Result<(), ProtocolResponseQueueError> {
        if response.is_empty() {
            return Ok(());
        }
        self.validate(response.len())?;
        let mut state = self.queue.state.lock();
        loop {
            if state.closed {
                return Err(ProtocolResponseQueueError::Closed);
            }
            if self.has_capacity(&state, response.len(), true) {
                self.enqueue_locked(&mut state, response);
                return Ok(());
            }
            self.queue.capacity_available.wait(&mut state);
        }
    }

    /// Forward as much queued protocol traffic as the shell writer currently
    /// accepts. `ShellSession::write(&[u8])` clones only after capacity is
    /// secured, so a failed attempt leaves the exact queued bytes untouched.
    pub fn flush(&self, shell: &ShellSession) -> Result<usize, ShellWriteError> {
        let mut state = self.queue.state.lock();
        let mut flushed = 0;
        loop {
            let response = state.pending.front();
            let Some(response) = response else {
                return Ok(flushed);
            };

            match shell.write(response) {
                Ok(()) => {
                    let response = state.pending.pop_front().expect("front response existed");
                    state.accounted_bytes = state.accounted_bytes.saturating_sub(response.len());
                    state.accounted_messages = state.accounted_messages.saturating_sub(1);
                    flushed += 1;
                    self.queue.capacity_available.notify_all();
                }
                Err(error) if error.is_backpressure() => {
                    // A one-shot repaint from enqueue is not enough when the
                    // shell queue remains full for several frames. Keep retrying
                    // at a bounded cadence even if the PTY is otherwise silent.
                    self.queue
                        .repaint_ctx
                        .request_repaint_after(std::time::Duration::from_millis(10));
                    return Err(error);
                }
                Err(error) => {
                    Self::close_locked(&self.queue, &mut state);
                    return Err(error);
                }
            }
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.queue.state.lock().pending.is_empty()
    }

    fn close_locked(queue: &ProtocolResponseQueue, state: &mut ProtocolResponseState) {
        state.closed = true;
        state.pending.clear();
        state.accounted_bytes = 0;
        state.accounted_messages = 0;
        queue.capacity_available.notify_all();
    }

    fn close(&self) {
        let mut state = self.queue.state.lock();
        if !state.closed {
            Self::close_locked(&self.queue, &mut state);
        }
    }
}

/// Name of the process group currently owning the session's PTY, i.e. the
/// command the user is waiting on. `None` while the shell itself is in the
/// foreground, so a pane header can fall back to showing the shell prompt.
///
/// Read from `/proc/<shell>/stat` rather than `tcgetpgrp`: the shell's own
/// entry names its controlling terminal's foreground group, so no PTY master
/// fd has to be threaded through the UI layer.
pub fn get_foreground_command(shell_pid: i32) -> Option<String> {
    let foreground_pgid = jterm_core::process::foreground_pgid_via_stat(shell_pid)?;
    if foreground_pgid == shell_pid {
        return None;
    }
    jterm_core::process::process_comm(foreground_pgid)
}

/// SessionManager - 管理所有终端会话
pub struct SessionManager {
    sessions: Vec<Session>,
    /// Kept index-aligned with `sessions`; handles already given to worker
    /// threads remain attached to the same session across tab reordering.
    protocol_responses: Vec<ProtocolResponseSender>,
    active_index: usize,
    repaint_ctx: egui::Context,
    configured_shell: Option<String>,
    /// 最近一次被切走的会话的稳定 ID。用于 SessionPrevActive
    /// (类似 Vim 的 Ctrl+^) 在两个 tab 间快速来回。存 session_id 而非
    /// index,避免增删/重排后索引漂移导致跳错。
    previous_session_id: Option<String>,
    /// Starting point for background-session output processing. Rotating this
    /// cursor prevents a noisy early tab from starving later hidden tabs.
    background_pump_cursor: usize,
}

#[derive(Debug, Default)]
pub struct BackgroundPumpResult {
    pub bytes_processed: usize,
    pub had_output: bool,
    pub has_more: bool,
    pub exited_indices: Vec<usize>,
    pub errors: Vec<(usize, String)>,
    pub clipboard_requests: Vec<(usize, Vec<ClipboardReadRequest>)>,
    pub osc52_writes: Vec<(usize, String)>,
    pub osc52_queries: Vec<usize>,
    pub notifications: Vec<(usize, String, String)>,
    /// Completed OSC 133 output snapshots. The caller forwards these to the
    /// asynchronous journal writer after all terminal locks have been dropped.
    pub completed_command_outputs: Vec<(usize, CompletedCommandOutput)>,
}

/// Retry one session's UI-accepted input as a single shell-writer message.
/// Success clears the exact FIFO, transient pressure preserves every byte, and
/// permanent failure clears bytes that can never be delivered again.
fn retry_pending_input(
    pending_input: &mut Vec<u8>,
    write: impl FnOnce(&[u8]) -> Result<(), ShellWriteError>,
) -> Result<bool, ShellWriteError> {
    if pending_input.is_empty() {
        return Ok(false);
    }
    match write(pending_input) {
        Ok(()) => {
            pending_input.clear();
            Ok(true)
        }
        Err(error) => {
            if !error.is_backpressure() {
                pending_input.clear();
            }
            Err(error)
        }
    }
}

pub(crate) fn user_input_is_blocked_by_mouse_edge(
    session_id: &str,
    barrier_session_id: Option<&str>,
) -> bool {
    barrier_session_id == Some(session_id)
}

fn restored_or_fresh_session_id(
    candidate: Option<String>,
    used_session_ids: &HashSet<String>,
) -> String {
    if let Some(id) = candidate
        .filter(|id| crate::session::is_valid_rsh_session_id(id) && !used_session_ids.contains(id))
    {
        return id;
    }
    loop {
        let id = crate::session::generate_session_id();
        if !used_session_ids.contains(&id) {
            return id;
        }
    }
}

impl SessionManager {
    /// 创建新的会话管理器，初始化一个默认会话
    pub fn new(
        first_session: Session,
        repaint_ctx: egui::Context,
        configured_shell: Option<String>,
    ) -> Self {
        let protocol_responses = vec![ProtocolResponseSender::new(repaint_ctx.clone())];
        SessionManager {
            sessions: vec![first_session],
            protocol_responses,
            active_index: 0,
            repaint_ctx,
            configured_shell,
            previous_session_id: None,
            background_pump_cursor: 0,
        }
    }

    /// Fairly parse PTY output for every non-active session within one shared
    /// byte budget. Visible split panes are considered first, then hidden tabs
    /// are rotated between frames. This keeps every visible pane live and
    /// prevents bounded PTY channels from permanently back-pressuring jobs.
    pub fn pump_inactive_sessions(
        &mut self,
        total_budget: usize,
        visible_session_indices: &[usize],
        user_input_barrier_session_id: Option<&str>,
    ) -> BackgroundPumpResult {
        let order = background_pump_order(
            self.active_index,
            self.sessions.len(),
            visible_session_indices,
            self.background_pump_cursor,
        );
        if self.sessions.len() > 1 {
            self.background_pump_cursor = (self.background_pump_cursor + 1) % self.sessions.len();
        }

        let mut result = BackgroundPumpResult::default();
        let mut remaining_budget = total_budget;
        let mut remaining_sessions = order.len();

        for session_idx in order {
            // Recompute the share after idle sessions so unused capacity flows
            // to busy sessions later in the order without exceeding the global
            // frame budget.
            let mut share = if remaining_budget == 0 {
                0
            } else {
                (remaining_budget / remaining_sessions.max(1)).max(1)
            };
            remaining_sessions = remaining_sessions.saturating_sub(1);
            let session = &mut self.sessions[session_idx];
            let protocol_responses = self.protocol_responses[session_idx].clone();
            if let Err(error) = protocol_responses.flush(&session.shell) {
                if !error.is_backpressure() {
                    result.errors.push((session_idx, error.to_string()));
                }
            }
            // Do not accept more PTY protocol requests while an older reply is
            // waiting for shell-writer capacity. This propagates bounded
            // backpressure to the PTY and protects the critical reply reserve.
            if protocol_responses.has_pending() {
                share = 0;
                result.has_more = true;
            } else if user_input_is_blocked_by_mouse_edge(
                &session.metadata.session_id,
                user_input_barrier_session_id,
            ) {
                // A press/release transition from an earlier frame still owns
                // this session's user-input ordering barrier. Continue
                // draining PTY output, but do not let newer keyboard/IME bytes
                // overtake that edge in its writer. Independent PTYs remain
                // usable while this route is backpressured.
                result.has_more |= !session.pending_input.is_empty();
            } else {
                let shell = &session.shell;
                match retry_pending_input(&mut session.pending_input, |bytes| shell.write(bytes)) {
                    Ok(_) => {}
                    Err(error) if error.is_backpressure() => result.has_more = true,
                    Err(error) => result.errors.push((session_idx, error.to_string())),
                }
            }
            if visible_session_indices.contains(&session_idx) {
                session.metadata.unseen_output = false;
            }
            let mut data = std::mem::take(&mut session.pending_output);
            let mut exited = false;

            if share > 0 && data.len() < share {
                loop {
                    match session.shell.events().try_recv() {
                        Ok(ShellEvent::Output(chunk)) => {
                            data.extend(chunk);
                            if data.len() >= share {
                                break;
                            }
                        }
                        Ok(ShellEvent::Exit(_code)) => {
                            exited = true;
                            break;
                        }
                        Ok(ShellEvent::Error(error)) => {
                            result.errors.push((session_idx, error));
                            break;
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            exited = true;
                            break;
                        }
                    }
                }
            }

            if share == 0 {
                session.pending_output = data;
                data = Vec::new();
                if !session.pending_output.is_empty() || !session.shell.events().is_empty() {
                    result.has_more = true;
                }
            } else if data.len() > share {
                session.pending_output = data.split_off(share);
            }

            if !data.is_empty() {
                result.had_output = true;
                result.bytes_processed += data.len();
                remaining_budget = remaining_budget.saturating_sub(data.len());
                // A split pane is already visible even when it is not the
                // focused session; only hidden tabs should gain an unread dot.
                if !visible_session_indices.contains(&session_idx) {
                    session.metadata.unseen_output = true;
                }
            }

            // Protocol timers and side effects must progress even in a silent
            // PTY. In particular, synchronized-output mode otherwise remains
            // frozen forever if an application dies after its begin marker.
            let mut terminal = session.terminal.lock();
            if !data.is_empty() {
                terminal.process_batch(&data);
            }
            terminal.check_sync_output_timeout();
            let response = terminal.get_output();
            if let Err((error, mut response)) = protocol_responses.try_enqueue(response) {
                // `get_output` drains. Restore the older reply before anything
                // appended concurrently/later so retry preserves byte order.
                if error == ProtocolResponseQueueError::Full {
                    response.append(&mut terminal.output_buffer);
                    terminal.output_buffer = response;
                    result.has_more = true;
                } else {
                    result.errors.push((session_idx, error.to_string()));
                }
            }
            let clipboard_requests = terminal.take_clipboard_read_requests();
            if !clipboard_requests.is_empty() {
                result
                    .clipboard_requests
                    .push((session_idx, clipboard_requests));
            }
            if let Some(text) = terminal.take_osc52_clipboard_set() {
                result.osc52_writes.push((session_idx, text));
            }
            if terminal.take_osc52_clipboard_query() {
                result.osc52_queries.push(session_idx);
            }
            for (title, body) in terminal.pending_notifications.drain(..) {
                result.notifications.push((session_idx, title, body));
            }
            result.completed_command_outputs.extend(
                terminal
                    .take_completed_command_outputs()
                    .into_iter()
                    .map(|completed| (session_idx, completed)),
            );
            drop(terminal);
            if let Err(error) = protocol_responses.flush(&session.shell) {
                if !error.is_backpressure() {
                    result.errors.push((session_idx, error.to_string()));
                }
            }

            if exited {
                result.exited_indices.push(session_idx);
            }
            if protocol_responses.has_pending()
                || !session.pending_output.is_empty()
                || !session.shell.events().is_empty()
            {
                result.has_more = true;
            }
        }

        result
    }

    /// 创建新会话并添加到当前活跃会话的右侧，继承当前工作目录
    pub fn new_session(
        &mut self,
        name: Option<String>,
        tags: Option<Vec<String>>,
        cols: usize,
        rows: usize,
        scrollback_lines: usize,
    ) -> usize {
        self.insert_session(name, tags, cols, rows, scrollback_lines, None)
    }

    /// 以显式 argv 打开一次性辅助会话(例如 rsh 安装脚本)，而不是交互 shell。
    /// 脚本自己打印进度，会话本身就是进度界面。
    pub fn new_command_session(
        &mut self,
        name: String,
        argv: Vec<String>,
        cols: usize,
        rows: usize,
        scrollback_lines: usize,
    ) -> usize {
        self.insert_session(Some(name), None, cols, rows, scrollback_lines, Some(argv))
    }

    fn insert_session(
        &mut self,
        name: Option<String>,
        tags: Option<Vec<String>>,
        cols: usize,
        rows: usize,
        scrollback_lines: usize,
        command_argv: Option<Vec<String>>,
    ) -> usize {
        let (cols, rows) = clamp_terminal_dimensions(cols, rows);
        let insert_index = self.active_index + 1;
        let name = name.unwrap_or_else(|| format!("Session {}", self.sessions.len() + 1));
        let tags = tags.unwrap_or_default();

        // 优先使用 shell 通过 OSC 7 报告的 cwd(SSH/tmux 等场景下 /proc 不能反
        // 映远端进程真实目录);否则退回 /proc/[pid]/cwd。
        let cwd = if !self.sessions.is_empty() {
            let active_session = &self.sessions[self.active_index];
            let osc7 = active_session.terminal.lock().current_working_dir.clone();
            osc7.or_else(|| jterm_core::process::process_cwd(active_session.get_shell_pid()))
        } else {
            None
        };

        // 在启动 shell 前分配稳定 ID；rsh 的 --session、tab 路由和执行
        // journal 必须从第一条输出起使用同一个值。
        let session_id = crate::session::generate_session_id();
        let cwd_ref = cwd.as_deref();
        match ShellSession::new_with_cwd(
            cols,
            rows,
            cwd_ref,
            Some(&session_id),
            self.configured_shell.as_deref(),
            command_argv.as_deref(),
            self.repaint_ctx.clone(),
        ) {
            Ok(shell) => {
                let mut terminal = TerminalState::new(cols, rows);
                terminal.set_max_scrollback(scrollback_lines);
                let terminal = Arc::new(ParkingMutex::new(terminal));
                let session = Session::new_with_session_id(name, tags, terminal, shell, session_id);
                self.sessions.insert(insert_index, session);
                self.protocol_responses.insert(
                    insert_index,
                    ProtocolResponseSender::new(self.repaint_ctx.clone()),
                );
                insert_index
            }
            Err(e) => {
                eprintln!("Failed to create new session: {}", e);
                self.active_index
            }
        }
    }

    /// 关闭指定会话
    pub fn close_session(&mut self, index: usize) -> bool {
        if index >= self.sessions.len() {
            return false;
        }

        if self.sessions.len() == 1 {
            // 不允许关闭最后一个会话
            return false;
        }

        self.sessions.remove(index);
        let responses = self.protocol_responses.remove(index);
        responses.close();

        // 调整活跃会话索引:
        // - 关闭的是活跃会话之前的会话:活跃会话整体左移一位,索引需 -1 才能继续指向同一会话。
        // - 关闭的就是活跃会话:索引保持不变,自然指向原先的下一个会话(下方再做越界钳制)。
        if index < self.active_index {
            self.active_index -= 1;
        }
        if self.active_index >= self.sessions.len() {
            self.active_index = self.sessions.len() - 1;
        }

        true
    }

    /// 切换到指定会话
    pub fn switch_session(&mut self, index: usize) -> bool {
        if index < self.sessions.len() {
            // 仅在真正切走时记录前一个会话的稳定 ID,供 SessionPrevActive 反跳。
            // 跳同一个 tab 不算切换,否则 Ctrl+` 反跳会失去意义。
            if index != self.active_index {
                if let Some(prev) = self.sessions.get(self.active_index) {
                    self.previous_session_id = Some(prev.metadata.session_id.clone());
                }
            }
            self.active_index = index;
            if let Some(session) = self.sessions.get_mut(index) {
                session.metadata.update_last_active();
                // 切到该会话即视为"已查看",清掉活动指示点。
                session.metadata.unseen_output = false;
            }
            true
        } else {
            false
        }
    }

    /// 跳到最近一次被切走的会话(若仍存在)。返回是否成功跳转。
    pub fn switch_to_previous_active(&mut self) -> bool {
        let Some(prev_id) = self.previous_session_id.clone() else {
            return false;
        };
        let target = self
            .sessions
            .iter()
            .position(|s| s.metadata.session_id == prev_id);
        match target {
            Some(idx) if idx != self.active_index => self.switch_session(idx),
            _ => false,
        }
    }

    /// 扫描所有后台会话:若其 shell 事件通道有未消费数据,标记 unseen_output。
    /// 主循环每帧只 drain active session,这里用通道非空作为"后台有产出"的代理。
    pub fn refresh_unseen_flags(&mut self, visible_session_indices: &[usize]) {
        let active = self.active_index;
        for (i, s) in self.sessions.iter_mut().enumerate() {
            s.metadata.unseen_output = refreshed_unseen_output(
                s.metadata.unseen_output,
                i == active,
                visible_session_indices.contains(&i),
                !s.shell.events().is_empty(),
            );
        }
    }

    /// 切换到下一个会话
    pub fn switch_to_next_session(&mut self) -> usize {
        let next = (self.active_index + 1) % self.sessions.len();
        self.switch_session(next);
        next
    }

    /// 切换到前一个会话
    pub fn switch_to_prev_session(&mut self) -> usize {
        let previous = if self.active_index == 0 {
            self.sessions.len() - 1
        } else {
            self.active_index - 1
        };
        self.switch_session(previous);
        previous
    }

    /// 获取当前活跃会话的索引
    pub fn active_index(&self) -> usize {
        self.active_index
    }

    /// Update the shell override used for subsequently-created sessions.
    /// Existing PTYs keep running unchanged.
    pub fn set_configured_shell(&mut self, shell: Option<String>) {
        self.configured_shell = shell;
    }

    /// 获取当前活跃会话（可变引用）
    pub fn get_active_session_mut(&mut self) -> &mut Session {
        &mut self.sessions[self.active_index]
    }

    /// 获取指定索引的会话（可变引用）
    pub fn get_session_mut(&mut self, index: usize) -> Option<&mut Session> {
        self.sessions.get_mut(index)
    }

    pub fn protocol_response_sender(&self, index: usize) -> Option<ProtocolResponseSender> {
        self.protocol_responses.get(index).cloned()
    }

    pub fn flush_protocol_responses(&self, index: usize) -> Option<Result<usize, ShellWriteError>> {
        let sender = self.protocol_responses.get(index)?;
        let session = self.sessions.get(index)?;
        Some(sender.flush(&session.shell))
    }

    /// 获取所有会话的不可变引用
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// 获取所有会话的可变引用
    pub fn sessions_mut(&mut self) -> &mut [Session] {
        &mut self.sessions
    }

    /// 会话总数（始终 ≥ 1，不存在空状态，故无 is_empty）
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// 用稳定 session ID 反查当前索引。跨帧保存的引用（拖拽、待确认粘贴）
    /// 必须走这里：索引会因关闭/重排而漂移。
    pub fn index_of(&self, session_id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.metadata.session_id == session_id)
    }

    /// 重排会话顺序（拖拽）
    pub fn reorder_sessions(&mut self, from_idx: usize, to_idx: usize) {
        if from_idx < self.sessions.len() && to_idx < self.sessions.len() && from_idx != to_idx {
            let session = self.sessions.remove(from_idx);
            self.sessions.insert(to_idx, session);
            let responses = self.protocol_responses.remove(from_idx);
            self.protocol_responses.insert(to_idx, responses);

            // 如果移动的是活跃会话，更新active_index
            if self.active_index == from_idx {
                self.active_index = to_idx;
            } else if from_idx < self.active_index && to_idx >= self.active_index {
                // 从左边移到右边，active_index向左移动
                self.active_index -= 1;
            } else if from_idx > self.active_index && to_idx <= self.active_index {
                // 从右边移到左边，active_index向右移动
                self.active_index += 1;
            }
        }
    }

    /// 获取会话列表的快照用于持久化（包含 cwd）
    pub fn get_session_snapshots(&self) -> Vec<session_persistence::SessionSnapshot> {
        self.sessions
            .iter()
            .map(|s| {
                // OSC 7 is authoritative for remote shells and multiplexers;
                // /proc only describes the local wrapper process in those
                // cases. Match new-tab inheritance so restore returns to the
                // directory the user actually saw.
                let cwd = s
                    .terminal
                    .lock()
                    .current_working_dir
                    .clone()
                    .or_else(|| jterm_core::process::process_cwd(s.get_shell_pid()));
                session_persistence::SessionSnapshot {
                    name: s.metadata.name.clone(),
                    tags: s.metadata.tags.clone(),
                    cwd,
                    session_id: Some(s.metadata.session_id.clone()),
                    custom_name: s.metadata.custom_name.clone(),
                }
            })
            .collect()
    }

    /// 从快照恢复额外的会话（第一个已经在外部创建好）
    pub fn restore_from_snapshots(
        &mut self,
        snapshots: Vec<session_persistence::SessionSnapshot>,
        active_index: Option<usize>,
    ) {
        let mut restored_indices = vec![None; snapshots.len()];
        // 第一个 shell 的 ID 已在 spawn 前从同一快照规范化并固定；这里
        // 只恢复展示元数据，绝不能再用损坏的磁盘值改写跨进程路由键。
        if let Some(first) = snapshots.first() {
            if let Some(session) = self.sessions.get_mut(0) {
                session.metadata.name = first.name.clone();
                session.metadata.tags = first.tags.clone();
                session.metadata.custom_name = first.custom_name.clone();
                restored_indices[0] = Some(0);
            }
        }

        let mut used_session_ids = self
            .sessions
            .iter()
            .map(|session| session.metadata.session_id.clone())
            .collect::<HashSet<_>>();
        // 为剩余快照创建新会话
        for (snapshot_idx, snap) in snapshots.into_iter().enumerate().skip(1) {
            let session_persistence::SessionSnapshot {
                name,
                tags,
                cwd,
                session_id,
                custom_name,
            } = snap;
            let session_id = restored_or_fresh_session_id(session_id, &used_session_ids);
            used_session_ids.insert(session_id.clone());
            let cwd_ref = cwd.as_deref();
            let mut shell_result = ShellSession::new_with_cwd(
                80,
                24,
                cwd_ref,
                Some(&session_id),
                self.configured_shell.as_deref(),
                None,
                self.repaint_ctx.clone(),
            );
            if let Err(error) = &shell_result {
                if cwd_ref.is_some() {
                    eprintln!(
                        "Failed to restore session in saved cwd ({error}); retrying in default cwd"
                    );
                    shell_result = ShellSession::new_with_cwd(
                        80,
                        24,
                        None,
                        Some(&session_id),
                        self.configured_shell.as_deref(),
                        None,
                        self.repaint_ctx.clone(),
                    );
                }
            }
            match shell_result {
                Ok(shell) => {
                    let terminal = Arc::new(ParkingMutex::new(TerminalState::new(80, 24)));
                    let mut session =
                        Session::new_with_session_id(name, tags, terminal, shell, session_id);
                    session.metadata.custom_name = custom_name;
                    self.sessions.push(session);
                    self.protocol_responses
                        .push(ProtocolResponseSender::new(self.repaint_ctx.clone()));
                    restored_indices[snapshot_idx] = Some(self.sessions.len() - 1);
                }
                Err(e) => {
                    eprintln!("Failed to restore session: {}", e);
                }
            }
        }

        // 恢复活跃标签页
        if let Some(idx) = active_index.and_then(|idx| {
            restored_indices
                .get(idx)
                .and_then(|restored_idx| *restored_idx)
        }) {
            self.active_index = idx;
        }
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        for sender in &self.protocol_responses {
            sender.close();
        }
    }
}

fn background_pump_order(
    active_index: usize,
    session_count: usize,
    visible_session_indices: &[usize],
    cursor: usize,
) -> Vec<usize> {
    let mut order = Vec::with_capacity(session_count.saturating_sub(1));

    for &idx in visible_session_indices {
        if idx < session_count && idx != active_index && !order.contains(&idx) {
            order.push(idx);
        }
    }

    if session_count > 1 {
        let start = cursor % session_count;
        for offset in 0..session_count {
            let idx = (start + offset) % session_count;
            if idx != active_index && !order.contains(&idx) {
                order.push(idx);
            }
        }
    }

    order
}

fn refreshed_unseen_output(
    current: bool,
    is_active: bool,
    is_visible: bool,
    has_pending_events: bool,
) -> bool {
    if is_active || is_visible {
        false
    } else {
        current || has_pending_events
    }
}

#[cfg(test)]
mod tests {
    use super::{
        background_pump_order, refreshed_unseen_output, restored_or_fresh_session_id,
        retry_pending_input, user_input_is_blocked_by_mouse_edge, ProtocolResponseLimits,
        ProtocolResponseQueueError, ProtocolResponseSender,
    };
    use crate::shell::ShellWriteError;
    use std::collections::HashSet;
    use std::time::Duration;

    fn tiny_protocol_limits() -> ProtocolResponseLimits {
        ProtocolResponseLimits {
            byte_capacity: 10,
            message_capacity: 4,
            max_message_bytes: 8,
            critical_reserve_bytes: 2,
            critical_reserve_messages: 1,
        }
    }

    #[test]
    fn visible_background_panes_are_pumped_first_without_duplicates() {
        assert_eq!(background_pump_order(0, 4, &[2, 2, 0], 1), vec![2, 1, 3]);
    }

    #[test]
    fn hidden_background_order_rotates_between_frames() {
        assert_eq!(background_pump_order(1, 4, &[], 0), vec![0, 2, 3]);
        assert_eq!(background_pump_order(1, 4, &[], 2), vec![2, 3, 0]);
    }

    #[test]
    fn visible_split_panes_never_show_an_unread_indicator() {
        assert!(!refreshed_unseen_output(true, false, true, true));
        assert!(!refreshed_unseen_output(false, false, true, true));
        assert!(refreshed_unseen_output(false, false, false, true));
        assert!(refreshed_unseen_output(true, false, false, false));
    }

    #[test]
    fn pending_input_retry_is_atomic_across_success_and_failure() {
        let original = b"\x1b[200~hello\x1b[201~".to_vec();
        let mut pending = original.clone();
        let full = ShellWriteError::Full {
            requested_bytes: pending.len(),
            queued_bytes: 10,
            byte_capacity: 10,
            queued_messages: 1,
            message_capacity: 1,
        };
        assert!(matches!(
            retry_pending_input(&mut pending, |_| Err(full.clone())),
            Err(ShellWriteError::Full { .. })
        ));
        assert_eq!(pending, original);

        let mut accepted = Vec::new();
        assert_eq!(
            retry_pending_input(&mut pending, |bytes| {
                accepted.extend_from_slice(bytes);
                Ok(())
            }),
            Ok(true)
        );
        assert_eq!(accepted, original);
        assert!(pending.is_empty());

        pending.extend_from_slice(b"cannot-deliver");
        assert_eq!(
            retry_pending_input(&mut pending, |_| Err(ShellWriteError::Closed)),
            Err(ShellWriteError::Closed)
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn mouse_edge_barrier_blocks_only_its_stable_session_route() {
        assert!(user_input_is_blocked_by_mouse_edge(
            "captured-session",
            Some("captured-session")
        ));
        assert!(!user_input_is_blocked_by_mouse_edge(
            "independent-session",
            Some("captured-session")
        ));
        assert!(!user_input_is_blocked_by_mouse_edge(
            "captured-session",
            None
        ));
    }

    #[test]
    fn restored_session_ids_are_valid_and_unique() {
        let used = HashSet::from(["already-used".to_owned()]);
        assert_eq!(
            restored_or_fresh_session_id(Some("saved-session".to_owned()), &used),
            "saved-session"
        );
        for candidate in [
            Some("already-used".to_owned()),
            Some("../bad".to_owned()),
            None,
        ] {
            let generated = restored_or_fresh_session_id(candidate, &used);
            assert!(crate::session::is_valid_rsh_session_id(&generated));
            assert!(!used.contains(&generated));
        }
    }

    #[test]
    fn protocol_queue_reserves_control_capacity_without_reordering_replies() {
        let sender = ProtocolResponseSender::new_with_limits(
            egui::Context::default(),
            tiny_protocol_limits(),
        );

        sender.try_enqueue(vec![1; 8]).unwrap();
        let (error, rejected) = sender.try_enqueue(vec![3; 2]).unwrap_err();
        assert_eq!(error, ProtocolResponseQueueError::Full);
        assert_eq!(rejected, vec![3; 2]);
        sender.try_enqueue_critical(vec![2; 2]).unwrap();

        let state = sender.queue.state.lock();
        assert_eq!(state.accounted_bytes, 10);
        assert_eq!(state.accounted_messages, 2);
        assert_eq!(state.pending.front().map(Vec::as_slice), Some(&[1; 8][..]));
        assert_eq!(state.pending.get(1).map(Vec::as_slice), Some(&[2, 2][..]));
    }

    #[test]
    fn closing_protocol_queue_wakes_a_bounded_worker_waiter() {
        let sender = ProtocolResponseSender::new_with_limits(
            egui::Context::default(),
            tiny_protocol_limits(),
        );
        sender.try_enqueue(vec![1; 8]).unwrap();
        sender.try_enqueue_critical(vec![2; 2]).unwrap();

        let waiting_sender = sender.clone();
        let waiter = std::thread::spawn(move || waiting_sender.enqueue_blocking(vec![3]));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!waiter.is_finished());

        sender.close();
        assert_eq!(
            waiter.join().unwrap(),
            Err(ProtocolResponseQueueError::Closed)
        );
    }

    #[test]
    fn blocking_protocol_enqueue_retries_exact_bytes_without_reordering() {
        let sender = ProtocolResponseSender::new_with_limits(
            egui::Context::default(),
            tiny_protocol_limits(),
        );
        sender.try_enqueue(vec![1; 8]).unwrap();
        sender.try_enqueue_critical(vec![2; 2]).unwrap();

        let waiting_sender = sender.clone();
        let waiter = std::thread::spawn(move || waiting_sender.enqueue_blocking(vec![3]));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!waiter.is_finished());

        {
            let mut state = sender.queue.state.lock();
            let flushed = state.pending.pop_front().unwrap();
            state.accounted_bytes -= flushed.len();
            state.accounted_messages -= 1;
            sender.queue.capacity_available.notify_all();
        }
        waiter.join().unwrap().unwrap();

        let state = sender.queue.state.lock();
        assert_eq!(state.pending.len(), 2);
        assert_eq!(state.pending.front().map(Vec::as_slice), Some(&[2, 2][..]));
        assert_eq!(state.pending.get(1).map(Vec::as_slice), Some(&[3][..]));
    }
}
