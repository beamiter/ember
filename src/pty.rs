use anyhow::{anyhow, Result};
use std::ffi::CString;
use std::os::unix::io::RawFd;

const TERM_PROGRAM_NAME: &str = "jterm2";
const TERM_PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");
const VTE_VERSION: &str = "7802";

/// PTY 读取结果。必须区分 EOF 与 WouldBlock:EOF 表示从端已关闭
/// (子进程退出),读循环应停止;WouldBlock 表示暂无数据,应继续 poll 等待。
/// 二者混为 Ok(0) 会导致 EOF 后忙等,直到下次存活检查才退出 —— 期间 CPU 跑满。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    Data(usize),
    WouldBlock,
    Eof,
}

/// PTY 写入结果。区分"写入了 n 字节(可能少于请求,即 partial write)"与
/// "缓冲区已满(WouldBlock,需 poll 等待可写)"。EINTR 在内部重试,不外泄。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written(usize),
    WouldBlock,
}

#[cfg(unix)]
mod unix_pty {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// 异步信号安全地向 stderr 写一条静态消息(fork 后、execve 前只能用此类调用)。
    /// SAFETY: 仅调用 write(2),它在 POSIX 异步信号安全函数列表中。
    unsafe fn write_stderr(msg: &[u8]) {
        let _ = libc::write(
            libc::STDERR_FILENO,
            msg.as_ptr() as *const libc::c_void,
            msg.len(),
        );
    }

    fn is_executable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
            .unwrap_or(false)
    }

    fn find_executable_in_path(exe_name: &str) -> Option<String> {
        let path_var = std::env::var_os("PATH")?;
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(exe_name))
            .find(|candidate| is_executable(candidate))
            .map(|p| p.to_string_lossy().to_string())
    }

    pub(crate) fn shell_single_quote(s: &str) -> String {
        let mut quoted = String::with_capacity(s.len() + 2);
        quoted.push('\'');
        for ch in s.chars() {
            if ch == '\'' {
                quoted.push_str("'\"'\"'");
            } else {
                quoted.push(ch);
            }
        }
        quoted.push('\'');
        quoted
    }

    fn choose_shell(configured_shell: Option<&str>) -> String {
        // Priority 1: explicit config / env var (needed when PATH is stripped by launchers like wofi)
        if let Some(path) = configured_shell {
            if is_executable(Path::new(path)) {
                return path.to_string();
            }
            eprintln!(
                "[PTY] Configured shell '{}' is not executable, falling back",
                path
            );
        }

        // Priority 2: rsh (preferred shell with advanced features)
        if let Some(rsh_path) = find_executable_in_path("rsh") {
            return rsh_path;
        }

        // Priority 3: bash (fallback)
        if let Some(bash_path) = find_executable_in_path("bash") {
            return bash_path;
        }

        // Priority 4: sh (last resort)
        "sh".to_string()
    }

    pub struct Pty {
        master: RawFd,
        child_pid: i32,
        exit_code_cached: Option<i32>,
    }

    impl Pty {
        pub fn new_with_cwd(
            cols: usize,
            rows: usize,
            cwd: Option<&str>,
            session_id: Option<&str>,
            configured_shell: Option<&str>,
        ) -> Result<Self> {
            // SAFETY: 这个 unsafe 块包含多个 libc 系统调用用于 PTY 创建和进程 fork。
            // 所有的 libc 调用都检查了返回值并正确处理错误。
            // 文件描述符的生命周期被正确管理（成功时存储在 PtySession 中，失败时关闭）。
            // fork 后的子进程分支永不返回（通过 execve 或 exit），避免了未定义行为。
            unsafe {
                // 1. 创建 PTY
                let mut master = 0;
                let mut slave = 0;

                let win_size = libc::winsize {
                    ws_row: rows as u16,
                    ws_col: cols as u16,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };

                if libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &win_size,
                ) != 0
                {
                    return Err(anyhow!("Failed to open PTY"));
                }

                // 2. 设置 master 非阻塞模式
                let flags = libc::fcntl(master, libc::F_GETFL, 0);
                if flags >= 0 {
                    let _ = libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
                }

                // 设置 FD_CLOEXEC，防止子进程继承
                let fd_flags = libc::fcntl(master, libc::F_GETFD, 0);
                if fd_flags >= 0 {
                    let _ = libc::fcntl(master, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC);
                }

                // 3. fork 之前完成所有分配、加锁、PATH 解析与环境构建。
                // 原因:在多线程进程中 fork 后,子进程直到 execve 之间只能调用
                // 异步信号安全的函数。malloc/CString/format!/Vec/setenv/std::env/std::fs
                // 都不安全 —— 若 fork 时另一线程恰好持有 malloc 锁,子进程会永久死锁。
                // 因此这里预先构建 argv、envp、cwd 的 C 字符串,子进程分支只做 syscall。

                // 选择 shell(读取 env/fs,必须在 fork 前)
                let shell_path = choose_shell(configured_shell);
                let shell_cstr = CString::new(shell_path.clone())
                    .map_err(|_| anyhow!("Invalid shell path: {}", shell_path))?;
                let shell_name = Path::new(&shell_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("sh")
                    .to_string();

                // argv[0] 前缀 "-" 表示登录 shell
                let dash_shell_cstr = CString::new(format!("-{}", shell_name))
                    .map_err(|_| anyhow!("Invalid shell name"))?;
                let login_arg = if shell_name == "bash" {
                    Some(CString::new("-l").unwrap())
                } else {
                    None
                };
                let session_flag = CString::new("--session").unwrap();
                let session_id_cstr = session_id.and_then(|s| CString::new(s).ok());

                let bash_path = if shell_name == "rsh" {
                    find_executable_in_path("bash").filter(|p| is_executable(Path::new(p)))
                } else {
                    None
                };

                let (exec_cstr, argv_cstrings): (CString, Vec<CString>) =
                    if shell_name == "rsh" && bash_path.is_some() {
                        let bash_path = bash_path.unwrap();
                        let mut exec_cmd = format!("exec {}", shell_single_quote(&shell_path));
                        if let Some(sid) = session_id {
                            exec_cmd.push_str(" --session ");
                            exec_cmd.push_str(&shell_single_quote(sid));
                        }
                        (
                            CString::new(bash_path)
                                .map_err(|_| anyhow!("Invalid bash path"))?,
                            vec![
                                CString::new("bash").unwrap(),
                                CString::new("-ic").unwrap(),
                                CString::new(exec_cmd)
                                    .map_err(|_| anyhow!("Invalid wrapped shell command"))?,
                            ],
                        )
                    } else {
                        let mut argv = vec![dash_shell_cstr.clone()];
                        if let Some(ref arg) = login_arg {
                            argv.push(arg.clone());
                        }
                        if shell_name == "rsh" {
                            if let Some(ref sid) = session_id_cstr {
                                argv.push(session_flag.clone());
                                argv.push(sid.clone());
                            }
                        }
                        (shell_cstr.clone(), argv)
                    };

                let mut argv_ptrs: Vec<*const libc::c_char> =
                    argv_cstrings.iter().map(|arg| arg.as_ptr()).collect();
                argv_ptrs.push(std::ptr::null());

                // 工作目录的 C 字符串(若指定)
                let cwd_cstr = match cwd {
                    Some(dir) => {
                        Some(CString::new(dir).map_err(|_| anyhow!("Invalid working directory"))?)
                    }
                    None => None,
                };

                // 构建子进程环境:继承父进程环境,覆盖终端相关变量(避免在子进程调用
                // 非异步信号安全的 setenv)。直接把构建好的 envp 传给 execve。
                const OVERRIDDEN: [&[u8]; 5] = [
                    b"TERM",
                    b"COLORTERM",
                    b"TERM_PROGRAM",
                    b"TERM_PROGRAM_VERSION",
                    b"VTE_VERSION",
                ];
                const LOCALE_KEYS: [&[u8]; 3] = [b"LANG", b"LC_ALL", b"LC_CTYPE"];
                let has_utf8_locale = std::env::vars_os().any(|(k, v)| {
                    let k_bytes = k.as_bytes();
                    if !LOCALE_KEYS.iter().any(|lk| *lk == k_bytes) {
                        return false;
                    }
                    let value = v.to_string_lossy().to_ascii_lowercase();
                    value.contains("utf-8") || value.contains("utf8")
                });
                let mut env_cstrings: Vec<CString> = Vec::new();
                let mut has_less = false;
                for (k, v) in std::env::vars_os() {
                    let k_bytes = k.as_bytes();
                    if k_bytes == b"LESS" {
                        has_less = true;
                    }
                    if OVERRIDDEN.iter().any(|ok| *ok == k_bytes) {
                        continue;
                    }
                    if !has_utf8_locale && LOCALE_KEYS.iter().any(|lk| *lk == k_bytes) {
                        continue;
                    }
                    let mut entry = Vec::with_capacity(k_bytes.len() + 1 + v.len());
                    entry.extend_from_slice(k_bytes);
                    entry.push(b'=');
                    entry.extend_from_slice(v.as_bytes());
                    if let Ok(cs) = CString::new(entry) {
                        env_cstrings.push(cs);
                    }
                }
                env_cstrings.push(CString::new("TERM=xterm-256color").unwrap());
                env_cstrings.push(CString::new("COLORTERM=truecolor").unwrap());
                env_cstrings
                    .push(CString::new(format!("TERM_PROGRAM={}", TERM_PROGRAM_NAME)).unwrap());
                env_cstrings.push(
                    CString::new(format!("TERM_PROGRAM_VERSION={}", TERM_PROGRAM_VERSION)).unwrap(),
                );
                env_cstrings.push(CString::new(format!("VTE_VERSION={}", VTE_VERSION)).unwrap());
                if !has_utf8_locale {
                    env_cstrings.push(CString::new("LANG=C.UTF-8").unwrap());
                    env_cstrings.push(CString::new("LC_CTYPE=C.UTF-8").unwrap());
                }
                // LESS=FR(不含 -X)让 git 等正确使用备用屏幕;仅在用户未设置时添加。
                if !has_less {
                    env_cstrings.push(CString::new("LESS=FR").unwrap());
                }
                let mut envp: Vec<*const libc::c_char> =
                    env_cstrings.iter().map(|c| c.as_ptr()).collect();
                envp.push(std::ptr::null());

                // 4. Fork 子进程
                let fork_result = libc::fork();

                if fork_result < 0 {
                    libc::close(master);
                    libc::close(slave);
                    return Err(anyhow!("Failed to fork"));
                }

                if fork_result == 0 {
                    // 子进程分支:从这里到 execve 只调用异步信号安全的 libc 函数。
                    libc::close(master);

                    // 【关键】设置父进程死亡信号：当父进程(jterm2)死亡时，此进程会收到SIGTERM
                    // 这是最后一道防线，确保即使jterm2被SIGKILL强制杀死或panic崩溃，
                    // rsh进程也会收到退出信号，不会变成孤儿进程继续运行。
                    #[cfg(target_os = "linux")]
                    {
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    }

                    // 创建新的会话和进程组（将此进程设为会话leader）
                    libc::setsid();

                    // 切换工作目录(使用 fork 前构建好的指针)
                    if let Some(ref dir_cstr) = cwd_cstr {
                        if libc::chdir(dir_cstr.as_ptr()) != 0 {
                            write_stderr(b"jterm2: chdir failed\n");
                            libc::_exit(127);
                        }
                    }

                    // 设置 slave 为控制终端
                    if libc::ioctl(slave, libc::TIOCSCTTY, 0) != 0 {
                        write_stderr(b"jterm2: ioctl TIOCSCTTY failed\n");
                    }

                    // 重定向 stdin/stdout/stderr 到 PTY slave
                    libc::dup2(slave, libc::STDIN_FILENO);
                    libc::dup2(slave, libc::STDOUT_FILENO);
                    libc::dup2(slave, libc::STDERR_FILENO);
                    if slave > libc::STDERR_FILENO {
                        libc::close(slave);
                    }

                    // 执行 shell，使用 fork 前构建好的 argv/envp
                    libc::execve(exec_cstr.as_ptr(), argv_ptrs.as_ptr(), envp.as_ptr());

                    // 如果 execve 返回，说明出错
                    write_stderr(b"jterm2: execve failed\n");
                    libc::_exit(127);
                } else {
                    // 父进程分支
                    // 关闭 slave
                    libc::close(slave);

                    Ok(Pty {
                        master,
                        child_pid: fork_result as i32,
                        exit_code_cached: None,
                    })
                }
            }
        }

        pub fn get_child_pid(&self) -> i32 {
            self.child_pid
        }

        pub fn master_fd(&self) -> RawFd {
            self.master
        }

        pub fn wait_fd_readable(fd: RawFd, timeout_ms: i32) -> Result<bool> {
            let mut poll_fd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };

            // SAFETY: poll_fd 是有效的栈上变量，libc::poll 接受可变指针和长度，
            // 超时参数是合法的毫秒值。poll 调用是原子的，不会导致数据竞争。
            loop {
                let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
                if ready < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        continue; // EINTR:被信号打断,重试
                    }
                    return Err(anyhow!("Failed to poll PTY: {}", err));
                } else if ready == 0 {
                    return Ok(false);
                } else {
                    return Ok(
                        (poll_fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0,
                    );
                }
            }
        }

        /// 单次非阻塞写入。返回写入的字节数(可能 partial),或 WouldBlock 表示缓冲区满。
        pub fn write(&mut self, data: &[u8]) -> Result<WriteOutcome> {
            // SAFETY: self.master 是有效的文件描述符，data.as_ptr() 指向有效的内存，
            // data.len() 是正确的长度。write 系统调用不会超出缓冲区边界。
            loop {
                let n = unsafe { libc::write(self.master, data.as_ptr() as *const _, data.len()) };
                if n >= 0 {
                    return Ok(WriteOutcome::Written(n as usize));
                }
                let err = std::io::Error::last_os_error();
                match err.kind() {
                    std::io::ErrorKind::Interrupted => continue, // EINTR:重试
                    std::io::ErrorKind::WouldBlock => return Ok(WriteOutcome::WouldBlock),
                    _ => return Err(anyhow!("Failed to write to PTY: {}", err)),
                }
            }
        }

        pub fn read(&mut self, buf: &mut [u8]) -> Result<ReadOutcome> {
            // SAFETY: self.master 是有效的文件描述符，buf.as_mut_ptr() 指向有效的可变内存，
            // buf.len() 是正确的缓冲区大小。read 不会超出边界。
            loop {
                let n = unsafe { libc::read(self.master, buf.as_mut_ptr() as *mut _, buf.len()) };
                if n > 0 {
                    return Ok(ReadOutcome::Data(n as usize));
                } else if n == 0 {
                    // read 返回 0 表示对端(slave)已关闭 —— EOF。
                    return Ok(ReadOutcome::Eof);
                } else {
                    let err = std::io::Error::last_os_error();
                    match err.kind() {
                        std::io::ErrorKind::Interrupted => continue, // EINTR:重试
                        std::io::ErrorKind::WouldBlock => return Ok(ReadOutcome::WouldBlock),
                        _ => return Err(anyhow!("Failed to read from PTY: {}", err)),
                    }
                }
            }
        }

        pub fn resize(&mut self, cols: usize, rows: usize) -> Result<()> {
            // SAFETY: win_size 是有效的栈上变量，符合 libc::winsize 的内存布局。
            // ioctl TIOCSWINSZ 调用是标准的 PTY 窗口大小设置操作。
            unsafe {
                let win_size = libc::winsize {
                    ws_row: rows as u16,
                    ws_col: cols as u16,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };

                if libc::ioctl(
                    self.master,
                    libc::TIOCSWINSZ,
                    (&win_size) as *const _ as *mut libc::c_void,
                ) < 0
                {
                    return Err(anyhow!("Failed to resize PTY"));
                }
            }
            Ok(())
        }

        /// 把 waitpid 返回的 status 解码为退出码并缓存。
        /// 关键不变量:任何 reap(回收僵尸)的路径都必须缓存退出码,
        /// 这样 `exit_code_cached.is_some()` 等价于"PID 已被回收、可能被复用";
        /// kill 路径据此判断是否还能安全发信号,避免误杀复用了该 PID 的无辜进程。
        fn cache_status(&mut self, status: i32) {
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status) as i32
            } else if libc::WIFSIGNALED(status) {
                -(libc::WTERMSIG(status) as i32)
            } else {
                -1
            };
            self.exit_code_cached = Some(code);
        }

        pub fn is_alive(&mut self) -> bool {
            // If we already have a cached exit code, the process is not alive
            if self.exit_code_cached.is_some() {
                return false;
            }

            // SAFETY: waitpid 使用 WNOHANG 非阻塞检查子进程状态。
            // status 是有效的栈变量，child_pid 是有效的进程 ID。
            unsafe {
                let mut status = 0;
                let result = libc::waitpid(self.child_pid, &mut status, libc::WNOHANG);
                if result == 0 {
                    true // 子进程还活着
                } else if result > 0 {
                    // 子进程已退出且刚刚被本次调用回收 —— 必须缓存退出码,
                    // 否则会留下"已 reap 但未标记"的窗口,导致后续 kill 误杀复用 PID。
                    self.cache_status(status);
                    false
                } else {
                    // ECHILD 等:进程已不存在/已被回收。
                    self.exit_code_cached = Some(0);
                    false
                }
            }
        }

        /// 优雅终止:仅在子进程尚未被回收时发送 SIGHUP(进程组)+SIGTERM。
        /// 调用者必须持有 Pty 的互斥锁,以与 io_loop 的回收路径串行化。
        pub fn signal_terminate(&mut self) {
            if self.exit_code_cached.is_some() {
                return; // 已回收,PID 可能被复用,绝不再发信号
            }
            // 此时子进程尚未被 reap(僵尸或运行中),其 PID 仍为我们保留,kill 安全。
            // SAFETY: 负 PID 向进程组发信号是标准做法;child_pid 来自 fork。
            unsafe {
                let pgid = -self.child_pid;
                let _ = libc::kill(pgid, libc::SIGHUP);
                let _ = libc::kill(self.child_pid, libc::SIGTERM);
            }
        }

        /// 强制终止并回收:仅在尚未被回收时 SIGKILL 进程组,然后阻塞 waitpid 恰好一次。
        /// 调用者必须持有 Pty 的互斥锁。
        pub fn force_kill_and_reap(&mut self) {
            if self.exit_code_cached.is_some() {
                return; // 已被 io_loop 回收,跳过(避免误杀复用 PID)
            }
            // SAFETY: 同上,子进程尚未 reap,PID 仍保留,kill/waitpid 安全。
            unsafe {
                let pgid = -self.child_pid;
                let _ = libc::kill(pgid, libc::SIGKILL);
                let _ = libc::kill(self.child_pid, libc::SIGKILL);
                let mut status = 0;
                loop {
                    let r = libc::waitpid(self.child_pid, &mut status, 0);
                    if r > 0 {
                        self.cache_status(status);
                        break;
                    } else if r < 0
                        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    {
                        continue; // EINTR:重试
                    } else {
                        // ECHILD 等:无法回收(已被回收),记一个强杀退出码。
                        self.exit_code_cached = Some(-9);
                        break;
                    }
                }
            }
        }

        /// 单次非阻塞 waitpid。已退出/ECHILD → Some(code); 仍在运行 → None。
        /// 调用方应在锁外做有界轮询,把 sleep 排除在 Pty 临界区之外,避免 UI
        /// 线程的 resize/write 在子进程退出窗口内被阻塞数十毫秒。
        pub fn try_reap(&mut self) -> Option<i32> {
            if let Some(code) = self.exit_code_cached {
                return Some(code);
            }
            // SAFETY: WNOHANG 非阻塞 waitpid;status 为有效栈变量,child_pid 来自 fork。
            unsafe {
                let mut status = 0;
                let r = libc::waitpid(self.child_pid, &mut status, libc::WNOHANG);
                if r > 0 {
                    self.cache_status(status);
                    Some(self.exit_code_cached.unwrap_or(-1))
                } else if r < 0 {
                    self.exit_code_cached = Some(0);
                    Some(0)
                } else {
                    None
                }
            }
        }

        pub fn wait_timeout(&mut self, _timeout_ms: u64) -> Result<i32> {
            // If we already have a cached exit code, return it directly
            if let Some(code) = self.exit_code_cached {
                return Ok(code);
            }

            // SAFETY: waitpid 阻塞等待子进程退出。status 是有效的栈变量，
            // child_pid 是我们 fork 创建的有效进程 ID。
            unsafe {
                let mut status = 0;
                loop {
                    let result = libc::waitpid(self.child_pid, &mut status, 0);
                    if result < 0 {
                        let err = std::io::Error::last_os_error();
                        if err.kind() == std::io::ErrorKind::Interrupted {
                            continue; // EINTR:重试
                        }
                        // ECHILD 表示进程已被回收,返回默认退出码 0
                        if err.raw_os_error() == Some(libc::ECHILD) {
                            crate::debug_log!(
                                "[PTY] waitpid returned ECHILD, process already reaped"
                            );
                            self.exit_code_cached = Some(0);
                            return Ok(0);
                        }
                        return Err(anyhow!("waitpid failed: {}", err));
                    } else {
                        self.cache_status(status);
                        return Ok(self.exit_code_cached.unwrap_or(-1));
                    }
                }
            }
        }

        pub fn terminate(&mut self) -> Result<()> {
            // 全程门控在 exit_code_cached 上:任一 reap 路径都会缓存退出码,
            // 因此只要未缓存,子进程必未被回收、PID 仍为我们保留,kill 安全。
            self.signal_terminate();
            std::thread::sleep(std::time::Duration::from_millis(50));
            self.force_kill_and_reap();
            Ok(())
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            if self.is_alive() {
                let _ = self.terminate();
            }
            // SAFETY: close 关闭文件描述符。master 是有效的 fd，
            // 关闭后不会再使用（因为这是 Drop 实现）。
            unsafe {
                let _ = libc::close(self.master);
            }
        }
    }
}

#[cfg(windows)]
mod windows_pty {
    use super::*;

    pub struct Pty;

    impl Pty {
        pub fn new(_cols: usize, _rows: usize) -> Result<Self> {
            Err(anyhow!("PTY support not yet implemented on Windows"))
        }

        pub fn write(&mut self, _data: &[u8]) -> Result<WriteOutcome> {
            Err(anyhow!("PTY not available"))
        }

        pub fn read(&mut self, _buf: &mut [u8]) -> Result<ReadOutcome> {
            Err(anyhow!("PTY not available"))
        }

        pub fn resize(&mut self, _cols: usize, _rows: usize) -> Result<()> {
            Err(anyhow!("PTY not available"))
        }

        pub fn is_alive(&mut self) -> bool {
            false
        }

        pub fn try_reap(&mut self) -> Option<i32> {
            None
        }

        pub fn wait_timeout(&mut self, _timeout_ms: u64) -> Result<i32> {
            Err(anyhow!("PTY not available"))
        }

        pub fn signal_terminate(&mut self) {}

        pub fn force_kill_and_reap(&mut self) {}

        pub fn terminate(&mut self) -> Result<()> {
            Err(anyhow!("PTY not available"))
        }
    }
}

#[cfg(unix)]
pub use unix_pty::Pty;

#[cfg(windows)]
pub use windows_pty::Pty;

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn shell_single_quote_escapes_embedded_quotes() {
        assert_eq!(
            super::unix_pty::shell_single_quote("/tmp/it's"),
            "'/tmp/it'\"'\"'s'"
        );
    }
}
