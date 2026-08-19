//! 远程文件系统访问层：侧边栏文件树通过它浏览本机、SSH 主机和 Docker 容器。
//!
//! 不依赖 sshfs、不新增 crate：远程访问复用 jsh-remote 的思路 —— spawn 系统的
//! `ssh` / `docker` 二进制，把一段小型 POSIX sh 探针脚本（[`PROBE_SCRIPT`]）喂到
//! 远端的 stdin，操作数走位置参数（`sh -s -- <op> [args...]`）。所有公共函数都
//! 是阻塞的；调用方（侧边栏的扫描/操作 worker 线程）负责把它们移出 UI 线程。
//!
//! 安全约束：
//! - ssh 的远端命令是单个 argv 元素，每个参数经 [`sq`] 单引号转义，绝不未加
//!   引号拼接路径（ssh 会把命令交给远端登录 shell 重新解析）。
//! - `docker exec` 走原始 argv，无需转义；永远用 `-i`（stdin），不用 `-t`
//!   （探针不是交互会话，分配 TTY 只会污染输出）。
//! - 子进程 stdout/stderr 都有界读取，watchdog 线程在超时后强制 kill。
//! - put/untar 的 stdin 要整个留给上传载荷，脚本本体改走 `sh -c` 内联
//!   （`sh -s` 的预读缓冲会和探针里的 `cat`/`tar x` 抢 stdin 字节）。

use parking_lot::Mutex;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use jterm_core::jsh_remote::RemoteHostConfig;

use crate::sidebar::MAX_DIRECTORY_ENTRIES;

/// list/home 探针超时（含连接建立）。
const PROBE_LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// 变更操作探针超时（`cp -a` 一个大目录可能需要的时间）。
const PROBE_OP_TIMEOUT: Duration = Duration::from_secs(60);
/// `list` stdout 上限：16K 条目的长名称输出通常不到一半，余量充足。
const MAX_LIST_OUTPUT: u64 = 8 * 1024 * 1024;
/// home / 变更类探针的输出上限（正常只有一行或为空）。
const MAX_SMALL_OUTPUT: u64 = 64 * 1024;
/// 与 sidebar::scan_dir 相同的扫描上限：超过即按截断处理。
const MAX_SCANNED_PAIRS: usize = MAX_DIRECTORY_ENTRIES * 4;
/// 本地递归复制的防御性深度上限。符号链接按链接复制、不会成环，
/// 这个上限防的是病态深树耗尽 op worker 的栈。
const MAX_COPY_DEPTH: usize = 256;
/// 跨位置传输（上传/下载/中转）的总字节上限。跨位置粘贴是显式的单
/// 文件/单目录操作，不是备份工具；超限即中止并清理部分数据。
pub const MAX_TRANSFER_BYTES: u64 = 512 * 1024 * 1024;
/// 传输总超时：15 分钟。大文件走慢链路需要余量；watchdog 仍兜底防卡死。
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// 流式转发的块大小。
const STREAM_BUF_SIZE: usize = 64 * 1024;

/// 文件树当前浏览的位置：本机，或 `config.remote_hosts` 里的第 N 台主机。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FsLocation {
    Local,
    Remote(usize),
}

impl FsLocation {
    /// 位置选择器里显示的标签。
    pub fn label(&self, hosts: &[RemoteHostConfig]) -> String {
        match self {
            FsLocation::Local => "Local".to_string(),
            FsLocation::Remote(index) => match hosts.get(*index) {
                Some(host) => format!(
                    "{}: {}",
                    if host.docker { "docker" } else { "ssh" },
                    host.display_name()
                ),
                None => format!("remote #{index}（已从配置移除）"),
            },
        }
    }
}

/// 目录条目。`path` = 所在目录 + `name`；远程场景下是远端路径。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// 文件操作剪贴板（Copy / Cut → Paste）。同一 [`FsLocation`] 内粘贴走
/// copy/rename 探针；跨位置粘贴走流式传输（下载/上传/本地中转）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsClipboard {
    pub loc: FsLocation,
    pub path: PathBuf,
    pub is_dir: bool,
    pub cut: bool,
}

impl FsClipboard {
    /// 粘贴目标路径：目标目录 + 源文件名。源是 `/` 这类没有文件名的路径时
    /// 返回 None（调用方应拒绝这次粘贴）。
    pub fn paste_destination(&self, target_dir: &Path) -> Option<PathBuf> {
        let name = self.path.file_name()?;
        Some(target_dir.join(name))
    }
}

/// 一次子进程调用的有界结果。
#[derive(Debug)]
pub struct Capture {
    /// 退出码；子进程被信号杀死（超时/取消 kill）时为 None。
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// watchdog 超时并 kill 了子进程。
    pub timed_out: bool,
    /// 用户取消（令牌触发 kill），区别于超时与真实错误。
    pub cancelled: bool,
}

/// 远端探针脚本协议 v3。默认经 stdin 传给远端的 `sh -s -- <op> [args...]`；
/// put/untar 例外，走 `sh -c` 内联脚本（stdin 整个留给上传载荷）：
/// - `list` 的 stdout 是 NUL 分隔的 "<t>\0<name>\0" 对，t ∈ {d, f, l}，相对名。
/// - v2 新增：`cat` 流式读文件、`put` 流式写新文件（临时名 + mv 原子就位）、
///   `tar` 目录打包流。
/// - v3：`untar` 改为 `untar <dir> <name>` —— 解包前先查 `<dir>/<name>` 是否
///   已存在（17），目录上传/中转因此 fail-closed（检查与解包之间仍有微秒级
///   TOCTOU 窗口，见代码注释；这是 tar 合并语义的协议极限）。新增 `stat`
///   打印 "<t> <size>"（f 为字节数，其余 0），取代 v2 的 list+cat 双探针预检。
/// - 退出码：0 正常，2 用法/路径非法，3 无法进入目录，4 操作失败，17 目标已存在。
pub const PROBE_SCRIPT: &str = r#"# remote-fs probe v3 — runs under `sh -s -- <op> [args...]`.
# `list` stdout: NUL-separated pairs "<t>\0<name>\0", t in {d,f,l}, names relative.
# Exit codes: 0 ok, 2 usage/bad path, 3 cannot enter dir, 4 op failed, 17 target exists.
# v2 adds: cat (stream file to stdout), put (stream stdin to a new file),
# tar (stream dir as tar to stdout), untar (extract stdin tar into a dir).
# v3: untar takes <dir> <name> and refuses an existing <dir>/<name> (17) before
# extracting; new stat op prints "<t> <size>" (t in {d,f,l}; bytes for f, else 0).
set -u
op=${1:-}
case "$op" in
  home)
    cd 2>/dev/null || cd / || exit 3
    pwd
    ;;
  list)
    d=${2:-}
    case "$d" in /*) ;; *) exit 2 ;; esac
    cd "$d" 2>/dev/null || exit 3
    for f in * .[!.]* ..?*; do
      if [ -d "$f" ]; then t=d
      elif [ -L "$f" ]; then t=l
      elif [ -e "$f" ]; then t=f
      else continue
      fi
      printf '%s\0%s\0' "$t" "$f"
    done
    ;;
  mkdir)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -e "$p" ] && exit 17
    mkdir "$p" || exit 4
    ;;
  mkfile)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -e "$p" ] && exit 17
    : > "$p" || exit 4
    ;;
  rm)
    p=${2:-}
    case "$p" in /*?*) ;; *) exit 2 ;; esac
    if [ -d "$p" ] && [ ! -L "$p" ]; then rm -rf "$p" || exit 4; else rm -f "$p" || exit 4; fi
    ;;
  mv)
    s=${2:-}; n=${3:-}
    case "$s" in /*) ;; *) exit 2 ;; esac
    case "$n" in /*) ;; *) exit 2 ;; esac
    [ -e "$n" ] && exit 17
    mv "$s" "$n" || exit 4
    ;;
  cp)
    s=${2:-}; n=${3:-}
    case "$s" in /*) ;; *) exit 2 ;; esac
    case "$n" in /*) ;; *) exit 2 ;; esac
    [ -e "$n" ] && exit 17
    cp -a "$s" "$n" || exit 4
    ;;
  cat)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -f "$p" ] && [ -r "$p" ] || exit 3
    cat "$p" || exit 4
    ;;
  put)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -e "$p" ] && exit 17
    t="$p.fspart.$$"
    if ! cat > "$t"; then rm -f "$t"; exit 4; fi
    [ -e "$p" ] && { rm -f "$t"; exit 17; }
    mv "$t" "$p" || { rm -f "$t"; exit 4; }
    ;;
  tar)
    p=${2:-}
    p=${p%/}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -d "$p" ] || exit 3
    command -v tar >/dev/null 2>&1 || { echo "remote-fs probe: tar is not available" >&2; exit 4; }
    tar cf - -C "${p%/*}" "${p##*/}" || exit 4
    ;;
  untar)
    d=${2:-}
    n=${3:-}
    case "$d" in /*) ;; *) exit 2 ;; esac
    case "$n" in ""|*/*) exit 2 ;; esac
    [ -d "$d" ] || exit 3
    [ -e "$d/$n" ] && exit 17
    command -v tar >/dev/null 2>&1 || { echo "remote-fs probe: tar is not available" >&2; exit 4; }
    tar xf - -C "$d" || exit 4
    ;;
  stat)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    if [ -d "$p" ]; then t=d
    elif [ -L "$p" ]; then t=l
    elif [ -e "$p" ]; then t=f
    else exit 3
    fi
    s=0
    if [ "$t" = f ]; then s=$(wc -c < "$p") || exit 4; fi
    printf '%s %s\n' "$t" "$s"
    ;;
  *) exit 2 ;;
esac
exit 0
"#;

/// 新名称（New File / New Folder / Rename 对话框共用）校验：
/// 非空、≤255 字节、不含 `/`/NUL、不是 `.`/`..`。
pub fn validate_new_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("名称不能为空".to_string());
    }
    if name.len() > 255 {
        return Err("名称过长（超过 255 字节）".to_string());
    }
    if name == "." || name == ".." {
        return Err("名称不能是 . 或 ..".to_string());
    }
    if name.contains('/') || name.contains('\0') {
        return Err("名称不能包含 / 或 NUL".to_string());
    }
    Ok(())
}

/// 字节数的人性化格式（1024 进制，一位小数）："512 B"、"12.4 KiB"、
/// "3.0 MiB"、"1.5 GiB"。传输进度行使用。
pub fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else if bytes < GIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    }
}

/// 进度回报节流间隔：≤4 次/秒。
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(250);
/// 进度回报节流字节粒度：增量 ≥256 KiB 也立即报一次。
const PROGRESS_MIN_BYTES: u64 = 256 * 1024;

/// 传输进度回报（节流 sink）：调用方每写完一块就 [`ProgressSink::report`]，
/// 是否真正外发由节流决定 —— 首次非零立即报；之后距上次 ≥250ms 或增量
/// ≥256 KiB 才报。成功收尾用 [`ProgressSink::report_final`] 强制报出终值。
pub struct ProgressSink {
    report: Box<dyn FnMut(u64) + Send>,
    last_sent_at: std::time::Instant,
    last_sent_bytes: u64,
}

impl ProgressSink {
    pub fn new(report: impl FnMut(u64) + Send + 'static) -> Self {
        Self {
            report: Box::new(report),
            // 预设成"早已该报"，让第一块就立即外发（UI 尽快出现非零值）。
            last_sent_at: std::time::Instant::now() - PROGRESS_MIN_INTERVAL,
            last_sent_bytes: 0,
        }
    }

    /// 每写完一块调用一次；是否外发由节流决定。
    pub fn report(&mut self, total: u64) {
        if total == self.last_sent_bytes {
            return;
        }
        let due_time = self.last_sent_at.elapsed() >= PROGRESS_MIN_INTERVAL;
        let due_bytes = total.saturating_sub(self.last_sent_bytes) >= PROGRESS_MIN_BYTES;
        if due_time || due_bytes {
            self.emit(total);
        }
    }

    /// 成功收尾时强制报出最终字节数（不走节流）。
    pub fn report_final(&mut self, total: u64) {
        if total != self.last_sent_bytes {
            self.emit(total);
        }
    }

    fn emit(&mut self, total: u64) {
        self.last_sent_at = std::time::Instant::now();
        self.last_sent_bytes = total;
        (self.report)(total);
    }
}

/// 传输控制：进度回报 + 取消令牌，跨 runner 传递。非传输探针不涉及。
#[derive(Default)]
pub struct TransferControl {
    pub progress: Option<ProgressSink>,
    pub cancel: Option<Arc<AtomicBool>>,
}

/// 取消语义的 io 错误。传输管线里 `Interrupted` 只可能来自取消令牌
/// （本模块自己的 IO 路径不会产出 EINTR），UI 据此显示中性"已取消"。
pub fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "transfer cancelled")
}

/// 见 [`cancelled_error`]：传输失败是否其实是用户取消。
pub fn is_cancelled_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Interrupted
}

/// POSIX 单引号转义：`'` → `'\''`。
fn sq(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// 远端命令行，作为单个 argv 元素传给 ssh。op 是我们自己的常量、直接拼接；
/// 每个参数经 [`sq`] 转义，路径永远不会被远端 shell 重新解释。
fn probe_command(op: &str, args: &[&str]) -> String {
    let mut cmd = String::from("sh -s -- ");
    cmd.push_str(op);
    for arg in args {
        cmd.push(' ');
        cmd.push_str(&sq(arg));
    }
    cmd
}

/// `sh -c` 内联脚本形式的远端命令行：put/untar 专用。`sh -c` 的第一个剩余
/// 参数是 $0（命令名），其后才是 $1...，与 `sh -s --` 的位置参数布局一致。
fn sh_c_probe_command(op: &str, args: &[&str]) -> String {
    let mut cmd = String::from("sh -c ");
    cmd.push_str(&sq(PROBE_SCRIPT));
    cmd.push_str(" remote-fs-probe ");
    cmd.push_str(op);
    for arg in args {
        cmd.push(' ');
        cmd.push_str(&sq(arg));
    }
    cmd
}

/// ssh 公共前缀：`-o BatchMode=yes -o ConnectTimeout=10 <ssh_args...> -- <dest>`。
/// BatchMode 保证无密钥时快速失败而不是挂在密码提示上。
fn ssh_base_argv(host: &RemoteHostConfig) -> Vec<String> {
    let mut argv = vec![
        "ssh".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
    ];
    argv.extend(host.ssh_args.iter().cloned());
    argv.push("--".to_string());
    let destination = match &host.user {
        Some(user) => format!("{user}@{}", host.host),
        None => host.host.clone(),
    };
    argv.push(destination);
    argv
}

/// docker 公共前缀：`docker exec -i [-u user] <container>`。原始 argv 无需
/// 转义（docker 不做远端 shell 重解析），`-i` 提供 stdin。
fn docker_base_argv(host: &RemoteHostConfig) -> Vec<String> {
    let mut argv = vec!["docker".to_string(), "exec".to_string(), "-i".to_string()];
    if let Some(user) = &host.user {
        argv.push("-u".to_string());
        argv.push(user.clone());
    }
    argv.push(host.host.clone());
    argv
}

/// 脚本走 stdin 的探针 argv（`sh -s --`）：除 put/untar 外的所有 op。
fn probe_argv(host: &RemoteHostConfig, op: &str, args: &[&str]) -> Vec<String> {
    if host.docker {
        let mut argv = docker_base_argv(host);
        argv.push("sh".to_string());
        argv.push("-s".to_string());
        argv.push("--".to_string());
        argv.push(op.to_string());
        argv.extend(args.iter().map(|arg| arg.to_string()));
        argv
    } else {
        let mut argv = ssh_base_argv(host);
        argv.push(probe_command(op, args));
        argv
    }
}

/// 脚本内联的探针 argv（`sh -c`）：put/untar 专用，stdin 整个留给上传载荷。
fn sh_c_probe_argv(host: &RemoteHostConfig, op: &str, args: &[&str]) -> Vec<String> {
    if host.docker {
        let mut argv = docker_base_argv(host);
        argv.push("sh".to_string());
        argv.push("-c".to_string());
        argv.push(PROBE_SCRIPT.to_string());
        argv.push("remote-fs-probe".to_string());
        argv.push(op.to_string());
        argv.extend(args.iter().map(|arg| arg.to_string()));
        argv
    } else {
        let mut argv = ssh_base_argv(host);
        argv.push(sh_c_probe_command(op, args));
        argv
    }
}

fn spawn_piped(argv: &[String]) -> io::Result<Child> {
    let Some((program, args)) = argv.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    };
    Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// 子进程 + watchdog 的组合：超时或取消令牌触发时强制 kill（同一条 kill
/// 路径），try_wait 轮询而不是 wait() 长持锁（watchdog 需要同一把锁来 kill）。
struct MonitoredChild {
    child: Arc<Mutex<Child>>,
    timed_out: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    done_tx: mpsc::Sender<()>,
    watchdog: std::thread::JoinHandle<()>,
}

/// watchdog 的醒转间隔：有取消令牌时按它轮询令牌。
const WATCHDOG_POLL: Duration = Duration::from_millis(100);

impl MonitoredChild {
    fn new(child: Child, timeout: Duration, cancel: Option<Arc<AtomicBool>>) -> io::Result<Self> {
        let child = Arc::new(Mutex::new(child));
        let timed_out = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let watchdog = {
            let child = child.clone();
            let timed_out = timed_out.clone();
            let cancelled = cancelled.clone();
            std::thread::Builder::new()
                .name("ember-fs-probe-watchdog".to_string())
                .spawn(move || {
                    let deadline = std::time::Instant::now() + timeout;
                    loop {
                        match done_rx.recv_timeout(WATCHDOG_POLL) {
                            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                // kill 成功才标记，避免与已自然退出的子进程竞争时误报。
                                if cancel
                                    .as_ref()
                                    .is_some_and(|token| token.load(Ordering::SeqCst))
                                {
                                    if child.lock().kill().is_ok() {
                                        cancelled.store(true, Ordering::SeqCst);
                                    }
                                    break;
                                }
                                if std::time::Instant::now() >= deadline {
                                    if child.lock().kill().is_ok() {
                                        timed_out.store(true, Ordering::SeqCst);
                                    }
                                    break;
                                }
                            }
                        }
                    }
                })?
        };
        Ok(Self {
            child,
            timed_out,
            cancelled,
            done_tx,
            watchdog,
        })
    }

    /// 传输超限/流错误时立即中止子进程。
    fn kill(&self) {
        let _ = self.child.lock().kill();
    }

    /// 等子进程退出并停掉 watchdog，返回（退出码，是否超时，是否取消）。
    fn wait(self) -> io::Result<(Option<i32>, bool, bool)> {
        let status = loop {
            if let Some(status) = self.child.lock().try_wait()? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        // 通知 watchdog 停止并等它退出，避免残留线程挂着子进程的锁。
        let _ = self.done_tx.send(());
        let _ = self.watchdog.join();
        Ok((
            status.code(),
            self.timed_out.load(Ordering::SeqCst),
            self.cancelled.load(Ordering::SeqCst),
        ))
    }
}

/// stderr 另开读者线程：子进程写满 stderr 管道而主线程在读 stdout 时，
/// 单线程顺序读会互相等待形成死锁。
fn spawn_stderr_reader(
    stderr: std::process::ChildStderr,
    max_out: u64,
) -> io::Result<std::thread::JoinHandle<Vec<u8>>> {
    std::thread::Builder::new()
        .name("ember-fs-probe-stderr".to_string())
        .spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.take(max_out).read_to_end(&mut buf);
            buf
        })
}

/// 有界地运行一个子进程：pipe stdio，写入并关闭 stdin，stdout/stderr 各按
/// `max_out` 截断读取，watchdog 线程在 `timeout` 后 kill 子进程。
pub fn run_capture(
    argv: &[String],
    stdin_bytes: &[u8],
    timeout: Duration,
    max_out: u64,
) -> io::Result<Capture> {
    run_capture_with_cancel(argv, stdin_bytes, timeout, max_out, None)
}

/// [`run_capture`] + 取消令牌（目录解包等传输途中的本地子进程使用）。
fn run_capture_with_cancel(
    argv: &[String],
    stdin_bytes: &[u8],
    timeout: Duration,
    max_out: u64,
    cancel: Option<Arc<AtomicBool>>,
) -> io::Result<Capture> {
    let mut child = spawn_piped(argv)?;

    // 探针脚本约 2KB，远小于 64KB 管道缓冲，同步写不会阻塞；子进程若立即
    // 退出，BrokenPipe 按无害处理（结果由退出码说话）。
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_bytes);
    }

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout pipe missing"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("child stderr pipe missing"))?;

    let monitored = MonitoredChild::new(child, timeout, cancel)?;
    let stderr_reader = spawn_stderr_reader(stderr_pipe, max_out)?;

    let mut stdout = Vec::new();
    let read_result = stdout_pipe.take(max_out).read_to_end(&mut stdout);
    // 读端随 Take 一起 drop，子进程继续写会收到 SIGPIPE 自行退出。

    let (code, timed_out, cancelled) = monitored.wait()?;
    let stderr = stderr_reader.join().unwrap_or_default();

    read_result?;
    Ok(Capture {
        status: code,
        stdout,
        stderr,
        timed_out,
        cancelled,
    })
}

/// 超限错误（512 MiB 传输帽）。
fn too_large_error(max_bytes: u64) -> io::Error {
    io::Error::other(format!(
        "transfer exceeds the {} MiB limit",
        max_bytes / (1024 * 1024)
    ))
}

/// 有界地把子进程 stdout 流进本地文件：整块转发、随时计数，超限或流错误
/// 立即 kill 子进程并报错；部分文件的清理由调用方负责。返回的 Capture
/// stdout 恒为空（字节都落盘了）。`control` 携带进度回报与取消令牌。
fn run_stream_to_file(
    argv: &[String],
    stdin_bytes: &[u8],
    dest: &Path,
    timeout: Duration,
    max_bytes: u64,
    mut control: TransferControl,
) -> io::Result<Capture> {
    let mut child = spawn_piped(argv)?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_bytes);
    }
    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout pipe missing"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("child stderr pipe missing"))?;

    let monitored = MonitoredChild::new(child, timeout, control.cancel.clone())?;
    let stderr_reader = spawn_stderr_reader(stderr_pipe, MAX_SMALL_OUTPUT)?;

    let mut file = std::fs::File::create(dest)?;
    let mut buffer = [0u8; STREAM_BUF_SIZE];
    let mut total = 0u64;
    let mut stream_error: Option<io::Error> = None;
    loop {
        match stdout_pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                total += n as u64;
                if total > max_bytes {
                    monitored.kill();
                    stream_error = Some(too_large_error(max_bytes));
                    break;
                }
                if let Err(error) = file.write_all(&buffer[..n]) {
                    monitored.kill();
                    stream_error = Some(error);
                    break;
                }
                if let Some(sink) = control.progress.as_mut() {
                    sink.report(total);
                }
            }
            Err(error) => {
                monitored.kill();
                stream_error = Some(error);
                break;
            }
        }
    }
    drop(file);

    let (code, timed_out, cancelled) = monitored.wait()?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if let Some(error) = stream_error {
        return Err(error);
    }
    if code == Some(0) && !timed_out && !cancelled {
        if let Some(sink) = control.progress.as_mut() {
            sink.report_final(total);
        }
    }
    Ok(Capture {
        status: code,
        stdout: Vec::new(),
        stderr,
        timed_out,
        cancelled,
    })
}

/// 有界地把本地文件流进子进程 stdin：先按 metadata 预检大小，写端在独立
/// 线程（子进程提前退出时 BrokenPipe 正常收尾，结果看退出码）。子进程退出
/// 码为 0 但写出的字节数与预检不符（本地文件在传输中被改动）时如实报错 —
/// 此时远端落位的可能是截断文件，绝不包装成成功。`control` 携带进度回报
/// （在写端线程里按块上报）与取消令牌。
fn run_stream_from_file(
    argv: &[String],
    src: &Path,
    timeout: Duration,
    max_bytes: u64,
    control: TransferControl,
) -> io::Result<Capture> {
    let expected = std::fs::metadata(src)?.len();
    if expected > max_bytes {
        return Err(too_large_error(max_bytes));
    }
    let mut child = spawn_piped(argv)?;
    let stdin_pipe = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("child stdin pipe missing"))?;
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout pipe missing"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("child stderr pipe missing"))?;

    let monitored = MonitoredChild::new(child, timeout, control.cancel.clone())?;
    let stderr_reader = spawn_stderr_reader(stderr_pipe, MAX_SMALL_OUTPUT)?;

    let src_path = src.to_path_buf();
    let mut progress = control.progress;
    let writer = std::thread::Builder::new()
        .name("ember-fs-upload-writer".to_string())
        .spawn(move || -> io::Result<u64> {
            let mut file = std::fs::File::open(&src_path)?;
            let mut stdin = stdin_pipe;
            let mut buffer = [0u8; STREAM_BUF_SIZE];
            let mut total = 0u64;
            let mut clean_eof = false;
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    clean_eof = true;
                    break;
                }
                total += n as u64;
                if total > max_bytes {
                    return Err(too_large_error(max_bytes));
                }
                match stdin.write_all(&buffer[..n]) {
                    Ok(()) => {}
                    // 子进程提前退出（比如 put 发现目标已存在直接 17）。
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
                    Err(error) => return Err(error),
                }
                if let Some(sink) = progress.as_mut() {
                    sink.report(total);
                }
            }
            if clean_eof {
                if let Some(sink) = progress.as_mut() {
                    sink.report_final(total);
                }
            }
            Ok(total)
        })?;

    let mut stdout = Vec::new();
    let read_result = stdout_pipe.take(MAX_SMALL_OUTPUT).read_to_end(&mut stdout);
    let (code, timed_out, cancelled) = monitored.wait()?;
    let stderr = stderr_reader.join().unwrap_or_default();
    let written = writer
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("upload writer panicked")))?;
    read_result?;
    if code == Some(0) && !timed_out && written != expected {
        return Err(io::Error::other(format!(
            "local file changed during upload ({written} of {expected} bytes sent)"
        )));
    }
    Ok(Capture {
        status: code,
        stdout,
        stderr,
        timed_out,
        cancelled,
    })
}

/// 运行一次远端探针。脚本本体经 stdin 传给远端的 sh。
fn run_probe(
    host: &RemoteHostConfig,
    op: &str,
    args: &[&str],
    timeout: Duration,
    max_out: u64,
) -> io::Result<Capture> {
    host.validate().map_err(|problem| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("remote host {}: {problem}", host.display_name()),
        )
    })?;
    run_capture(
        &probe_argv(host, op, args),
        PROBE_SCRIPT.as_bytes(),
        timeout,
        max_out,
    )
}

/// 探针退出码 → io 错误。脚本协议：0 正常，2 用法/路径非法，3 无法进入
/// 目录，4 操作失败，17 目标已存在；其余（含 127 = 远端没有 sh）一律 Other。
/// 取消与超时优先于退出码（被 kill 的进程退出码没有意义）。
fn probe_output(capture: Capture) -> io::Result<Vec<u8>> {
    if capture.cancelled {
        return Err(cancelled_error());
    }
    if capture.timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "remote probe timed out",
        ));
    }
    match capture.status {
        Some(0) => Ok(capture.stdout),
        Some(17) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "target already exists",
        )),
        Some(3) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "cannot enter directory",
        )),
        Some(2) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            probe_stderr(&capture),
        )),
        _ => Err(io::Error::other(probe_stderr(&capture))),
    }
}

/// 从有界的 stderr 里取一行短消息用于 UI 展示。
fn probe_stderr(capture: &Capture) -> String {
    let text = String::from_utf8_lossy(&capture.stderr);
    let text = text.trim();
    if text.is_empty() {
        return format!("remote probe failed (exit {:?})", capture.status);
    }
    let mut line = text
        .lines()
        .next()
        .unwrap_or("remote probe failed")
        .to_string();
    if line.len() > 200 {
        line.truncate(200);
    }
    line
}

/// 解析 `list` 的 stdout：NUL 分隔的 (type, name) 对。d → 目录，f/l → 文件
/// （符号链接不展开成目录）。与本地 scan_dir 同一策略：隐藏 dotfiles、目录
/// 在前且大小写不敏感排序；非 UTF-8 名称 lossy 转换。远端输出不可信：
/// 空名、`.`/`..`、带 `/` 的名称一律跳过。至多保留 MAX_DIRECTORY_ENTRIES + 1
/// 条 —— 多出的第 MAX+1 条让上层（扫描 worker）据此标记"目录已截断"。
fn parse_list(bytes: &[u8], dir: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut tokens = bytes.split(|byte| *byte == 0);
    let mut scanned = 0usize;
    while let (Some(kind), Some(name)) = (tokens.next(), tokens.next()) {
        scanned += 1;
        if scanned > MAX_SCANNED_PAIRS || entries.len() > MAX_DIRECTORY_ENTRIES {
            break;
        }
        if name.is_empty() || matches!(name, [b'.'] | [b'.', b'.']) || name.contains(&b'/') {
            continue;
        }
        let name = String::from_utf8_lossy(name).into_owned();
        if name.starts_with('.') {
            continue;
        }
        let is_dir = match kind.first() {
            Some(b'd') => true,
            Some(b'f') | Some(b'l') => false,
            _ => continue,
        };
        entries.push(Entry {
            path: dir.join(&name),
            name,
            is_dir,
        });
    }
    sort_entries(&mut entries);
    entries
}

/// 与 sidebar::scan_dir 相同的排序：目录在前，名称大小写不敏感。
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
}

/// 本机目录列举，与远端 [`parse_list`] 完全同策略（dotfiles / 排序 / 上限），
/// 这样本机与远程在文件树里的行为没有可见差异。
fn local_list_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for (scanned, entry) in std::fs::read_dir(dir)?.enumerate() {
        if scanned >= MAX_SCANNED_PAIRS || entries.len() > MAX_DIRECTORY_ENTRIES {
            break;
        }
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        entries.push(Entry {
            path: entry.path(),
            name,
            is_dir: entry.file_type()?.is_dir(),
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

fn host_at(hosts: &[RemoteHostConfig], index: usize) -> io::Result<&RemoteHostConfig> {
    hosts.get(index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("remote host #{index} is no longer configured"),
        )
    })
}

/// 远程操作的参数路径必须是绝对路径（探针脚本同样强制，这里是前端防线）。
fn require_absolute(path: &Path) -> io::Result<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("remote path must be absolute: {}", path.display()),
        ))
    }
}

/// 探针命令行走 String argv，不合法的 UTF-8 路径无法表达，明确拒绝。
fn path_str(path: &Path) -> io::Result<&str> {
    path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path is not valid UTF-8: {}", path.display()),
        )
    })
}

/// 目标已存在（含悬空符号链接）→ AlreadyExists，与探针的 `[ -e "$n" ]` 对齐；
/// `-e` 对悬空链接为假，但覆盖符号链接同样危险，故用 symlink_metadata。
fn ensure_absent(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("target already exists: {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn probe_op(host: &RemoteHostConfig, op: &str, args: &[&str]) -> io::Result<()> {
    let capture = run_probe(host, op, args, PROBE_OP_TIMEOUT, MAX_SMALL_OUTPUT)?;
    probe_output(capture).map(|_| ())
}

/// 列举目录。返回的条目数最多 MAX_DIRECTORY_ENTRIES + 1（截断信号，见上）。
pub fn list_dir(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    dir: &Path,
) -> io::Result<Vec<Entry>> {
    match loc {
        FsLocation::Local => local_list_dir(dir),
        FsLocation::Remote(index) => {
            let host = host_at(hosts, *index)?;
            require_absolute(dir)?;
            let capture = run_probe(
                host,
                "list",
                &[path_str(dir)?],
                PROBE_LIST_TIMEOUT,
                MAX_LIST_OUTPUT,
            )?;
            let output = probe_output(capture)?;
            Ok(parse_list(&output, dir))
        }
    }
}

/// 进入某个位置时的起始目录：本机沿用今天的行为（进程 cwd，失败回 `/`），
/// 远程取远端 `$HOME`（探针 `home`）。
pub fn start_dir(loc: &FsLocation, hosts: &[RemoteHostConfig]) -> io::Result<PathBuf> {
    match loc {
        FsLocation::Local => Ok(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))),
        FsLocation::Remote(index) => {
            let host = host_at(hosts, *index)?;
            let capture = run_probe(host, "home", &[], PROBE_LIST_TIMEOUT, MAX_SMALL_OUTPUT)?;
            let output = probe_output(capture)?;
            let path = PathBuf::from(String::from_utf8_lossy(&output).trim().to_string());
            require_absolute(&path)?;
            Ok(path)
        }
    }
}

/// 新建目录；已存在 → AlreadyExists。
pub fn create_dir(loc: &FsLocation, hosts: &[RemoteHostConfig], path: &Path) -> io::Result<()> {
    match loc {
        FsLocation::Local => std::fs::create_dir(path),
        FsLocation::Remote(index) => {
            let host = host_at(hosts, *index)?;
            require_absolute(path)?;
            probe_op(host, "mkdir", &[path_str(path)?])
        }
    }
}

/// 新建空文件；已存在 → AlreadyExists。
pub fn create_file(loc: &FsLocation, hosts: &[RemoteHostConfig], path: &Path) -> io::Result<()> {
    match loc {
        FsLocation::Local => std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(|_| ()),
        FsLocation::Remote(index) => {
            let host = host_at(hosts, *index)?;
            require_absolute(path)?;
            probe_op(host, "mkfile", &[path_str(path)?])
        }
    }
}

/// 删除文件或目录（目录递归删除；符号链接按链接本身删）。拒绝删除 `/`。
pub fn delete(loc: &FsLocation, hosts: &[RemoteHostConfig], path: &Path) -> io::Result<()> {
    if path == Path::new("/") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to delete /",
        ));
    }
    match loc {
        FsLocation::Local => {
            // 与探针一致：目录（非符号链接）递归删除，其余按文件删。
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            }
        }
        FsLocation::Remote(index) => {
            let host = host_at(hosts, *index)?;
            require_absolute(path)?;
            probe_op(host, "rm", &[path_str(path)?])
        }
    }
}

/// 重命名/移动；目标已存在 → AlreadyExists。
pub fn rename(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    match loc {
        FsLocation::Local => {
            ensure_absent(dst)?;
            std::fs::rename(src, dst)
        }
        FsLocation::Remote(index) => {
            let host = host_at(hosts, *index)?;
            require_absolute(src)?;
            require_absolute(dst)?;
            probe_op(host, "mv", &[path_str(src)?, path_str(dst)?])
        }
    }
}

/// 复制文件或目录（目录递归复制；符号链接按链接复制）；目标已存在 → AlreadyExists。
pub fn copy(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    match loc {
        FsLocation::Local => {
            ensure_absent(dst)?;
            copy_recursive(src, dst, 0)
        }
        FsLocation::Remote(index) => {
            let host = host_at(hosts, *index)?;
            require_absolute(src)?;
            require_absolute(dst)?;
            probe_op(host, "cp", &[path_str(src)?, path_str(dst)?])
        }
    }
}

/// 本地递归复制（对齐 `cp -a` 的形状，不带权限复制的细枝末节）。
fn copy_recursive(src: &Path, dst: &Path, depth: usize) -> io::Result<()> {
    if depth > MAX_COPY_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory nesting too deep to copy",
        ));
    }
    let metadata = std::fs::symlink_metadata(src)?;
    if metadata.is_dir() {
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()), depth + 1)?;
        }
        Ok(())
    } else if metadata.file_type().is_symlink() {
        // 复制链接本身，与 cp -a 对齐。
        let target = std::fs::read_link(src)?;
        std::os::unix::fs::symlink(target, dst)
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

// ---- 跨位置传输（上传 / 下载 / 本地中转） ----
//
// 全部流式执行，任何时刻内存里只有一个 64KB 块；字节帽
// [`MAX_TRANSFER_BYTES`] 在转发途中实时执行（超限即中止并清理部分数据）。
// 本地侧的"部分文件"用与目标同目录的隐藏名（`.name.fspart-<pid>`），
// 最终存在性检查通过后才 rename 就位（同目录 rename 是原子的）。

/// 与目标同目录的隐藏临时名（下载文件落盘用）。
fn part_path(dst: &Path) -> io::Result<PathBuf> {
    let name = dst
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "dst has no file name"))?;
    let mut temp = std::ffi::OsString::from(".");
    temp.push(name);
    temp.push(format!(".fspart-{}", std::process::id()));
    Ok(dst.with_file_name(temp))
}

/// 目录传输时本地 tar 中间文件的隐藏临时名。
fn part_tar_path(anchor: &Path) -> io::Result<PathBuf> {
    let name = anchor
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut temp = std::ffi::OsString::from(".");
    temp.push(name);
    temp.push(format!(".fspart-{}.tar", std::process::id()));
    Ok(anchor.with_file_name(temp))
}

/// 中转（远程 → 远程）的本地临时名：unique、用完即删。
fn relay_temp_path(is_dir: bool) -> PathBuf {
    static RELAY_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let suffix = if is_dir { ".tar" } else { "" };
    std::env::temp_dir().join(format!(
        "ember-fs-relay-{}-{}{}",
        std::process::id(),
        RELAY_COUNTER.fetch_add(1, Ordering::SeqCst),
        suffix
    ))
}

/// 部分文件就位前的最终存在性检查 + 原子 rename；失败由调用方清理 temp。
fn finalize_part(temp: &Path, dst: &Path) -> io::Result<()> {
    // 流式传输期间的竞态：目标被别人（或另一个实例）创建了。
    ensure_absent(dst)?;
    std::fs::rename(temp, dst)
}

/// 本地命令（tar 等）的退出状态检查，语义与 [`probe_output`] 对齐。
fn local_status(capture: Capture) -> io::Result<()> {
    if capture.cancelled {
        return Err(cancelled_error());
    }
    if capture.timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "local command timed out",
        ));
    }
    if capture.status == Some(0) {
        Ok(())
    } else {
        Err(io::Error::other(probe_stderr(&capture)))
    }
}

fn probe_output_empty(capture: Capture) -> io::Result<()> {
    probe_output(capture).map(|_| ())
}

/// 目录传输前确认本地有 tar：目录流就是 tar 格式，缺了它什么都传不了。
fn require_local_tar() -> io::Result<()> {
    let argv = vec!["tar".to_string(), "--version".to_string()];
    match run_capture(&argv, &[], Duration::from_secs(5), MAX_SMALL_OUTPUT) {
        Ok(capture) if capture.status == Some(0) => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "transferring directories requires the system tar command",
        )),
    }
}

/// 远端 stat 探针的解析结果：类型（d/f/l）与大小（f 为字节数，其余 0）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteStat {
    /// 探针的类型字符：b'd' / b'f' / b'l'（与 list 同一套约定）。
    pub kind: u8,
    pub size: u64,
}

/// 解析 `stat` 的一行输出："<t> <size>\n"。
fn parse_stat(bytes: &[u8]) -> Option<RemoteStat> {
    let line = bytes.split(|byte| *byte == b'\n').next()?;
    let space = line.iter().position(|byte| *byte == b' ')?;
    let kind = match &line[..space] {
        [kind @ (b'd' | b'f' | b'l')] => *kind,
        _ => return None,
    };
    let size = std::str::from_utf8(&line[space + 1..])
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(RemoteStat { kind, size })
}

/// 远端 stat：Ok(Some) 存在，Ok(None) 不存在（探针 3），Err 为传输/用法错误。
fn remote_stat(host: &RemoteHostConfig, path: &Path) -> io::Result<Option<RemoteStat>> {
    require_absolute(path)?;
    let capture = run_probe(
        host,
        "stat",
        &[path_str(path)?],
        PROBE_LIST_TIMEOUT,
        MAX_SMALL_OUTPUT,
    )?;
    match capture.status {
        Some(0) => {
            Ok(Some(parse_stat(&capture.stdout).ok_or_else(|| {
                io::Error::other("unparsable stat probe output")
            })?))
        }
        Some(3) => Ok(None),
        // 其余退出码/超时/取消走统一的错误映射。
        _ => probe_output(capture).map(|_| None),
    }
}

/// 目录/中转传输前的远端存在性预检：v3 起用 stat 一次探针完成（取代 v2 的
/// list+cat 双探针）。untar 自身的 17 检查才是 fail-closed 的权威防线；
/// 这里的预检只为不浪费本地打包/下载的字节。
fn remote_ensure_absent(host: &RemoteHostConfig, path: &Path) -> io::Result<()> {
    if remote_stat(host, path)?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("target already exists: {}", path.display()),
        ));
    }
    Ok(())
}

/// 跨位置传输（下载 / 上传 / 本地中转）的统一入口，在 op worker 上阻塞
/// 执行；返回最终落位路径。本机→本机不应走这里（那是 copy/rename），
/// 防御性报错。`control` 携带进度回报与取消令牌。
pub fn transfer(
    src_loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    src: &Path,
    src_is_dir: bool,
    dst_loc: &FsLocation,
    dst_dir: &Path,
    control: TransferControl,
) -> io::Result<PathBuf> {
    let name = src.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "source path has no file name")
    })?;
    let dst = dst_dir.join(name);
    match (src_loc, dst_loc) {
        (FsLocation::Remote(index), FsLocation::Local) => {
            let host = host_at(hosts, *index)?;
            download(host, src, src_is_dir, &dst, control)
        }
        (FsLocation::Local, FsLocation::Remote(index)) => {
            let host = host_at(hosts, *index)?;
            upload(host, src, src_is_dir, dst_dir, &dst, control)
        }
        (FsLocation::Remote(i), FsLocation::Remote(j)) if i != j => {
            let (src_host, dst_host) = (host_at(hosts, *i)?, host_at(hosts, *j)?);
            relay(src_host, src, src_is_dir, dst_host, dst_dir, &dst, control)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "same-location transfer should use copy/rename instead",
        )),
    }
    .map(|_| dst)
}

/// 两腿传输共享的进度链：每条腿一个带偏移量的 sink，底层是同一条节流，
/// 这样第二腿（上传/解包）的字节数接着第一腿（下载/打包）累计。
struct LegProgress {
    shared: Option<Arc<Mutex<ProgressSink>>>,
    cancel: Option<Arc<AtomicBool>>,
}

impl LegProgress {
    fn new(control: TransferControl) -> Self {
        Self {
            shared: control.progress.map(|sink| Arc::new(Mutex::new(sink))),
            cancel: control.cancel,
        }
    }

    fn control_for(&self, base: u64) -> TransferControl {
        TransferControl {
            progress: self.shared.clone().map(|shared| {
                ProgressSink::new(move |bytes| {
                    shared.lock().report(base + bytes);
                })
            }),
            cancel: self.cancel.clone(),
        }
    }
}

/// 下载：远端 → 本地。
fn download(
    host: &RemoteHostConfig,
    src: &Path,
    src_is_dir: bool,
    dst: &Path,
    control: TransferControl,
) -> io::Result<()> {
    require_absolute(src)?;
    let arg = path_str(src)?;
    if src_is_dir {
        download_dir(
            &probe_argv(host, "tar", &[arg]),
            dst,
            MAX_TRANSFER_BYTES,
            control,
        )
    } else {
        download_file(
            &probe_argv(host, "cat", &[arg]),
            dst,
            MAX_TRANSFER_BYTES,
            control,
        )
    }
}

/// 下载文件核心：argv 是完整的 cat 探针调用（测试可注入本机 sh）。
/// 目标存在性在流式传输前检查一次、rename 就位前再原子检查一次。
fn download_file(
    cat_argv: &[String],
    dst: &Path,
    max_bytes: u64,
    control: TransferControl,
) -> io::Result<()> {
    ensure_absent(dst)?;
    let temp = part_path(dst)?;
    // 上次崩溃可能留下同名部分文件（命名只有我们自己的 pid），清掉再来。
    let _ = std::fs::remove_file(&temp);
    let outcome = run_stream_to_file(
        cat_argv,
        PROBE_SCRIPT.as_bytes(),
        &temp,
        TRANSFER_TIMEOUT,
        max_bytes,
        control,
    )
    .and_then(probe_output_empty)
    .and_then(|()| finalize_part(&temp, dst));
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    outcome
}

/// 下载目录核心：远端 tar 流 → 本地临时 tar 文件（限额）→ 本地解包。
fn download_dir(
    tar_argv: &[String],
    dst: &Path,
    max_bytes: u64,
    control: TransferControl,
) -> io::Result<()> {
    require_local_tar()?;
    ensure_absent(dst)?;
    let temp = part_tar_path(dst)?;
    let _ = std::fs::remove_file(&temp);
    let cancel = control.cancel.clone();
    let downloaded = run_stream_to_file(
        tar_argv,
        PROBE_SCRIPT.as_bytes(),
        &temp,
        TRANSFER_TIMEOUT,
        max_bytes,
        control,
    )
    .and_then(probe_output_empty);
    let outcome = downloaded.and_then(|()| {
        let parent = dst.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "dst has no parent directory")
        })?;
        let argv = vec![
            "tar".to_string(),
            "xf".to_string(),
            path_str(&temp)?.to_string(),
            "-C".to_string(),
            path_str(parent)?.to_string(),
        ];
        run_capture_with_cancel(&argv, &[], TRANSFER_TIMEOUT, MAX_SMALL_OUTPUT, cancel)
            .and_then(local_status)
    });
    let _ = std::fs::remove_file(&temp);
    if outcome.is_err() && dst.exists() {
        // 解包失败：开传前已确认 dst 不存在，这个解了一半的目录是我们的，清掉。
        let _ = std::fs::remove_dir_all(dst);
    }
    outcome
}

/// 上传：本地 → 远端。
fn upload(
    host: &RemoteHostConfig,
    src: &Path,
    src_is_dir: bool,
    dst_dir: &Path,
    dst: &Path,
    control: TransferControl,
) -> io::Result<()> {
    require_absolute(dst_dir)?;
    host.validate().map_err(|problem| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("remote host {}: {problem}", host.display_name()),
        )
    })?;
    if src_is_dir {
        upload_dir(host, src, dst_dir, dst, MAX_TRANSFER_BYTES, control)
    } else {
        // put 在远端读流之前就检查 [ -e "$p" ] → 17，不浪费字节；
        // mv 就位前再查一次，存在性检查天然原子。
        let argv = sh_c_probe_argv(host, "put", &[path_str(dst)?]);
        upload_file(&argv, src, MAX_TRANSFER_BYTES, control)
    }
}

/// 上传文件核心：argv 是完整的 put 探针调用（测试可注入本机 sh）。
fn upload_file(
    put_argv: &[String],
    src: &Path,
    max_bytes: u64,
    control: TransferControl,
) -> io::Result<()> {
    run_stream_from_file(put_argv, src, TRANSFER_TIMEOUT, max_bytes, control)
        .and_then(probe_output_empty)
}

/// 上传目录核心：远端存在性预检 → 本地打包（限额）→ 远端解包。
fn upload_dir(
    host: &RemoteHostConfig,
    src: &Path,
    dst_dir: &Path,
    dst: &Path,
    max_bytes: u64,
    control: TransferControl,
) -> io::Result<()> {
    require_local_tar()?;
    remote_ensure_absent(host, dst)?;
    let parent = src.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "source has no parent directory",
        )
    })?;
    let name = src
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "source has no UTF-8 file name")
        })?;
    let tar_argv = vec![
        "tar".to_string(),
        "cf".to_string(),
        "-".to_string(),
        "-C".to_string(),
        path_str(parent)?.to_string(),
        name.to_string(),
    ];
    let legs = LegProgress::new(control);
    let temp = part_tar_path(src)?;
    let _ = std::fs::remove_file(&temp);
    let packed = run_stream_to_file(
        &tar_argv,
        &[],
        &temp,
        TRANSFER_TIMEOUT,
        max_bytes,
        legs.control_for(0),
    )
    .and_then(local_status);
    let outcome = packed.and_then(|()| {
        // 解包腿的字节数接着打包腿累计。untar v3 在解包前原子拒绝
        // 已存在的 <dir>/<name>（检查与解包之间仍有微秒级 TOCTOU 窗口，
        // 这是 tar 合并语义的协议极限，Friendly 错误由 17 映射给出）。
        let base = std::fs::metadata(&temp).map(|meta| meta.len()).unwrap_or(0);
        let untar_argv = sh_c_probe_argv(host, "untar", &[path_str(dst_dir)?, name]);
        run_stream_from_file(
            &untar_argv,
            &temp,
            TRANSFER_TIMEOUT,
            max_bytes,
            legs.control_for(base),
        )
        .and_then(probe_output_empty)
    });
    let _ = std::fs::remove_file(&temp);
    outcome
}

/// 中转：远程 i → 本地唯一临时文件 → 远程 j，用完即删。
fn relay(
    src_host: &RemoteHostConfig,
    src: &Path,
    src_is_dir: bool,
    dst_host: &RemoteHostConfig,
    dst_dir: &Path,
    dst: &Path,
    control: TransferControl,
) -> io::Result<()> {
    require_absolute(src)?;
    require_absolute(dst_dir)?;
    // 下载腿会先烧掉流量，所以存在性预检提前做（文件靠 put 的 17 兜底
    // 也来得及，但那时下载已经完成，白传一份）。
    remote_ensure_absent(dst_host, dst)?;
    let legs = LegProgress::new(control);
    let temp = relay_temp_path(src_is_dir);
    let _ = std::fs::remove_file(&temp);
    let src_arg = path_str(src)?;
    let download_op = if src_is_dir { "tar" } else { "cat" };
    let outcome = run_stream_to_file(
        &probe_argv(src_host, download_op, &[src_arg]),
        PROBE_SCRIPT.as_bytes(),
        &temp,
        TRANSFER_TIMEOUT,
        MAX_TRANSFER_BYTES,
        legs.control_for(0),
    )
    .and_then(probe_output_empty)
    .and_then(|()| {
        // 上传腿的字节数接着下载腿累计。
        let base = std::fs::metadata(&temp).map(|meta| meta.len()).unwrap_or(0);
        let upload_argv = if src_is_dir {
            // tar 流的顶层名就是 src 的 basename（tar 探针 -C 父目录打包）。
            let name = src
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "source has no UTF-8 file name")
                })?;
            sh_c_probe_argv(dst_host, "untar", &[path_str(dst_dir)?, name])
        } else {
            sh_c_probe_argv(dst_host, "put", &[path_str(dst)?])
        };
        run_stream_from_file(
            &upload_argv,
            &temp,
            TRANSFER_TIMEOUT,
            MAX_TRANSFER_BYTES,
            legs.control_for(base),
        )
        .and_then(probe_output_empty)
    });
    let _ = std::fs::remove_file(&temp);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 唯一临时目录，Drop 时递归清理。
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ember-remote-fs-test-{}-{}",
                std::process::id(),
                TEST_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir(&path).expect("create test dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ssh_host() -> RemoteHostConfig {
        toml::from_str(
            r#"
name = "devbox"
host = "dev.example.com"
user = "yj"
ssh_args = ["-p", "2222"]
"#,
        )
        .expect("ssh host")
    }

    fn docker_host() -> RemoteHostConfig {
        toml::from_str(
            r#"
name = "myubuntu"
host = "myubuntu"
user = "devuser"
docker = true
"#,
        )
        .expect("docker host")
    }

    fn capture(status: Option<i32>, stderr: &str) -> Capture {
        Capture {
            status,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
            timed_out: false,
            cancelled: false,
        }
    }

    // ---- 引用与 argv 构造 ----

    #[test]
    fn sq_escapes_single_quotes_posix_style() {
        assert_eq!(sq("plain"), "'plain'");
        assert_eq!(sq(""), "''");
        assert_eq!(sq("it's"), "'it'\\''s'");
        assert_eq!(sq("a'b'c"), "'a'\\''b'\\''c'");
    }

    #[test]
    fn probe_command_quotes_arguments_but_not_the_op() {
        assert_eq!(probe_command("home", &[]), "sh -s -- home");
        assert_eq!(
            probe_command("list", &["/tmp/a b", "$(touch /tmp/pwned)"]),
            "sh -s -- list '/tmp/a b' '$(touch /tmp/pwned)'"
        );
        // 单引号在参数里必须被终止-转义-重开，不能留在引号语境内被解析。
        assert_eq!(
            probe_command("mv", &["/a/x'y", "/b"]),
            "sh -s -- mv '/a/x'\\''y' '/b'"
        );
    }

    #[test]
    fn ssh_argv_is_batch_mode_single_command_element() {
        let argv = probe_argv(&ssh_host(), "list", &["/var/log"]);
        assert_eq!(
            argv,
            vec![
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-p",
                "2222",
                "--",
                "yj@dev.example.com",
                "sh -s -- list '/var/log'",
            ]
        );
        // 远端命令必须是恰好一个 argv 元素，ssh 才会把它当一条命令重解析。
        assert_eq!(argv.len(), 10);
    }

    #[test]
    fn ssh_argv_without_user_uses_bare_host() {
        let host: RemoteHostConfig = toml::from_str(r#"host = "builder""#).unwrap();
        let argv = probe_argv(&host, "home", &[]);
        assert_eq!(argv[argv.len() - 2], "builder");
        assert_eq!(argv[argv.len() - 1], "sh -s -- home");
    }

    #[test]
    fn docker_argv_is_raw_and_never_allocates_a_tty() {
        let argv = probe_argv(&docker_host(), "mv", &["/a b/c", "/d"]);
        assert_eq!(
            argv,
            vec![
                "docker", "exec", "-i", "-u", "devuser", "myubuntu", "sh", "-s", "--", "mv",
                "/a b/c", "/d",
            ]
        );
        assert!(!argv.iter().any(|arg| arg == "-t"), "{argv:?}");

        let host: RemoteHostConfig = toml::from_str("host = \"c1\"\ndocker = true").unwrap();
        let argv = probe_argv(&host, "home", &[]);
        assert_eq!(
            argv,
            vec!["docker", "exec", "-i", "c1", "sh", "-s", "--", "home"]
        );
    }

    #[test]
    fn sh_c_argv_inlines_the_script_and_keeps_positional_layout() {
        // put/untar 走 sh -c 内联脚本：$1=op、$2=路径，stdin 整个留给载荷。
        let argv = sh_c_probe_argv(&docker_host(), "put", &["/tmp/dst file"]);
        assert_eq!(
            argv,
            vec![
                "docker",
                "exec",
                "-i",
                "-u",
                "devuser",
                "myubuntu",
                "sh",
                "-c",
                PROBE_SCRIPT,
                "remote-fs-probe",
                "put",
                "/tmp/dst file",
            ]
        );
        assert!(!argv.iter().any(|arg| arg == "-t"), "{argv:?}");

        let argv = sh_c_probe_argv(&ssh_host(), "untar", &["/data"]);
        let cmd = argv.last().unwrap();
        assert!(cmd.starts_with("sh -c '"), "{cmd}");
        assert!(cmd.ends_with(" remote-fs-probe untar '/data'"), "{cmd}");
        // ssh 场景下远端命令仍必须是恰好一个 argv 元素。
        assert_eq!(argv.len(), 10);
    }

    // ---- list 输出解析 ----

    #[test]
    fn parse_list_reads_types_and_sorts_dirs_first() {
        let bytes = b"f\0zeta.txt\0d\0subdir\0l\0alink\0f\0Alpha.txt\0";
        let entries = parse_list(bytes, Path::new("/base"));
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // 目录在前；文件按小写名称排序（alink < alpha.txt < zeta.txt）。
        assert_eq!(names, vec!["subdir", "alink", "Alpha.txt", "zeta.txt"]);
        assert!(entries[0].is_dir);
        // l 按文件处理，不展开成目录。
        assert!(!entries[1].is_dir);
        assert_eq!(entries[0].path, PathBuf::from("/base/subdir"));
    }

    #[test]
    fn parse_list_filters_dotfiles_and_dangerous_names() {
        let bytes = b"f\0.visible\0d\0..\0f\0.\0f\0a/b\0f\0\0f\0kept.txt\0x\0bogus\0";
        let entries = parse_list(bytes, Path::new("/base"));
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // 未知类型 x 同样丢弃；剩下的只有 kept.txt。
        assert_eq!(names, vec!["kept.txt"]);
    }

    #[test]
    fn parse_list_tolerates_spaces_newlines_and_non_utf8_names() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"f\0my file.txt\0");
        bytes.extend_from_slice(b"f\0line\nbreak\0");
        bytes.extend_from_slice(b"f\0bad\xffname\0");
        let entries = parse_list(&bytes, Path::new("/base"));
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["bad\u{fffd}name", "line\nbreak", "my file.txt"]);
    }

    #[test]
    fn parse_list_ignores_a_trailing_partial_pair() {
        let bytes = b"f\0a.txt\0d\0";
        let entries = parse_list(bytes, Path::new("/base"));
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt"]);
    }

    #[test]
    fn parse_list_keeps_at_most_max_plus_one_entries() {
        let mut bytes = Vec::new();
        for index in 0..MAX_DIRECTORY_ENTRIES + 50 {
            bytes.extend_from_slice(format!("f\0file-{index:06}\0").as_bytes());
        }
        let entries = parse_list(&bytes, Path::new("/base"));
        // 第 MAX+1 条是截断信号；解析自身有界，绝不全量读入。
        assert_eq!(entries.len(), MAX_DIRECTORY_ENTRIES + 1);
    }

    // ---- 校验 ----

    #[test]
    fn new_name_validation() {
        assert!(validate_new_name("notes.md").is_ok());
        assert!(validate_new_name("a".repeat(255).as_str()).is_ok());
        assert!(validate_new_name("").is_err());
        assert!(validate_new_name("a".repeat(256).as_str()).is_err());
        assert!(validate_new_name("a/b").is_err());
        assert!(validate_new_name("a\0b").is_err());
        assert!(validate_new_name(".").is_err());
        assert!(validate_new_name("..").is_err());
    }

    #[test]
    fn remote_ops_reject_relative_paths_and_unknown_hosts_before_spawning() {
        let hosts = vec![ssh_host()];
        let relative = Path::new("etc/hostname");
        for result in [
            create_dir(&FsLocation::Remote(0), &hosts, relative),
            create_file(&FsLocation::Remote(0), &hosts, relative),
            delete(&FsLocation::Remote(0), &hosts, relative),
            rename(&FsLocation::Remote(0), &hosts, relative, Path::new("/x")),
            copy(&FsLocation::Remote(0), &hosts, Path::new("/x"), relative),
        ] {
            let error = result.expect_err("relative path must fail before spawning ssh");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
        // 主机下标越界也要在 spawn 之前失败。
        let error = list_dir(&FsLocation::Remote(9), &hosts, Path::new("/"))
            .expect_err("unknown host index");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn deleting_root_is_refused_on_both_backends() {
        for loc in [FsLocation::Local, FsLocation::Remote(0)] {
            let error = delete(&loc, &[ssh_host()], Path::new("/")).expect_err("delete /");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    // ---- 本地操作（真实文件系统） ----

    #[test]
    fn local_create_refuses_existing_targets() {
        let dir = TestDir::new();
        let sub = dir.join("sub");
        create_dir(&FsLocation::Local, &[], &sub).unwrap();
        let error = create_dir(&FsLocation::Local, &[], &sub).expect_err("mkdir again");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

        let file = dir.join("note.txt");
        create_file(&FsLocation::Local, &[], &file).unwrap();
        assert!(file.exists());
        let error = create_file(&FsLocation::Local, &[], &file).expect_err("mkfile again");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn local_delete_removes_files_dirs_and_symlinks() {
        let dir = TestDir::new();
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("inner.txt"), b"data").unwrap();
        delete(&FsLocation::Local, &[], &sub).unwrap();
        assert!(!sub.exists());

        let file = dir.join("note.txt");
        std::fs::write(&file, b"data").unwrap();
        delete(&FsLocation::Local, &[], &file).unwrap();
        assert!(!file.exists());

        // 指向目录的符号链接按链接删除，目标目录不受影响。
        let target = dir.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep.txt"), b"data").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        delete(&FsLocation::Local, &[], &link).unwrap();
        assert!(target.join("keep.txt").exists());
        assert!(std::fs::symlink_metadata(&link).is_err());
    }

    #[test]
    fn local_rename_moves_and_refuses_existing_destination() {
        let dir = TestDir::new();
        let src = dir.join("old.txt");
        std::fs::write(&src, b"data").unwrap();
        let dst = dir.join("new.txt");
        rename(&FsLocation::Local, &[], &src, &dst).unwrap();
        assert!(!src.exists());
        assert_eq!(std::fs::read(&dst).unwrap(), b"data");

        let other = dir.join("other.txt");
        std::fs::write(&other, b"x").unwrap();
        let error = rename(&FsLocation::Local, &[], &other, &dst).expect_err("rename onto");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn local_copy_recurses_and_preserves_symlinks() {
        let dir = TestDir::new();
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("nested/file.txt"), b"data").unwrap();
        std::os::unix::fs::symlink("nested/file.txt", src.join("rel-link")).unwrap();

        let dst = dir.join("dst");
        copy(&FsLocation::Local, &[], &src, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("nested/file.txt")).unwrap(), b"data");
        let link_meta = std::fs::symlink_metadata(dst.join("rel-link")).unwrap();
        assert!(link_meta.file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(dst.join("rel-link")).unwrap(),
            PathBuf::from("nested/file.txt")
        );

        let error = copy(&FsLocation::Local, &[], &src, &dst).expect_err("copy onto");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn local_list_dir_matches_scan_dir_policy() {
        let dir = TestDir::new();
        std::fs::create_dir(dir.join("subdir")).unwrap();
        std::fs::write(dir.join("b.txt"), b"").unwrap();
        std::fs::write(dir.join("A.txt"), b"").unwrap();
        std::fs::write(dir.join(".hidden"), b"").unwrap();
        let entries = list_dir(&FsLocation::Local, &[], dir.path()).unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["subdir", "A.txt", "b.txt"]);
        assert!(entries[0].is_dir);
        assert!(!entries[1].is_dir);
    }

    #[test]
    fn start_dir_local_is_the_process_cwd() {
        let dir = start_dir(&FsLocation::Local, &[]).unwrap();
        assert_eq!(dir, std::env::current_dir().unwrap());
    }

    // ---- 剪贴板 ----

    #[test]
    fn paste_destination_joins_the_source_file_name() {
        let clipboard = FsClipboard {
            loc: FsLocation::Local,
            path: PathBuf::from("/home/yj/notes.md"),
            is_dir: false,
            cut: false,
        };
        assert_eq!(
            clipboard.paste_destination(Path::new("/tmp/target")),
            Some(PathBuf::from("/tmp/target/notes.md"))
        );
        let root_clipboard = FsClipboard {
            loc: FsLocation::Local,
            path: PathBuf::from("/"),
            is_dir: true,
            cut: true,
        };
        assert_eq!(root_clipboard.paste_destination(Path::new("/tmp")), None);
    }

    #[test]
    fn location_labels() {
        let hosts = vec![ssh_host(), docker_host()];
        assert_eq!(FsLocation::Local.label(&hosts), "Local");
        assert_eq!(FsLocation::Remote(0).label(&hosts), "ssh: devbox");
        assert_eq!(FsLocation::Remote(1).label(&hosts), "docker: myubuntu");
        assert!(FsLocation::Remote(9).label(&hosts).contains('#'));
    }

    // ---- run_capture ----

    #[test]
    fn run_capture_captures_output_and_exit_status() {
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf hello; printf oops >&2; exit 17".to_string(),
        ];
        let capture = run_capture(&argv, &[], Duration::from_secs(5), 1024).unwrap();
        assert_eq!(capture.status, Some(17));
        assert_eq!(capture.stdout, b"hello");
        assert_eq!(capture.stderr, b"oops");
        assert!(!capture.timed_out);
    }

    #[test]
    fn run_capture_kills_a_runaway_child() {
        let argv = vec!["sh".to_string(), "-c".to_string(), "sleep 30".to_string()];
        let capture = run_capture(&argv, &[], Duration::from_millis(150), 1024).unwrap();
        assert!(capture.timed_out);
        assert_eq!(capture.status, None);
    }

    #[test]
    fn run_capture_bounds_stdout() {
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "head -c 100000 /dev/zero | tr '\\0' 'a'".to_string(),
        ];
        let capture = run_capture(&argv, &[], Duration::from_secs(5), 1024).unwrap();
        assert_eq!(capture.stdout.len(), 1024);
    }

    #[test]
    fn probe_output_maps_exit_codes() {
        let error = probe_output(capture(Some(17), "")).expect_err("17");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        let error = probe_output(capture(Some(3), "")).expect_err("3");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let error = probe_output(capture(Some(2), "usage")).expect_err("2");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let error = probe_output(capture(Some(4), "disk full\nmore detail")).expect_err("4");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.to_string(), "disk full");
        let error = probe_output(capture(Some(127), "")).expect_err("127");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("127"));
        let mut timed = capture(None, "");
        timed.timed_out = true;
        let error = probe_output(timed).expect_err("timeout");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    // ---- 探针脚本端到端（本机 sh，不触网） ----

    /// 直接对系统 sh 跑 PROBE_SCRIPT，返回原始 Capture。
    fn run_probe_locally(args: &[&str]) -> Capture {
        let mut argv = vec!["sh".to_string(), "-s".to_string(), "--".to_string()];
        argv.extend(args.iter().map(|arg| arg.to_string()));
        run_capture(
            &argv,
            PROBE_SCRIPT.as_bytes(),
            Duration::from_secs(5),
            MAX_LIST_OUTPUT,
        )
        .expect("spawn sh")
    }

    #[test]
    fn probe_script_list_and_home_work_under_real_sh() {
        let dir = TestDir::new();
        std::fs::create_dir(dir.join("sub dir")).unwrap();
        std::fs::write(dir.join("file.txt"), b"x").unwrap();
        std::fs::write(dir.join(".hidden"), b"x").unwrap();

        let dir_arg = dir.path().to_str().unwrap().to_string();
        let capture = run_probe_locally(&["list", &dir_arg]);
        assert_eq!(capture.status, Some(0), "stderr: {:?}", capture.stderr);
        let entries = parse_list(&capture.stdout, dir.path());
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // dotfile 在 Rust 侧过滤；空格名原样保留。
        assert_eq!(names, vec!["sub dir", "file.txt"]);
        assert!(entries[0].is_dir);

        let capture = run_probe_locally(&["home"]);
        assert_eq!(capture.status, Some(0));
        let home = String::from_utf8_lossy(&capture.stdout).trim().to_string();
        assert!(home.starts_with('/'), "home: {home:?}");
    }

    #[test]
    fn probe_script_exit_codes_match_the_protocol() {
        let dir = TestDir::new();
        let dir_arg = dir.path().to_str().unwrap().to_string();

        // 相对路径 → 2；不存在的目录 → 3；未知 op → 2。
        assert_eq!(
            run_probe_locally(&["list", "relative/path"]).status,
            Some(2)
        );
        let missing = dir.join("missing");
        assert_eq!(
            run_probe_locally(&["list", missing.to_str().unwrap()]).status,
            Some(3)
        );
        assert_eq!(run_probe_locally(&["bogus"]).status, Some(2));
        assert_eq!(run_probe_locally(&[]).status, Some(2));

        // mkdir 正常 + 已存在 17。
        let sub = dir.join("sub");
        let sub_arg = sub.to_str().unwrap().to_string();
        assert_eq!(run_probe_locally(&["mkdir", &sub_arg]).status, Some(0));
        assert!(sub.is_dir());
        assert_eq!(run_probe_locally(&["mkdir", &sub_arg]).status, Some(17));

        // mkfile 正常 + 已存在 17。
        let file = dir.join("note.txt");
        let file_arg = file.to_str().unwrap().to_string();
        assert_eq!(run_probe_locally(&["mkfile", &file_arg]).status, Some(0));
        assert!(file.is_file());
        assert_eq!(run_probe_locally(&["mkfile", &file_arg]).status, Some(17));

        // mv 正常 + 目标已存在 17。
        let renamed = dir.join("renamed.txt");
        let renamed_arg = renamed.to_str().unwrap().to_string();
        assert_eq!(
            run_probe_locally(&["mv", &file_arg, &renamed_arg]).status,
            Some(0)
        );
        assert!(!file.exists() && renamed.is_file());
        assert_eq!(
            run_probe_locally(&["mv", &renamed_arg, &sub_arg]).status,
            Some(17)
        );

        // cp 递归正常 + 目标已存在 17。
        let copied = dir.join("sub-copy");
        let copied_arg = copied.to_str().unwrap().to_string();
        assert_eq!(
            run_probe_locally(&["cp", &sub_arg, &copied_arg]).status,
            Some(0)
        );
        assert!(copied.is_dir());
        assert_eq!(
            run_probe_locally(&["cp", &sub_arg, &copied_arg]).status,
            Some(17)
        );

        // rm：拒绝 "/"，文件与目录都能删。
        assert_eq!(run_probe_locally(&["rm", "/"]).status, Some(2));
        assert_eq!(run_probe_locally(&["rm", &renamed_arg]).status, Some(0));
        assert!(!renamed.exists());
        assert_eq!(run_probe_locally(&["rm", &copied_arg]).status, Some(0));
        assert!(!copied.exists());

        // 目录里还有内容时 rm 递归删除。
        std::fs::write(sub.join("inner.txt"), b"x").unwrap();
        assert_eq!(run_probe_locally(&["rm", &sub_arg]).status, Some(0));
        assert!(!sub.exists());

        let _ = dir_arg;
    }

    // ---- 探针 v2 流式 op（本机 sh 端到端，不触网） ----

    /// put/untar 的本机 sh -c 调用（与 sh_c_probe_argv 的 docker 形态同构）。
    fn sh_c_argv_locally(op: &str, args: &[&str]) -> Vec<String> {
        let mut argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            PROBE_SCRIPT.to_string(),
            "remote-fs-probe".to_string(),
            op.to_string(),
        ];
        argv.extend(args.iter().map(|arg| arg.to_string()));
        argv
    }

    /// cat/tar 的本机 sh -s 调用（脚本走 stdin）。
    fn sh_s_argv_locally(op: &str, args: &[&str]) -> Vec<String> {
        let mut argv = vec![
            "sh".to_string(),
            "-s".to_string(),
            "--".to_string(),
            op.to_string(),
        ];
        argv.extend(args.iter().map(|arg| arg.to_string()));
        argv
    }

    /// 覆盖全部 256 个字节值的二进制样本。
    fn binary_sample() -> Vec<u8> {
        (0..=255u8).cycle().take(4096).collect()
    }

    #[test]
    fn probe_v2_cat_streams_binary_and_rejects_non_files() {
        let dir = TestDir::new();
        let file = dir.join("blob.bin");
        std::fs::write(&file, binary_sample()).unwrap();
        let arg = file.to_str().unwrap().to_string();

        let capture = run_capture(
            &sh_s_argv_locally("cat", &[&arg]),
            PROBE_SCRIPT.as_bytes(),
            Duration::from_secs(5),
            MAX_LIST_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(0), "stderr: {:?}", capture.stderr);
        assert_eq!(capture.stdout, binary_sample());

        // 目录与缺失路径都不是"可读普通文件" → 3。
        let dir_arg = dir.path().to_str().unwrap().to_string();
        for target in [dir_arg, dir.join("missing").to_str().unwrap().to_string()] {
            let capture = run_capture(
                &sh_s_argv_locally("cat", &[&target]),
                PROBE_SCRIPT.as_bytes(),
                Duration::from_secs(5),
                MAX_LIST_OUTPUT,
            )
            .unwrap();
            assert_eq!(capture.status, Some(3), "target: {target}");
        }
    }

    #[test]
    fn probe_v2_put_writes_stdin_atomically_and_refuses_existing() {
        let dir = TestDir::new();
        let file = dir.join("upload.bin");
        let arg = file.to_str().unwrap().to_string();
        let payload = binary_sample();

        let capture = run_capture(
            &sh_c_argv_locally("put", &[&arg]),
            &payload,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(0), "stderr: {:?}", capture.stderr);
        assert_eq!(std::fs::read(&file).unwrap(), payload);

        // 已存在 → 17，且临时文件不残留。
        let capture = run_capture(
            &sh_c_argv_locally("put", &[&arg]),
            &payload,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(17));
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".fspart."))
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");

        // 目标目录不存在 → 4。
        let bad = dir.join("missing/file").to_str().unwrap().to_string();
        let capture = run_capture(
            &sh_c_argv_locally("put", &[&bad]),
            &payload,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(4));
    }

    #[test]
    fn probe_v2_tar_untar_round_trip_preserves_tree() {
        let dir = TestDir::new();
        let src = dir.join("tree");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/blob.bin"), binary_sample()).unwrap();
        std::fs::write(src.join("note.txt"), b"hello").unwrap();
        let src_arg = src.to_str().unwrap().to_string();

        // tar 打包流。
        let capture = run_capture(
            &sh_s_argv_locally("tar", &[&src_arg]),
            PROBE_SCRIPT.as_bytes(),
            Duration::from_secs(5),
            MAX_LIST_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(0), "stderr: {:?}", capture.stderr);
        assert!(!capture.stdout.is_empty());

        // 文件不是目录 → 3。
        let file_arg = src.join("note.txt").to_str().unwrap().to_string();
        let capture = run_capture(
            &sh_s_argv_locally("tar", &[&file_arg]),
            PROBE_SCRIPT.as_bytes(),
            Duration::from_secs(5),
            MAX_LIST_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(3));

        // untar 解进已存在的目录（v3：参数为 <dir> <name>，name 是 tar 流顶层名）。
        let out = dir.join("out");
        std::fs::create_dir(&out).unwrap();
        let out_arg = out.to_str().unwrap().to_string();
        let tar_bytes = run_capture(
            &sh_s_argv_locally("tar", &[&src_arg]),
            PROBE_SCRIPT.as_bytes(),
            Duration::from_secs(5),
            MAX_LIST_OUTPUT,
        )
        .unwrap()
        .stdout;
        let capture = run_capture(
            &sh_c_argv_locally("untar", &[&out_arg, "tree"]),
            &tar_bytes,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(0), "stderr: {:?}", capture.stderr);
        assert_eq!(
            std::fs::read(out.join("tree/sub/blob.bin")).unwrap(),
            binary_sample()
        );
        assert_eq!(std::fs::read(out.join("tree/note.txt")).unwrap(), b"hello");

        // v3：目标已存在 → 解包前直接 17，不合并不覆盖。
        let capture = run_capture(
            &sh_c_argv_locally("untar", &[&out_arg, "tree"]),
            &tar_bytes,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(17));

        // 目标目录缺失 → 3；name 带 / 或为空 → 2。
        let missing = dir.join("missing").to_str().unwrap().to_string();
        let capture = run_capture(
            &sh_c_argv_locally("untar", &[&missing, "tree"]),
            &tar_bytes,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(3));
        let capture = run_capture(
            &sh_c_argv_locally("untar", &[&out_arg, "a/b"]),
            &tar_bytes,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(2));
        let capture = run_capture(
            &sh_c_argv_locally("untar", &[&out_arg]),
            &tar_bytes,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(2));
    }

    #[test]
    fn probe_v3_stat_reports_type_size_and_missing() {
        let dir = TestDir::new();
        let file = dir.join("blob.bin");
        std::fs::write(&file, binary_sample()).unwrap();
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();

        let stat = |path: &Path| {
            run_capture(
                &sh_s_argv_locally("stat", &[path.to_str().unwrap()]),
                PROBE_SCRIPT.as_bytes(),
                Duration::from_secs(5),
                MAX_SMALL_OUTPUT,
            )
            .unwrap()
        };
        let capture = stat(&file);
        assert_eq!(capture.status, Some(0));
        assert_eq!(
            parse_stat(&capture.stdout),
            Some(RemoteStat {
                kind: b'f',
                size: binary_sample().len() as u64,
            })
        );
        assert_eq!(
            parse_stat(&stat(&sub).stdout),
            Some(RemoteStat {
                kind: b'd',
                size: 0
            })
        );
        assert_eq!(
            parse_stat(&stat(&link).stdout),
            Some(RemoteStat {
                kind: b'l',
                size: 0
            })
        );
        assert_eq!(stat(&dir.join("missing")).status, Some(3));

        // 解析对垃圾输入防御性失败。
        assert_eq!(parse_stat(b""), None);
        assert_eq!(parse_stat(b"x 1"), None);
        assert_eq!(parse_stat(b"f"), None);
        assert_eq!(parse_stat(b"f abc"), None);
    }

    // ---- 流式 runner 与传输组合（本机 sh 当"远端"） ----

    #[test]
    fn run_stream_to_file_enforces_the_cap_and_download_cleans_up() {
        let dir = TestDir::new();
        let big = dir.join("big.bin");
        std::fs::write(&big, vec![7u8; 16 * 1024]).unwrap();
        let big_arg = big.to_str().unwrap().to_string();
        let dst = dir.join("dst.bin");

        // 1KB 帽下下载 16KB：报错、部分文件被 download_file 清掉、dst 不出现。
        let error = download_file(
            &sh_s_argv_locally("cat", &[&big_arg]),
            &dst,
            1024,
            TransferControl::default(),
        )
        .expect_err("cap must abort the download");
        assert!(error.to_string().contains("limit"), "{error}");
        assert!(!dst.exists());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".fspart-"))
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
    }

    #[test]
    fn run_stream_from_file_prechecks_size_before_spawning() {
        let dir = TestDir::new();
        let big = dir.join("big.bin");
        std::fs::write(&big, vec![7u8; 16 * 1024]).unwrap();
        let dst = dir.join("dst.bin");
        let dst_arg = dst.to_str().unwrap().to_string();

        // 预检直接报错：子进程从未运行，远端（本机）不出现任何文件。
        let error = upload_file(
            &sh_c_argv_locally("put", &[&dst_arg]),
            &big,
            1024,
            TransferControl::default(),
        )
        .expect_err("oversized upload must fail before streaming");
        assert!(error.to_string().contains("limit"), "{error}");
        assert!(!dst.exists());
    }

    #[test]
    fn download_file_refuses_an_existing_dst_before_streaming() {
        let dir = TestDir::new();
        let src = dir.join("src.bin");
        std::fs::write(&src, binary_sample()).unwrap();
        let src_arg = src.to_str().unwrap().to_string();
        let dst = dir.join("dst.bin");
        std::fs::write(&dst, b"keep me").unwrap();

        let error = download_file(
            &sh_s_argv_locally("cat", &[&src_arg]),
            &dst,
            MAX_TRANSFER_BYTES,
            TransferControl::default(),
        )
        .expect_err("existing dst");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        // 原内容原样保留，连临时文件都没有出现过。
        assert_eq!(std::fs::read(&dst).unwrap(), b"keep me");
    }

    #[test]
    fn finalize_part_rechecks_existence_before_the_atomic_rename() {
        let dir = TestDir::new();
        let temp = dir.join(".name.fspart-test");
        std::fs::write(&temp, binary_sample()).unwrap();
        let dst = dir.join("name");
        std::fs::write(&dst, b"raced in").unwrap();

        // 流式传输完成后目标被别人创建：AlreadyExists，调用方清 temp。
        let error = finalize_part(&temp, &dst).expect_err("final existence check");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        let _ = std::fs::remove_file(&temp);
        assert_eq!(std::fs::read(&dst).unwrap(), b"raced in");

        // 正常路径：rename 就位，内容完整。
        std::fs::write(&temp, binary_sample()).unwrap();
        let dst = dir.join("other");
        finalize_part(&temp, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), binary_sample());
        assert!(!temp.exists());
    }

    #[test]
    fn download_then_upload_round_trips_binary_like_a_relay() {
        let dir = TestDir::new();
        // "远端 A"的文件。
        let remote_a = dir.join("a");
        std::fs::create_dir(&remote_a).unwrap();
        std::fs::write(remote_a.join("blob.bin"), binary_sample()).unwrap();
        // 中转临时文件 → "远端 B"。
        let relay_temp = dir.join("relay-temp");
        let remote_b = dir.join("b");
        std::fs::create_dir(&remote_b).unwrap();
        let final_path = remote_b.join("blob.bin");

        let src_arg = remote_a.join("blob.bin").to_str().unwrap().to_string();
        download_file(
            &sh_s_argv_locally("cat", &[&src_arg]),
            &relay_temp,
            MAX_TRANSFER_BYTES,
            TransferControl::default(),
        )
        .unwrap();
        let dst_arg = final_path.to_str().unwrap().to_string();
        upload_file(
            &sh_c_argv_locally("put", &[&dst_arg]),
            &relay_temp,
            MAX_TRANSFER_BYTES,
            TransferControl::default(),
        )
        .unwrap();
        std::fs::remove_file(&relay_temp).unwrap();
        assert_eq!(std::fs::read(&final_path).unwrap(), binary_sample());
    }

    #[test]
    fn cut_transfer_deletes_source_only_after_a_successful_copy() {
        let dir = TestDir::new();
        let src_dir = dir.join("src-side");
        let dst_dir = dir.join("dst-side");
        std::fs::create_dir(&src_dir).unwrap();
        std::fs::create_dir(&dst_dir).unwrap();
        let src = src_dir.join("move.bin");
        std::fs::write(&src, binary_sample()).unwrap();
        let dst = dst_dir.join("move.bin");

        // 复制成功 → 删源成功：源消失、目标完整（execute_op 的组合顺序）。
        let src_arg = src.to_str().unwrap().to_string();
        download_file(
            &sh_s_argv_locally("cat", &[&src_arg]),
            &dst,
            MAX_TRANSFER_BYTES,
            TransferControl::default(),
        )
        .unwrap();
        delete(&FsLocation::Local, &[], &src).unwrap();
        assert!(!src.exists());
        assert_eq!(std::fs::read(&dst).unwrap(), binary_sample());

        // 删源失败（父目录只读）→ 部分成功：目标已就位，源保留待人工处理。
        let src2 = src_dir.join("move2.bin");
        std::fs::write(&src2, binary_sample()).unwrap();
        let dst2 = dst_dir.join("move2.bin");
        let src2_arg = src2.to_str().unwrap().to_string();
        download_file(
            &sh_s_argv_locally("cat", &[&src2_arg]),
            &dst2,
            MAX_TRANSFER_BYTES,
            TransferControl::default(),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&src_dir).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o555);
            std::fs::set_permissions(&src_dir, perms.clone()).unwrap();
            let delete_result = delete(&FsLocation::Local, &[], &src2);
            perms.set_mode(0o755);
            std::fs::set_permissions(&src_dir, perms).unwrap();
            assert!(delete_result.is_err(), "read-only dir must block delete");
            assert!(src2.exists(), "source survives a failed cut-delete");
            assert_eq!(std::fs::read(&dst2).unwrap(), binary_sample());
        }
    }

    #[test]
    fn transfer_validates_locations_before_contacting_any_host() {
        let dir = TestDir::new();
        let src = dir.join("file.txt");
        std::fs::write(&src, b"x").unwrap();

        // 本机→本机不是传输（是 copy/rename）。
        let error = transfer(
            &FsLocation::Local,
            &[],
            &src,
            false,
            &FsLocation::Local,
            dir.path(),
            TransferControl::default(),
        )
        .expect_err("same-location transfer");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        // 主机下标越界在 spawn 之前失败。
        let error = transfer(
            &FsLocation::Local,
            &[],
            &src,
            false,
            &FsLocation::Remote(9),
            dir.path(),
            TransferControl::default(),
        )
        .expect_err("unknown host");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        // 源没有文件名（"/"）。
        let error = transfer(
            &FsLocation::Local,
            &[],
            Path::new("/"),
            true,
            &FsLocation::Remote(0),
            dir.path(),
            TransferControl::default(),
        )
        .expect_err("root source");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    // ---- 进度 / 取消 ----

    #[test]
    fn format_bytes_human_readable() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(12 * 1024 + 410), "12.4 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(format_bytes(13_000_000), "12.4 MiB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024 + 1), "5.0 GiB");
    }

    #[test]
    fn progress_sink_throttles_by_bytes_and_time() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut sink = ProgressSink::new({
            let sent = sent.clone();
            move |bytes| sent.lock().push(bytes)
        });
        // 字节节流：一个 250ms 窗口内每块 1KiB，只有首块 + 每 256KiB 外发。
        for k in 1..=600u64 {
            sink.report(k * 1024);
        }
        assert_eq!(*sent.lock(), vec![1024, 263_168, 525_312]);

        // 时间节流：窗口之外的小增量也外发。
        std::thread::sleep(Duration::from_millis(260));
        sink.report(615_424);
        assert_eq!(sent.lock().len(), 4);

        // report_final 不走节流，终值必达。
        sink.report_final(616_448);
        assert_eq!(sent.lock().len(), 5);
        assert_eq!(sent.lock().last(), Some(&616_448));
    }

    #[test]
    fn cancel_kills_mid_download_and_cleans_the_partial_file() {
        let dir = TestDir::new();
        let dst = dir.join("dst.bin");
        let token = Arc::new(AtomicBool::new(false));
        // "远端"先吐 3 字节再睡死：取消必定打在传输中途。
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf abc; sleep 30".to_string(),
        ];
        let control = TransferControl {
            progress: None,
            cancel: Some(token.clone()),
        };
        let dst_in_thread = dst.clone();
        let handle = std::thread::spawn(move || {
            download_file(&argv, &dst_in_thread, MAX_TRANSFER_BYTES, control)
        });
        // 让 abc 落盘、子进程睡死，再取消。
        std::thread::sleep(Duration::from_millis(300));
        token.store(true, Ordering::SeqCst);
        let error = handle
            .join()
            .expect("download thread")
            .expect_err("cancelled download");
        // 取消是中性语义（Interrupted），不是错误文案。
        assert!(is_cancelled_error(&error), "{error}");
        // 目标没有出现，隐藏部分文件也被清掉。
        assert!(!dst.exists());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".fspart-"))
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
    }

    #[test]
    fn cancel_after_completion_is_a_no_op() {
        let dir = TestDir::new();
        let src = dir.join("src.bin");
        std::fs::write(&src, binary_sample()).unwrap();
        let src_arg = src.to_str().unwrap().to_string();
        let dst = dir.join("dst.bin");
        let token = Arc::new(AtomicBool::new(false));
        let control = TransferControl {
            progress: None,
            cancel: Some(token.clone()),
        };
        // 正常下载完成；此刻再置令牌，结果必须不受影响（已落位的文件不动）。
        download_file(
            &sh_s_argv_locally("cat", &[&src_arg]),
            &dst,
            MAX_TRANSFER_BYTES,
            control,
        )
        .unwrap();
        token.store(true, Ordering::SeqCst);
        assert_eq!(std::fs::read(&dst).unwrap(), binary_sample());
    }

    #[test]
    fn progress_reports_throttled_totals_during_a_real_download() {
        let dir = TestDir::new();
        let src = dir.join("src.bin");
        std::fs::write(&src, vec![3u8; 600 * 1024]).unwrap();
        let src_arg = src.to_str().unwrap().to_string();
        let dst = dir.join("dst.bin");
        let sent = Arc::new(Mutex::new(Vec::new()));
        let control = TransferControl {
            progress: Some(ProgressSink::new({
                let sent = sent.clone();
                move |bytes| sent.lock().push(bytes)
            })),
            cancel: None,
        };
        download_file(
            &sh_s_argv_locally("cat", &[&src_arg]),
            &dst,
            MAX_TRANSFER_BYTES,
            control,
        )
        .unwrap();
        let sent = sent.lock().clone();
        // 600KiB：节流后只有少数几次外发，且终值（report_final）必达。
        assert!(sent.len() <= 5, "sent: {sent:?}");
        assert_eq!(sent.last(), Some(&(600 * 1024)), "sent: {sent:?}");
        assert!(sent.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(std::fs::read(&dst).unwrap().len(), 600 * 1024);
    }
}
