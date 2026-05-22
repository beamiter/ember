use crate::pty::Pty;
use crate::terminal::clamp_terminal_dimensions;
use crossbeam::channel::{unbounded, Receiver};
use eframe::egui;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum ShellEvent {
    Output(Vec<u8>),
    Exit(i32),
    Error(String),
}

/// ShellSession 管理 PTY 和后台 I/O 线程
pub struct ShellSession {
    pty: Arc<Mutex<Pty>>,
    event_rx: Receiver<ShellEvent>,
    child_pid: i32,            // 存储 shell 子进程的 PID
    shutdown: Arc<AtomicBool>, // 通知 IO 线程退出
}

impl ShellSession {
    /// 启动新的 shell session
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

                let (event_tx, event_rx) = unbounded::<ShellEvent>();
                let shutdown = Arc::new(AtomicBool::new(false));

                let pty = Arc::new(Mutex::new(pty));
                let pty_clone = Arc::clone(&pty);
                let repaint_ctx_clone = repaint_ctx.clone();
                let shutdown_clone = Arc::clone(&shutdown);

                thread::spawn(move || {
                    Self::io_loop(pty_clone, event_tx, repaint_ctx_clone, shutdown_clone);
                });

                Ok(ShellSession {
                    pty,
                    event_rx,
                    child_pid,
                    shutdown,
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
        const BUFFER_SIZE: usize = 65536; // 64KB 读缓冲
        const BATCH_SIZE_THRESHOLD: usize = 131072; // 128KB 累积阈值
        const BATCH_TIMEOUT_MS: u64 = 2; // 2ms 累积超时

        let mut buf = vec![0u8; BUFFER_SIZE];
        let mut accumulated = Vec::with_capacity(BATCH_SIZE_THRESHOLD);
        let mut last_alive_check = std::time::Instant::now();
        let mut last_batch_time = std::time::Instant::now();

        crate::debug_log!("[IOLoop] 后台 I/O 线程启动 (P3 批处理优化)");

        loop {
            // 检查 shutdown 标志
            if shutdown.load(Ordering::Relaxed) {
                crate::debug_log!("[IOLoop] 收到 shutdown 信号，退出 IO 线程");
                return;
            }

            // 动态计算超时：累积数据时快速超时，空闲时正常超时
            let timeout_ms = if !accumulated.is_empty() {
                // 已有累积数据，快速超时以便发送
                let elapsed_ms = last_batch_time.elapsed().as_millis() as i32;
                let remaining_ms = BATCH_TIMEOUT_MS as i32 - elapsed_ms;
                remaining_ms.max(1).min(100)
            } else {
                // 无累积数据，使用标准超时（减去 alive_check 耗时）
                (100_i32)
                    .saturating_sub(last_alive_check.elapsed().as_millis() as i32)
                    .max(1)
            };

            let master_fd = match pty.lock() {
                Ok(pty_guard) => pty_guard.master_fd(),
                Err(_) => {
                    let _ = Self::send_event(
                        &event_tx,
                        &repaint_ctx,
                        ShellEvent::Error("PTY lock poisoned".to_string()),
                    );
                    return;
                }
            };

            match Pty::wait_fd_readable(master_fd, timeout_ms) {
                Ok(true) => {
                    // PTY 可读，读取数据
                    if let Ok(mut pty_guard) = pty.lock() {
                        // 非阻塞读取所有可用数据
                        loop {
                            match pty_guard.read(&mut buf) {
                                Ok(n) if n > 0 => {
                                    accumulated.extend_from_slice(&buf[..n]);
                                    // 数据足够时立即发送，避免内存爆炸
                                    if accumulated.len() >= BATCH_SIZE_THRESHOLD {
                                        let data = std::mem::take(&mut accumulated);
                                        if !Self::send_event(
                                            &event_tx,
                                            &repaint_ctx,
                                            ShellEvent::Output(data),
                                        ) {
                                            crate::debug_log!("[IOLoop] 接收者已断开，退出循环");
                                            return;
                                        }
                                        last_batch_time = std::time::Instant::now();
                                    }
                                }
                                Ok(_) => break, // EOF 或无更多数据
                                Err(e) => {
                                    crate::debug_log!("[IOLoop] 读取错误: {}", e);
                                    if !pty_guard.is_alive() {
                                        // 进程已退出，发送积累的数据和退出事件
                                        if !accumulated.is_empty() {
                                            let data = std::mem::take(&mut accumulated);
                                            let _ = Self::send_event(
                                                &event_tx,
                                                &repaint_ctx,
                                                ShellEvent::Output(data),
                                            );
                                        }
                                        match pty_guard.wait_timeout(0) {
                                            Ok(exit_code) => {
                                                crate::debug_log!(
                                                    "[IOLoop] 发送 Exit 事件，exit_code={}",
                                                    exit_code
                                                );
                                                let result = Self::send_event(
                                                    &event_tx,
                                                    &repaint_ctx,
                                                    ShellEvent::Exit(exit_code),
                                                );
                                                crate::debug_log!(
                                                    "[IOLoop] Exit 事件发送结果: {}",
                                                    result
                                                );
                                            }
                                            Err(e) => {
                                                let _ = Self::send_event(
                                                    &event_tx,
                                                    &repaint_ctx,
                                                    ShellEvent::Error(format!(
                                                        "Process exit error: {}",
                                                        e
                                                    )),
                                                );
                                            }
                                        }
                                        return;
                                    }
                                    let _ = Self::send_event(
                                        &event_tx,
                                        &repaint_ctx,
                                        ShellEvent::Error(format!("Read error: {}", e)),
                                    );
                                    break;
                                }
                            }
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

            // 检查进程是否存活（100ms 检查一次）
            if last_alive_check.elapsed() >= Duration::from_millis(100) {
                if let Ok(mut pty_guard) = pty.lock() {
                    if !pty_guard.is_alive() {
                        crate::debug_log!("[IOLoop] 检测到子进程已退出");
                        // 发送积累的数据
                        if !accumulated.is_empty() {
                            let data = std::mem::take(&mut accumulated);
                            let _ =
                                Self::send_event(&event_tx, &repaint_ctx, ShellEvent::Output(data));
                        }
                        match pty_guard.wait_timeout(0) {
                            Ok(exit_code) => {
                                crate::debug_log!("[IOLoop] 子进程退出码: {}", exit_code);
                                let result = Self::send_event(
                                    &event_tx,
                                    &repaint_ctx,
                                    ShellEvent::Exit(exit_code),
                                );
                                crate::debug_log!("[IOLoop] Exit 事件发送结果: {}", result);
                            }
                            Err(e) => {
                                let _ = Self::send_event(
                                    &event_tx,
                                    &repaint_ctx,
                                    ShellEvent::Error(format!("Process exit error: {}", e)),
                                );
                            }
                        }
                        return;
                    }
                    last_alive_check = std::time::Instant::now();
                } else {
                    let _ = Self::send_event(
                        &event_tx,
                        &repaint_ctx,
                        ShellEvent::Error("PTY lock poisoned".to_string()),
                    );
                    return;
                }
            }
        }
    }

    /// 处理大数据写入：循环写入并在 poll 等待时释放锁，避免与 io_loop 死锁
    pub(crate) fn write_to_pty(
        pty: &Arc<Mutex<Pty>>,
        data: &[u8],
    ) -> std::result::Result<(), String> {
        let mut offset = 0;
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(10);

        // 先获取 master_fd（不需要长期持锁）
        let master_fd = {
            let pty = pty
                .lock()
                .map_err(|_| "Failed to lock PTY for fd".to_string())?;
            pty.master_fd()
        };

        while offset < data.len() {
            if start.elapsed() > timeout {
                return Err(format!(
                    "PTY write timed out, wrote {}/{} bytes",
                    offset,
                    data.len()
                ));
            }

            // 获取锁，尝试写入，然后立即释放锁
            {
                let mut pty = pty
                    .lock()
                    .map_err(|_| "Failed to lock PTY for write".to_string())?;
                match pty.write(&data[offset..]) {
                    Ok(n) if n > 0 => {
                        offset += n;
                        continue; // 写成功，立刻尝试写更多
                    }
                    Ok(_) => break, // wrote 0
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("Resource temporarily unavailable")
                            || msg.contains("WouldBlock")
                            || msg.contains("EAGAIN")
                        {
                            // 缓冲区满，需要 poll 等待 — 先释放锁（落到下面）
                        } else {
                            return Err(format!("Write error: {}", e));
                        }
                    }
                }
            }
            // 锁已释放！io_loop 可以读取 PTY 输出，vim 可以排空缓冲区
            // poll 等待 PTY 可写
            // SAFETY: pfd 是有效的栈变量，符合 libc::pollfd 布局。
            // poll 调用使用有效的 fd 和超时值，不会导致未定义行为。
            unsafe {
                let mut pfd = libc::pollfd {
                    fd: master_fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                libc::poll(&mut pfd, 1, 50); // 50ms
            }
        }
        Ok(())
    }

    /// 向 shell 发送输入数据（例如用户输入）
    pub fn write(&self, data: &[u8]) -> std::result::Result<(), String> {
        Self::write_to_pty(&self.pty, data)
    }

    /// 在后台线程发送大块数据，避免阻塞 UI 更新循环。
    pub fn write_async(&self, data: Vec<u8>) {
        let pty = Arc::clone(&self.pty);
        let _ = thread::Builder::new()
            .name("pty-async-writer".to_string())
            .spawn(move || {
                if let Err(e) = Self::write_to_pty(&pty, &data) {
                    eprintln!("[ERROR] Failed to write to PTY asynchronously: {}", e);
                }
            });
    }

    pub fn resize(&self, cols: usize, rows: usize) -> std::result::Result<(), String> {
        let mut pty = self
            .pty
            .lock()
            .map_err(|_| "Failed to lock PTY for resize".to_string())?;
        pty.resize(cols, rows)
            .map_err(|e| format!("Resize error: {}", e))
    }

    /// 获取 shell 子进程的 PID
    pub fn get_child_pid(&self) -> i32 {
        self.child_pid
    }

    /// 获取 PTY master fd（用于 tcgetpgrp 等系统调用）
    pub fn get_master_fd(&self) -> Option<RawFd> {
        self.pty.lock().ok().map(|pty| pty.master_fd())
    }

    /// 获取可克隆的 PTY writer（用于延迟写入命令）
    pub fn pty_writer(&self) -> Arc<Mutex<Pty>> {
        Arc::clone(&self.pty)
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
        // 通知 IO 线程退出
        self.shutdown.store(true, Ordering::Relaxed);

        // 直接杀死整个进程组，确保 shell 及其子进程都被清理
        // SAFETY: kill 向进程/进程组发送信号。负 PID 向进程组发送信号是标准做法。
        // child_pid 来自 fork 创建的有效进程。即使进程已退出，kill 也是安全的。
        unsafe {
            let pgid = -self.child_pid;
            // SIGHUP: 通知shell会话终止
            libc::kill(pgid, libc::SIGHUP);
            // SIGTERM: 请求优雅退出
            libc::kill(self.child_pid, libc::SIGTERM);

            // 短暂等待后强制杀死
            std::thread::sleep(std::time::Duration::from_millis(30));

            // 强制杀死残留进程
            let _ = libc::kill(pgid, libc::SIGKILL);
            let _ = libc::kill(self.child_pid, libc::SIGKILL);

            // 回收僵尸进程
            let mut status = 0;
            let _ = libc::waitpid(self.child_pid, &mut status, libc::WNOHANG);
        }
    }
}
