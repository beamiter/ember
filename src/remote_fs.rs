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

use parking_lot::Mutex;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// 文件操作剪贴板（Copy / Cut → Paste）。只允许同一 [`FsLocation`] 内粘贴：
/// 跨位置粘贴没有定义好的语义，也不该一次点击就隐式地把文件拉过网络，
/// UI 层把它禁用并说明原因。
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
    /// 退出码；子进程被信号杀死（含超时 kill）时为 None。
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// watchdog 超时并 kill 了子进程。
    pub timed_out: bool,
}

/// 远端探针脚本协议 v1。经 stdin 传给远端的 `sh -s -- <op> [args...]`：
/// - `list` 的 stdout 是 NUL 分隔的 "<t>\0<name>\0" 对，t ∈ {d, f, l}，相对名。
/// - 退出码：0 正常，2 用法/路径非法，3 无法进入目录，4 操作失败，17 目标已存在。
pub const PROBE_SCRIPT: &str = r#"# remote-fs probe v1 — runs under `sh -s -- <op> [args...]`.
# `list` stdout: NUL-separated pairs "<t>\0<name>\0", t in {d,f,l}, names relative.
# Exit codes: 0 ok, 2 usage/bad path, 3 cannot enter dir, 4 op failed, 17 target exists.
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

/// `ssh -o BatchMode=yes -o ConnectTimeout=10 <ssh_args...> -- <dest> <cmd>`。
/// BatchMode 保证无密钥时快速失败而不是挂在密码提示上。
fn ssh_probe_argv(host: &RemoteHostConfig, op: &str, args: &[&str]) -> Vec<String> {
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
    argv.push(probe_command(op, args));
    argv
}

/// `docker exec -i [-u user] <container> sh -s -- <op> <args...>`：原始 argv，
/// 无需转义（docker 不做远端 shell 重解析），`-i` 提供 stdin。
fn docker_probe_argv(host: &RemoteHostConfig, op: &str, args: &[&str]) -> Vec<String> {
    let mut argv = vec!["docker".to_string(), "exec".to_string(), "-i".to_string()];
    if let Some(user) = &host.user {
        argv.push("-u".to_string());
        argv.push(user.clone());
    }
    argv.push(host.host.clone());
    argv.push("sh".to_string());
    argv.push("-s".to_string());
    argv.push("--".to_string());
    argv.push(op.to_string());
    argv.extend(args.iter().map(|arg| arg.to_string()));
    argv
}

/// 有界地运行一个子进程：pipe stdio，写入并关闭 stdin，stdout/stderr 各按
/// `max_out` 截断读取，watchdog 线程在 `timeout` 后 kill 子进程。
pub fn run_capture(
    argv: &[String],
    stdin_bytes: &[u8],
    timeout: Duration,
    max_out: u64,
) -> io::Result<Capture> {
    let Some((program, args)) = argv.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // 探针脚本约 1.5KB，远小于 64KB 管道缓冲，同步写不会阻塞；子进程若立即
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

    let child = Arc::new(Mutex::new(child));
    let timed_out = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let watchdog = {
        let child = child.clone();
        let timed_out = timed_out.clone();
        std::thread::Builder::new()
            .name("ember-fs-probe-watchdog".to_string())
            .spawn(move || {
                if done_rx.recv_timeout(timeout).is_err() {
                    // kill 成功才标记超时，避免与已自然退出的子进程竞争时误报。
                    if child.lock().kill().is_ok() {
                        timed_out.store(true, Ordering::SeqCst);
                    }
                }
            })?
    };

    // stderr 另开读者线程：子进程写满 stderr 管道而主线程在读 stdout 时，
    // 单线程顺序读会互相等待形成死锁。
    let stderr_reader = std::thread::Builder::new()
        .name("ember-fs-probe-stderr".to_string())
        .spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr_pipe.take(max_out).read_to_end(&mut buf);
            buf
        })?;

    let mut stdout = Vec::new();
    let read_result = stdout_pipe.take(max_out).read_to_end(&mut stdout);
    // 读端随 Take 一起 drop，子进程继续写会收到 SIGPIPE 自行退出。

    // try_wait 轮询而不是wait() 长持锁：watchdog 需要同一把锁来 kill。
    let status = loop {
        if let Some(status) = child.lock().try_wait()? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // 通知 watchdog 停止并等它退出，避免残留线程挂着子进程的锁。
    let _ = done_tx.send(());
    let _ = watchdog.join();
    let stderr = stderr_reader.join().unwrap_or_default();

    read_result?;
    Ok(Capture {
        status: status.code(),
        stdout,
        stderr,
        timed_out: timed_out.load(Ordering::SeqCst),
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
    let argv = if host.docker {
        docker_probe_argv(host, op, args)
    } else {
        ssh_probe_argv(host, op, args)
    };
    run_capture(&argv, PROBE_SCRIPT.as_bytes(), timeout, max_out)
}

/// 探针退出码 → io 错误。脚本协议：0 正常，2 用法/路径非法，3 无法进入
/// 目录，4 操作失败，17 目标已存在；其余（含 127 = 远端没有 sh）一律 Other。
fn probe_output(capture: Capture) -> io::Result<Vec<u8>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

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
        let argv = ssh_probe_argv(&ssh_host(), "list", &["/var/log"]);
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
        let argv = ssh_probe_argv(&host, "home", &[]);
        assert_eq!(argv[argv.len() - 2], "builder");
        assert_eq!(argv[argv.len() - 1], "sh -s -- home");
    }

    #[test]
    fn docker_argv_is_raw_and_never_allocates_a_tty() {
        let argv = docker_probe_argv(&docker_host(), "mv", &["/a b/c", "/d"]);
        assert_eq!(
            argv,
            vec![
                "docker", "exec", "-i", "-u", "devuser", "myubuntu", "sh", "-s", "--", "mv",
                "/a b/c", "/d",
            ]
        );
        assert!(!argv.iter().any(|arg| arg == "-t"), "{argv:?}");

        let host: RemoteHostConfig = toml::from_str("host = \"c1\"\ndocker = true").unwrap();
        let argv = docker_probe_argv(&host, "home", &[]);
        assert_eq!(
            argv,
            vec!["docker", "exec", "-i", "c1", "sh", "-s", "--", "home"]
        );
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
}
