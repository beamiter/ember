use anyhow::{anyhow, Result};
use std::ffi::CString;
use std::os::unix::io::RawFd;

/// PTY 读取结果。流关闭与子进程退出必须分开判断：Linux 的 EIO/Hangup
/// 只说明当前没有 slave fd，子进程仍可能存活；只有 waitpid 才能确认退出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    Data(usize),
    WouldBlock,
    Eof,
    Hangup,
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
    use std::ffi::OsStr;
    use std::path::Path;

    /// Retry an interrupted syscall while preserving every other error for
    /// the caller to classify. In particular, waitpid errors other than
    /// ECHILD must not be mistaken for a successfully reaped child.
    pub(super) fn retry_on_eintr<T>(
        mut operation: impl FnMut() -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        loop {
            match operation() {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }

    /// 异步信号安全地向 stderr 写一条静态消息(fork 后、execve 前只能用此类调用)。
    /// SAFETY: 仅调用 write(2),它在 POSIX 异步信号安全函数列表中。
    unsafe fn write_stderr(msg: &[u8]) {
        let _ = libc::write(
            libc::STDERR_FILENO,
            msg.as_ptr() as *const libc::c_void,
            msg.len(),
        );
    }

    /// Create a close-on-exec pipe used to report setup failures from the
    /// post-fork child. EOF means `execve` succeeded and closed the write end.
    fn startup_status_pipe() -> std::io::Result<[RawFd; 2]> {
        let mut pipe_fds = [-1; 2];

        #[cfg(any(target_os = "linux", target_os = "android"))]
        let result = unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) };

        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let result = unsafe {
            let result = libc::pipe(pipe_fds.as_mut_ptr());
            if result == 0 {
                for fd in pipe_fds {
                    let flags = libc::fcntl(fd, libc::F_GETFD, 0);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
                        libc::close(pipe_fds[0]);
                        libc::close(pipe_fds[1]);
                        return Err(std::io::Error::last_os_error());
                    }
                }
            }
            result
        };

        if result == 0 {
            Ok(pipe_fds)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Only async-signal-safe syscalls are allowed between fork and exec.
    unsafe fn report_startup_failure(fd: RawFd, code: u8) {
        // A one-byte write is atomic and the fresh pipe has capacity. Retry
        // EINTR until the parent receives a status byte; otherwise EOF means
        // successful exec and must not be produced by an interrupted write.
        loop {
            let result = libc::write(fd, &code as *const u8 as *const libc::c_void, 1);
            if result == 1 {
                return;
            }
            if result < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            return;
        }
    }

    /// Shell candidates are looked up through `jterm_core::host`, which now
    /// imposes what ember's local copy used to: the execute bit, a regular
    /// file, an absolute result, and an injectable `PATH` (launchers like wofi
    /// strip it, so the process environment cannot be the only source).
    fn find_executable_in_path_with(exe_name: &str, path_var: Option<&OsStr>) -> Option<String> {
        jterm_core::host::find_executable_in(exe_name, path_var)
            .map(|path| path.to_string_lossy().into_owned())
    }

    /// A `jsh` on PATH need not be the interactive shell ember prefers: the
    /// name can be taken by an unrelated binary, or be a symlink pointing
    /// somewhere else entirely. Launching such a program makes the child reject
    /// `--session` and exit immediately (ssh, for one, exits 255), which used to
    /// close the only tab and take the whole window down with it. Accept the
    /// candidate only when it really resolves to a `jsh` program.
    ///
    /// The shell was called `rsh` until 0.3; that name was an alternatives
    /// symlink to the BSD remote shell on Debian-family systems, which is how
    /// this guard came about. The rename removed that particular collision, but
    /// resolving the candidate is still the only way to know what we exec.
    pub(super) fn is_interactive_jsh(path: &Path) -> bool {
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let Some(name) = resolved.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        // Version-suffixed builds (jsh-0.3.0) stay eligible; anything the name
        // resolves to under another basename does not.
        name == "jsh" || name.starts_with("jsh-") || name.starts_with("jsh.")
    }

    pub(super) fn choose_shell_with_path(
        configured_shell: Option<&str>,
        path_var: Option<&OsStr>,
        sh_fallback: &Path,
    ) -> Result<String> {
        // Priority 1: explicit config / env var (needed when PATH is stripped by launchers like wofi)
        if let Some(path) = configured_shell {
            // A bare name is a PATH lookup, never an implicit `./name`.
            // Otherwise opening a project directory containing a malicious
            // executable named "bash" could hijack `shell = "bash"`. That rule
            // now lives in `host::resolve_configured_program`, which keeps it
            // verbatim (`file_name() == token` selects the PATH branch) and adds
            // the absolutization the exec path needs.
            if let Some(resolved) = jterm_core::host::resolve_configured_program(path, path_var) {
                return Ok(resolved.to_string_lossy().into_owned());
            }
            eprintln!(
                "[PTY] Configured shell '{}' is not executable, falling back",
                path
            );
        }

        // Priority 2: jsh (preferred shell with advanced features)
        if let Some(jsh_path) = find_executable_in_path_with("jsh", path_var) {
            if is_interactive_jsh(Path::new(&jsh_path)) {
                return Ok(jsh_path);
            }
            eprintln!(
                "[PTY] Ignoring '{}': it resolves to another program, not the jsh shell",
                jsh_path
            );
        }

        // Priority 3: bash (fallback)
        if let Some(bash_path) = find_executable_in_path_with("bash", path_var) {
            return Ok(bash_path);
        }

        // Priority 4: sh (last resort). execve does not search PATH, so never
        // return a bare "sh" token here.
        if let Some(sh_path) = find_executable_in_path_with("sh", path_var) {
            return Ok(sh_path);
        }
        // The fallback is a path, not a name: `resolve_configured_program` takes
        // its directory branch and only checks that it is executable.
        if let Some(sh_path) = sh_fallback
            .to_str()
            .and_then(|token| jterm_core::host::resolve_configured_program(token, None))
        {
            return Ok(sh_path.to_string_lossy().into_owned());
        }

        Err(anyhow!(
            "No executable shell found (tried configured shell, jsh, bash, PATH sh, and {})",
            sh_fallback.display()
        ))
    }

    fn choose_shell(configured_shell: Option<&str>) -> Result<String> {
        let path_var = std::env::var_os("PATH");
        choose_shell_with_path(configured_shell, path_var.as_deref(), Path::new("/bin/sh"))
    }

    /// ember's child-environment policy.
    ///
    /// `less_default = "FR"` is deliberate and predates the shared module: no
    /// `-X`, so `git`/`man` still get the alternate screen, and `F` quits on a
    /// page that fits. `color_defaults` stays off because ember has never
    /// shipped an `LS_COLORS`, and the locale repair stays on: the GPU renderer
    /// draws UTF-8 either way, but a `C`-locale child emits mojibake into it.
    pub(super) fn child_environment() -> jterm_core::child_env::ChildEnv<'static> {
        jterm_core::child_env::ChildEnv {
            // Spelled out rather than taken from `identity`, which reports the
            // *core* crate's version until `main` initializes it: the version a
            // child reads has to be this binary's whatever the startup order is.
            app_version: env!("CARGO_PKG_VERSION"),
            less_default: Some("FR"),
            color_defaults: false,
            normalize_locale: true,
            ..jterm_core::child_env::ChildEnv::from_identity()
        }
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
            command_argv: Option<&[String]>,
        ) -> Result<Self> {
            // SAFETY: 这个 unsafe 块包含多个 libc 系统调用用于 PTY 创建和进程 fork。
            // 所有的 libc 调用都检查了返回值并正确处理错误。
            // 文件描述符的生命周期被正确管理（成功时存储在 PtySession 中，失败时关闭）。
            // fork 后的子进程分支永不返回（通过 execve 或 exit），避免了未定义行为。
            unsafe {
                let win_size = libc::winsize {
                    ws_row: rows as u16,
                    ws_col: cols as u16,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };

                // Before opening file descriptors, complete every fallible
                // allocation, PATH lookup and CString conversion. This both
                // keeps the post-fork child async-signal-safe and avoids
                // leaking a PTY pair if preparation returns an error.
                // 原因:在多线程进程中 fork 后,子进程直到 execve 之间只能调用
                // 异步信号安全的函数。malloc/CString/format!/Vec/setenv/std::env/std::fs
                // 都不安全 —— 若 fork 时另一线程恰好持有 malloc 锁,子进程会永久死锁。
                // 因此这里预先构建 argv、envp、cwd 的 C 字符串,子进程分支只做 syscall。

                // 选择 shell(读取 env/fs,必须在 fork 前)
                let shell_path = choose_shell(configured_shell)?;
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

                // `find_executable_in_path` is now exec-bit-checked and
                // absolutizing in core, so the local re-check it used to need
                // is gone.
                let bash_path = if shell_name == "jsh" {
                    jterm_core::host::find_executable_in_path("bash")
                        .map(|p| p.to_string_lossy().into_owned())
                } else {
                    None
                };

                // 一次性辅助进程(例如 jsh 安装脚本)按原样 exec 给定 argv:
                // 不做登录 shell 包装,也不注入 --session。
                let command_cstrings: Option<(CString, Vec<CString>)> = match command_argv {
                    Some(argv) => {
                        let program = argv
                            .first()
                            .ok_or_else(|| anyhow!("Command argv must not be empty"))?;
                        // Resolve with `execvp` semantics *before* the fork: the
                        // child can only call `execve`, and a helper that is not
                        // installed has to fail here, where the caller can show
                        // it, instead of as a pane that exits 127 silently.
                        let program_path = jterm_core::host::resolve_executable(
                            program,
                            std::env::var_os("PATH").as_deref(),
                            cwd,
                        )
                        .map_err(|error| {
                            anyhow!("Command program '{program}' is not executable: {error}")
                        })?
                        .to_string_lossy()
                        .into_owned();
                        let mut args = Vec::with_capacity(argv.len());
                        for arg in argv {
                            args.push(
                                CString::new(arg.as_str())
                                    .map_err(|_| anyhow!("Invalid command argument"))?,
                            );
                        }
                        Some((
                            CString::new(program_path)
                                .map_err(|_| anyhow!("Invalid command path"))?,
                            args,
                        ))
                    }
                    None => None,
                };

                let (exec_cstr, argv_cstrings): (CString, Vec<CString>) =
                    if let Some(command) = command_cstrings {
                        command
                    } else if let Some(bash_path) = bash_path {
                        let exec_cmd =
                            jterm_core::process::build_jsh_exec_command(&shell_path, session_id);
                        (
                            CString::new(bash_path).map_err(|_| anyhow!("Invalid bash path"))?,
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
                        if shell_name == "jsh" {
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
                // 策略与变量集合由 jterm_core::child_env 持有,四个终端共用;这里
                // 只声明 ember 的选择 —— LESS=FR、UTF-8 locale 修正、不接管 ls 颜色。
                let env_cstrings = jterm_core::child_env::envp(&child_environment(), &[])
                    .map_err(|error| anyhow!("Invalid child environment: {error}"))?;
                let mut envp: Vec<*const libc::c_char> =
                    env_cstrings.iter().map(|c| c.as_ptr()).collect();
                envp.push(std::ptr::null());

                // Create the PTY only after all `?` exits above are behind us.
                let mut master = 0;
                let mut slave = 0;
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

                // 设置 master 非阻塞模式
                let flags = libc::fcntl(master, libc::F_GETFL, 0);
                if flags >= 0 {
                    let _ = libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
                }

                // 设置 FD_CLOEXEC，防止子进程继承
                let fd_flags = libc::fcntl(master, libc::F_GETFD, 0);
                if fd_flags >= 0 {
                    let _ = libc::fcntl(master, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC);
                }

                // The child reports chdir/exec failures through this pipe.
                // Successful exec closes its CLOEXEC write end, so the parent
                // does not return Ok until shell startup has actually crossed
                // the exec boundary.
                let startup_pipe = match startup_status_pipe() {
                    Ok(pipe) => pipe,
                    Err(error) => {
                        libc::close(master);
                        libc::close(slave);
                        return Err(anyhow!("Failed to create shell startup pipe: {error}"));
                    }
                };
                // Capture process-global state before fork. The child branch
                // then needs only async-signal-safe syscalls.
                let inherited_instance_lock_fd =
                    crate::session_persistence::inherited_instance_lock_fd();
                let mut default_signal_action: libc::sigaction = std::mem::zeroed();
                default_signal_action.sa_sigaction = libc::SIG_DFL;
                if libc::sigemptyset(&mut default_signal_action.sa_mask) != 0 {
                    libc::close(master);
                    libc::close(slave);
                    libc::close(startup_pipe[0]);
                    libc::close(startup_pipe[1]);
                    return Err(anyhow!("Failed to prepare child signal state"));
                }
                #[cfg(target_os = "linux")]
                let expected_parent_pid = libc::getpid();

                // Fork 子进程
                let fork_result = libc::fork();

                if fork_result < 0 {
                    libc::close(master);
                    libc::close(slave);
                    libc::close(startup_pipe[0]);
                    libc::close(startup_pipe[1]);
                    return Err(anyhow!("Failed to fork"));
                }

                if fork_result == 0 {
                    // 子进程分支:从这里到 execve 只调用异步信号安全的 libc 函数。
                    // A flock is tied to the inherited open-file description,
                    // not merely this process. Close it before any operation
                    // that could stall, so a PTY child can never outlive the
                    // primary process while retaining the instance lock.
                    if inherited_instance_lock_fd >= 0 {
                        libc::close(inherited_instance_lock_fd);
                    }
                    libc::close(master);
                    libc::close(startup_pipe[0]);

                    // Do not inherit the GUI's graceful-shutdown handler. A
                    // pre-exec child never polls SHUTDOWN_REQUESTED, so that
                    // handler would neutralize the parent-death SIGTERM.
                    if libc::sigaction(libc::SIGTERM, &default_signal_action, std::ptr::null_mut())
                        != 0
                    {
                        report_startup_failure(startup_pipe[1], b'S');
                        libc::_exit(127);
                    }
                    libc::sigaction(libc::SIGINT, &default_signal_action, std::ptr::null_mut());

                    // 【关键】设置父进程死亡信号：当父进程(ember)死亡时，此进程会收到SIGTERM
                    // 这是最后一道防线，确保即使ember被SIGKILL强制杀死或panic崩溃，
                    // jsh进程也会收到退出信号，不会变成孤儿进程继续运行。
                    #[cfg(target_os = "linux")]
                    {
                        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                            report_startup_failure(startup_pipe[1], b'S');
                            libc::_exit(127);
                        }
                        // The parent can die between fork and prctl. Detect
                        // that race explicitly instead of waiting forever for
                        // a signal the kernel could not retroactively deliver.
                        if libc::getppid() != expected_parent_pid {
                            libc::_exit(128 + libc::SIGTERM);
                        }
                    }

                    // 创建新的会话和进程组（将此进程设为会话leader）
                    libc::setsid();

                    // 切换工作目录(使用 fork 前构建好的指针)
                    if let Some(ref dir_cstr) = cwd_cstr {
                        if libc::chdir(dir_cstr.as_ptr()) != 0 {
                            report_startup_failure(startup_pipe[1], b'C');
                            libc::_exit(127);
                        }
                    }

                    // 设置 slave 为控制终端
                    if libc::ioctl(slave, libc::TIOCSCTTY, 0) != 0 {
                        write_stderr(b"ember: ioctl TIOCSCTTY failed\n");
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
                    report_startup_failure(startup_pipe[1], b'E');
                    libc::_exit(127);
                } else {
                    // 父进程分支
                    // 关闭 slave
                    libc::close(slave);
                    libc::close(startup_pipe[1]);

                    let startup_result = loop {
                        let mut startup_code = 0u8;
                        let result = libc::read(
                            startup_pipe[0],
                            &mut startup_code as *mut u8 as *mut libc::c_void,
                            1,
                        );
                        if result == 0 {
                            break Ok(None);
                        }
                        if result == 1 {
                            break Ok(Some(startup_code));
                        }
                        let error = std::io::Error::last_os_error();
                        if error.kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        break Err(error);
                    };
                    libc::close(startup_pipe[0]);

                    let startup_code = match startup_result {
                        Ok(None) => {
                            return Ok(Pty {
                                master,
                                child_pid: fork_result as i32,
                                exit_code_cached: None,
                            });
                        }
                        Ok(Some(code)) => code,
                        Err(error) => {
                            // An unknown pipe failure leaves shell state
                            // ambiguous, so terminate and reap before returning.
                            libc::close(master);
                            let _ = libc::kill(fork_result, libc::SIGKILL);
                            let mut status = 0;
                            while libc::waitpid(fork_result, &mut status, 0) < 0
                                && std::io::Error::last_os_error().kind()
                                    == std::io::ErrorKind::Interrupted
                            {}
                            return Err(anyhow!("Failed to read shell startup status: {error}"));
                        }
                    };

                    // A failed child never becomes an owned Pty, so close the
                    // master and reap it here instead of leaving a zombie.
                    libc::close(master);
                    let mut status = 0;
                    while libc::waitpid(fork_result, &mut status, 0) < 0
                        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    {
                    }
                    match startup_code {
                        b'C' => Err(anyhow!("Failed to enter saved working directory")),
                        b'E' => Err(anyhow!("Failed to execute shell")),
                        _ => Err(anyhow!("Shell failed during startup")),
                    }
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
                    // Linux PTY masters report EIO (rather than read(2) == 0)
                    // once the final slave fd closes. Keep it distinct from
                    // process exit: the reader must stop polling this fd and
                    // waitpid at a bounded cadence, not fabricate an exit code.
                    if err.raw_os_error() == Some(libc::EIO) {
                        return Ok(ReadOutcome::Hangup);
                    }
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
            let mut status = 0;
            let child_pid = self.child_pid;
            let result = retry_on_eintr(|| {
                // SAFETY: WNOHANG is non-blocking; status is a valid mutable
                // integer and child_pid belongs to this Pty instance.
                let result = unsafe { libc::waitpid(child_pid, &mut status, libc::WNOHANG) };
                if result < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(result)
                }
            });
            match result {
                Ok(0) => true,
                Ok(_) => {
                    // 子进程已退出且刚刚被本次调用回收 —— 必须缓存退出码,
                    // 否则会留下"已 reap 但未标记"的窗口,导致后续 kill 误杀复用 PID。
                    self.cache_status(status);
                    false
                }
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                    self.exit_code_cached = Some(0);
                    false
                }
                Err(error) => {
                    // Conservatively keep the process live: an unrelated
                    // waitpid failure is not evidence that the PID was reaped.
                    log::warn!("waitpid(WNOHANG) failed while checking PTY child: {error}");
                    true
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
            let mut status = 0;
            let child_pid = self.child_pid;
            let result = retry_on_eintr(|| {
                // SAFETY: see is_alive above.
                let result = unsafe { libc::waitpid(child_pid, &mut status, libc::WNOHANG) };
                if result < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(result)
                }
            });
            match result {
                Ok(0) => None,
                Ok(_) => {
                    self.cache_status(status);
                    Some(self.exit_code_cached.unwrap_or(-1))
                }
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                    self.exit_code_cached = Some(0);
                    Some(0)
                }
                Err(error) => {
                    log::warn!("waitpid(WNOHANG) failed while reaping PTY child: {error}");
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
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(unix)]
    static NEXT_SHELL_TEST: AtomicU64 = AtomicU64::new(0);

    #[cfg(unix)]
    struct ShellTestDir(std::path::PathBuf);

    #[cfg(unix)]
    impl ShellTestDir {
        fn new(label: &str) -> Self {
            let id = NEXT_SHELL_TEST.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ember-shell-test-{label}-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn executable(&self, name: &str) -> std::path::PathBuf {
            use std::os::unix::fs::PermissionsExt;

            let path = self.0.join(name);
            std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path
        }

        fn search_path(&self) -> std::ffi::OsString {
            std::env::join_paths([&self.0]).unwrap()
        }
    }

    #[cfg(unix)]
    impl Drop for ShellTestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn retry_on_eintr_retries_only_interrupted_errors() {
        let mut attempts = 0;
        let result = super::unix_pty::retry_on_eintr(|| {
            attempts += 1;
            if attempts < 3 {
                Err(std::io::Error::from_raw_os_error(libc::EINTR))
            } else {
                Ok(42)
            }
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts, 3);

        let mut attempts = 0;
        let error = super::unix_pty::retry_on_eintr::<()>(|| {
            attempts += 1;
            Err(std::io::Error::from_raw_os_error(libc::ECHILD))
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
        assert_eq!(attempts, 1);
    }

    #[cfg(unix)]
    #[test]
    fn configured_bare_shell_name_is_resolved_through_path() {
        let root = ShellTestDir::new("configured");
        let executable = root.executable("custom-shell");
        let search_path = root.search_path();

        let selected = super::unix_pty::choose_shell_with_path(
            Some("custom-shell"),
            Some(&search_path),
            &root.0.join("missing-sh"),
        )
        .unwrap();

        assert_eq!(std::path::Path::new(&selected), executable);
        assert!(std::path::Path::new(&selected).is_absolute());
    }

    #[cfg(unix)]
    #[test]
    fn jsh_symlinked_to_another_program_is_skipped() {
        let root = ShellTestDir::new("jsh-alternatives");
        let ssh = root.executable("ssh");
        let bash = root.executable("bash");
        // A `jsh` on PATH that is really a symlink to ssh: right name, wrong
        // binary. Exec'ing it would exit 255 and close the only tab.
        std::os::unix::fs::symlink(&ssh, root.0.join("jsh")).unwrap();
        let search_path = root.search_path();

        let selected = super::unix_pty::choose_shell_with_path(
            None,
            Some(&search_path),
            &root.0.join("missing-sh"),
        )
        .unwrap();

        assert_eq!(std::path::Path::new(&selected), bash);
    }

    #[cfg(unix)]
    #[test]
    fn a_real_jsh_binary_is_still_preferred() {
        let root = ShellTestDir::new("jsh-real");
        let jsh = root.executable("jsh");
        root.executable("bash");
        let search_path = root.search_path();

        let selected = super::unix_pty::choose_shell_with_path(
            None,
            Some(&search_path),
            &root.0.join("missing-sh"),
        )
        .unwrap();

        assert_eq!(std::path::Path::new(&selected), jsh);
        assert!(super::unix_pty::is_interactive_jsh(&jsh));
    }

    #[cfg(unix)]
    #[test]
    fn versioned_jsh_builds_remain_eligible() {
        let root = ShellTestDir::new("jsh-versioned");
        let versioned = root.executable("jsh-0.3.0");
        std::os::unix::fs::symlink(&versioned, root.0.join("jsh")).unwrap();

        assert!(super::unix_pty::is_interactive_jsh(&root.0.join("jsh")));
    }

    #[cfg(unix)]
    #[test]
    fn sh_fallback_is_resolved_to_an_executable_path() {
        let root = ShellTestDir::new("path-sh");
        let executable = root.executable("sh");
        let search_path = root.search_path();

        let selected = super::unix_pty::choose_shell_with_path(
            None,
            Some(&search_path),
            &root.0.join("missing-sh"),
        )
        .unwrap();

        assert_eq!(std::path::Path::new(&selected), executable);
        assert_ne!(selected, "sh");
    }

    #[cfg(unix)]
    #[test]
    fn missing_shells_return_an_explicit_error() {
        let root = ShellTestDir::new("missing");
        let error = super::unix_pty::choose_shell_with_path(None, None, &root.0.join("missing-sh"))
            .unwrap_err();

        assert!(error.to_string().contains("No executable shell found"));
    }

    #[cfg(unix)]
    #[test]
    fn missing_working_directory_is_reported_before_returning_a_pty() {
        let root = ShellTestDir::new("missing-cwd");
        let missing_cwd = root.0.join("deleted");
        let error =
            super::Pty::new_with_cwd(80, 24, missing_cwd.to_str(), None, Some("/bin/sh"), None)
                .err()
                .expect("a missing cwd must fail before returning a PTY");

        assert!(
            error
                .to_string()
                .contains("Failed to enter saved working directory"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn exec_failure_is_reported_before_returning_a_pty() {
        use std::os::unix::fs::PermissionsExt;

        let root = ShellTestDir::new("exec-failure");
        let invalid_shell = root.0.join("invalid-shell");
        std::fs::write(&invalid_shell, b"not an executable format").unwrap();
        std::fs::set_permissions(&invalid_shell, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error =
            super::Pty::new_with_cwd(80, 24, Some("/tmp"), None, invalid_shell.to_str(), None)
                .err()
                .expect("execve failure must be reported synchronously");

        assert!(
            error.to_string().contains("Failed to execute shell"),
            "{error}"
        );
    }
    /// ember's child-environment choices are now flags on a shared policy, so
    /// pin the ones a "colour fix" could quietly change: the pager stays `FR`,
    /// `ls` colours stay the user's business, and the locale repair stays on.
    #[cfg(unix)]
    #[test]
    fn child_environment_keeps_the_pager_and_locale_choices() {
        let options = super::unix_pty::child_environment();
        assert_eq!(options.less_default, Some("FR"));
        assert!(!options.color_defaults);
        assert!(options.normalize_locale);
        assert_eq!(options.app_version, env!("CARGO_PKG_VERSION"));

        let pairs = jterm_core::child_env::pairs(&options, &[]);
        let value = |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == std::ffi::OsStr::new(name))
                .map(|(_, value)| value.to_string_lossy().into_owned())
        };
        // The reason for the whole exercise: a child that used to be told only
        // TERM now learns the colour depth and which terminal build it is in.
        assert_eq!(value("COLORTERM").as_deref(), Some("truecolor"));
        assert_eq!(
            value("TERM_PROGRAM_VERSION").as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(value("TERM").as_deref(), Some("xterm-256color"));
        assert_eq!(value("VTE_VERSION").as_deref(), Some("7802"));
        assert_eq!(value("LS_COLORS"), None);
        // `pairs` reads this process's environment, so only one of the two
        // outcomes is available at a time — both are assertions about policy.
        match std::env::var_os("LESS") {
            Some(_) => assert_eq!(value("LESS"), None, "a user's LESS is never overridden"),
            None => assert_eq!(value("LESS").as_deref(), Some("FR")),
        }
    }

    /// Resolution moved ahead of the fork: a helper argv naming a program that
    /// is not installed must fail where the caller can report it, instead of
    /// opening a pane whose child exits 127 for no visible reason.
    #[cfg(unix)]
    #[test]
    fn an_unresolvable_command_program_fails_before_forking() {
        let argv = vec!["ember-definitely-not-installed-program".to_string()];
        let error = super::Pty::new_with_cwd(80, 24, Some("/tmp"), None, None, Some(&argv))
            .err()
            .expect("an unknown helper program must not spawn");

        assert!(error.to_string().contains("is not executable"), "{error}");
    }

    /// The one-shot helper path (jsh installer) must exec the given argv
    /// verbatim: no login-shell wrapping, no --session injection.
    #[cfg(unix)]
    #[test]
    fn an_explicit_argv_is_exec_ed_verbatim() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'argv0=%s arg=%s' \"$0\" \"$1\"".to_string(),
            "jterm-command-label".to_string(),
            "payload".to_string(),
        ];
        let mut pty = super::Pty::new_with_cwd(80, 24, Some("/tmp"), None, None, Some(&argv))
            .expect("explicit argv must spawn");

        let mut seen = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut buf = [0u8; 1024];
        while std::time::Instant::now() < deadline && !seen.contains("arg=payload") {
            match pty.read(&mut buf) {
                Ok(super::ReadOutcome::Data(n)) => {
                    seen.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                Ok(super::ReadOutcome::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(_) | Err(_) => break,
            }
        }

        assert!(
            seen.contains("argv0=jterm-command-label"),
            "argv[0] must reach the child unchanged: {seen:?}"
        );
        assert!(
            seen.contains("arg=payload"),
            "remaining arguments must reach the child: {seen:?}"
        );
    }
}
