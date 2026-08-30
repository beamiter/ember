//! 远程文件系统访问层：侧边栏文件树通过它浏览本机、SSH 主机和 Docker 容器。
//!
//! 不依赖 sshfs、不新增 crate：远程访问复用 jsh-remote 的思路 —— spawn 系统的
//! `ssh` / `docker` 二进制，把一段小型 POSIX sh 探针脚本（[`PROBE_SCRIPT`]）喂到
//! 远端的 stdin，操作数走位置参数（`sh -s -- <op> [args...]`）。所有公共函数都
//! 是阻塞的；调用方（侧边栏的扫描/操作 worker 线程）负责把它们移出 UI 线程。
//!
//! 安全约束：
//! - ssh 的远端命令是单个 argv 元素，每个参数经 `sq` 单引号转义，绝不未加
//!   引号拼接路径（ssh 会把命令交给远端登录 shell 重新解析）。
//! - `docker exec` 走原始 argv，无需转义；永远用 `-i`（stdin），不用 `-t`
//!   （探针不是交互会话，分配 TTY 只会污染输出）。
//! - 子进程 stdout/stderr 都有界读取，watchdog 线程在超时后强制 kill。
//! - put/untar 的 stdin 要整个留给上传载荷，脚本本体改走 `sh -c` 内联
//!   （`sh -s` 的预读缓冲会和探针里的 `cat`/`tar x` 抢 stdin 字节）。

use parking_lot::Mutex;
use std::collections::BTreeMap;
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
/// Remote names become command operands after parsing. Reject pathological
/// protocol rows before allocation/path construction; real filesystem
/// NAME_MAX values are far below this defensive ceiling.
const MAX_REMOTE_NAME_BYTES: usize = 4096;
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

/// 文件树当前浏览的位置：本机、`config.remote_hosts` 里的第 N 台主机，
/// 或从真实前台 `ssh` argv 临时派生出的独立 profile。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FsLocation {
    Local,
    Remote(usize),
    Transient(RemoteHostConfig),
}

/// Execution-only connection material for one Files endpoint, kept outside
/// its stable identity. This can come from an explicit `ssh -S`/
/// `ControlPath` option or from a trusted jsh launcher's reusable
/// ControlMaster socket; storing either in `RemoteHostConfig::ssh_args` would
/// corrupt config matching, clipboard identity, and transient deduplication.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshExecutionOverlay {
    pub control_path: Option<String>,
}

impl SshExecutionOverlay {
    pub fn from_control_path(path: Option<String>) -> Self {
        Self { control_path: path }
    }

    pub fn is_empty(&self) -> bool {
        self.control_path.is_none()
    }
}

/// An immutable Files execution endpoint captured when asynchronous work is
/// dispatched. `location` remains the stable namespace identity while
/// `overlay` carries only the live execution material for that snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsEndpointSnapshot {
    pub location: FsLocation,
    pub overlay: SshExecutionOverlay,
}

impl FsEndpointSnapshot {
    pub fn new(location: FsLocation, overlay: SshExecutionOverlay) -> Self {
        Self { location, overlay }
    }
}

impl FsLocation {
    pub fn is_remote(&self) -> bool {
        !matches!(self, Self::Local)
    }

    /// 位置选择器里显示的标签。
    pub fn label(&self, hosts: &[RemoteHostConfig]) -> String {
        match self {
            FsLocation::Local => "Local".to_string(),
            FsLocation::Remote(index) => match hosts.get(*index) {
                Some(host) => {
                    let unavailable =
                        crate::config::validate_remote_host_at(hosts, *index).is_err();
                    let mut label = format!(
                        "{}: {}",
                        if host.docker { "docker" } else { "ssh" },
                        crate::config::remote_host_location_display_name(host, *index)
                    );
                    if unavailable {
                        label.push_str(" (unavailable)");
                    }
                    label
                }
                None => format!("remote #{index}（已从配置移除）"),
            },
            FsLocation::Transient(host) => format!(
                "ssh: {} (temporary)",
                crate::config::remote_host_runtime_location_label(host)
            ),
        }
    }

    /// Full safe endpoint detail for a Files-location selector tooltip. The
    /// visible label is compact; this keeps the complete ordinary DSW host
    /// available without letting it determine sidebar width.
    pub fn detail(&self, hosts: &[RemoteHostConfig]) -> String {
        match self {
            FsLocation::Local => "Local filesystem".to_string(),
            FsLocation::Remote(index) => match hosts.get(*index) {
                Some(host) => {
                    let kind = if host.docker { "docker" } else { "ssh" };
                    let display = crate::config::remote_host_display_name(host, *index);
                    let endpoint = crate::config::remote_host_endpoint_detail(host);
                    if display == endpoint {
                        format!("{kind}: {endpoint}")
                    } else {
                        format!("{kind}: {display} — {endpoint}")
                    }
                }
                None => format!("remote #{} (removed from configuration)", index + 1),
            },
            FsLocation::Transient(host) => format!(
                "Temporary SSH profile: {}",
                crate::config::remote_host_endpoint_detail(host)
            ),
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
/// 多选批量：items 逐项粘贴（跳过失败、汇总上报）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsClipboard {
    pub loc: FsLocation,
    /// Frozen execution metadata for the source location. It is independent
    /// of the active tree's overlay so switching destinations cannot make a
    /// later paste lose the authenticated source socket.
    pub overlay: SshExecutionOverlay,
    pub items: Vec<FsClipboardItem>,
    pub cut: bool,
}

/// 剪贴板里的一个条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsClipboardItem {
    pub path: PathBuf,
    pub is_dir: bool,
}

impl FsClipboardItem {
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

/// 远端探针脚本协议 v6。默认经 stdin 传给远端的 `sh -s -- <op> [args...]`；
/// put/untar 例外，走 `sh -c` 内联脚本（stdin 整个留给上传载荷）：
/// - `list` 的 stdout 是 NUL 分隔的 `<t>\0<name>\0` 对，t ∈ {d, f, l}，相对名。
/// - v2 新增：`cat` 流式读文件、`put` 流式写新文件、
///   `tar` 目录打包流。
/// - 所有创建型操作把悬空符号链接也视为已存在（17）；`test -e` 会漏掉这种
///   目录项，而 `mkfile` 随后会跟随它写到预期目录之外。
/// - `stat` 与 `list` 一样先识别符号链接，并且只读取普通文件的大小；FIFO、
///   socket/device 等现存叶节点返回 `f 0`，目标预检不会为求大小而阻塞。
/// - v3：`untar` 改为 `untar <dir> <name>` —— 解包前先查 `<dir>/<name>` 是否
///   已存在（17），目录上传/中转因此 fail-closed（检查与解包之间仍有微秒级
///   TOCTOU 窗口，见代码注释；这是 tar 合并语义的协议极限）。新增 `stat`
///   打印 `<t> <size>`（普通文件为字节数，其余 0），取代 v2 的 list+cat 双探针预检。
/// - v5：`put` 在同父级私有目录内接收 stdin，再以 hard-link no-replace 原子发布；
///   预植临时路径不会被跟随，最后检查后出现的目标也不会被 `mv` 覆盖。
/// - v6：`put <path> <transfer-id>` 由客户端唯一令牌命名有界的候选目录，
///   取消/超时后只清理本次上传的 32 个可能候选。
/// - 退出码：0 正常，2 用法/路径非法，3 缺失，4 操作失败，13 权限，
///   17 目标已存在，20 非目录。
pub const PROBE_SCRIPT: &str = r#"# remote-fs probe v6 — runs under `sh -s -- <op> [args...]`.
# `list` stdout: NUL-separated pairs "<t>\0<name>\0", t in {d,f,l}, names relative.
# Exit codes: 0 ok, 2 usage/bad path, 3 missing, 4 op failed, 13 permission,
# 17 target exists, 20 not a directory.
# v2 adds: cat (stream file to stdout), put (stream stdin to a new file),
# tar (stream dir as tar to stdout), untar (extract stdin tar into a dir).
# v3: untar takes <dir> <name> and refuses an existing <dir>/<name> (17) before
# extracting; new stat op prints "<t> <size>" (t in {d,f,l}; regular-file bytes, else 0).
# v4: list accepts [max_rows] [show_hidden], stops remotely at the requested
# retained-row ceiling, and classifies symlinks before directories.
# v5: put receives data in a private same-parent directory and publishes with
# an atomic no-replace hard link.
# v6: put takes a client transfer id so cancel cleanup can enumerate only that
# upload's bounded collision candidates.
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
    limit=${3:-0}
    show_hidden=${4:-1}
    case "$limit" in *[!0-9]*|'') exit 2 ;; esac
    case "$show_hidden" in 0|1) ;; *) exit 2 ;; esac
    cd "$d" 2>/dev/null || {
      [ -e "$d" ] || exit 3
      [ -d "$d" ] || exit 20
      exit 13
    }
    count=0
    for f in * .[!.]* ..?*; do
      case "$show_hidden:$f" in 0:.*) continue ;; esac
      if [ -L "$f" ]; then t=l
      elif [ -d "$f" ]; then t=d
      elif [ -e "$f" ]; then t=f
      else continue
      fi
      printf '%s\0%s\0' "$t" "$f"
      count=$((count + 1))
      if [ "$limit" -gt 0 ] && [ "$count" -ge "$limit" ]; then break; fi
    done
    ;;
  mkdir)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    if [ -e "$p" ] || [ -L "$p" ]; then exit 17; fi
    mkdir "$p" || exit 4
    ;;
  mkfile)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    if [ -e "$p" ] || [ -L "$p" ]; then exit 17; fi
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
    if [ -e "$n" ] || [ -L "$n" ]; then exit 17; fi
    mv "$s" "$n" || exit 4
    ;;
  cp)
    s=${2:-}; n=${3:-}
    case "$s" in /*) ;; *) exit 2 ;; esac
    case "$n" in /*) ;; *) exit 2 ;; esac
    if [ -e "$n" ] || [ -L "$n" ]; then exit 17; fi
    cp -a "$s" "$n" || exit 4
    ;;
  cat)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -f "$p" ] && [ -r "$p" ] || exit 3
    cat "$p" || exit 4
    ;;
  put)
    p=${2:-}; id=${3:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    case "$id" in ''|*[!0-9a-f-]*) exit 2 ;; esac
    [ "${#id}" -le 96 ] || exit 2
    if [ -e "$p" ] || [ -L "$p" ]; then exit 17; fi
    d=${p%/*}
    d=${d:-/}
    stage=
    i=0
    umask 077
    while [ "$i" -lt 32 ]; do
      case "$d" in
        /) candidate="/.ember-fs-put-$id-$i" ;;
        *) candidate="$d/.ember-fs-put-$id-$i" ;;
      esac
      i=$((i + 1))
      [ "$candidate" = "$p" ] && continue
      if mkdir "$candidate" 2>/dev/null; then stage=$candidate; break; fi
    done
    [ -n "$stage" ] || exit 4
    payload="$stage/payload"
    code=4
    if cat > "$payload"; then
      if ln -T "$payload" "$p" 2>/dev/null; then
        code=0
      elif [ -e "$p" ] || [ -L "$p" ]; then
        code=17
      fi
    fi
    rm -f "$payload"
    rmdir "$stage" 2>/dev/null || :
    exit "$code"
    ;;
  tar)
    p=${2:-}
    p=${p%/}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -d "$p" ] || exit 3
    command -v tar >/dev/null 2>&1 || { echo "remote-fs probe: tar is not available" >&2; exit 4; }
    d=${p%/*}
    d=${d:-/}
    tar cf - -C "$d" "${p##*/}" || exit 4
    ;;
  untar)
    d=${2:-}
    n=${3:-}
    case "$d" in /*) ;; *) exit 2 ;; esac
    case "$n" in ""|*/*) exit 2 ;; esac
    [ -d "$d" ] || exit 3
    if [ -e "$d/$n" ] || [ -L "$d/$n" ]; then exit 17; fi
    command -v tar >/dev/null 2>&1 || { echo "remote-fs probe: tar is not available" >&2; exit 4; }
    tar xf - -C "$d" || exit 4
    ;;
  stat)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    if [ -L "$p" ]; then t=l
    elif [ -d "$p" ]; then t=d
    elif [ -f "$p" ]; then t=f
    elif [ -e "$p" ]; then t=f
    else exit 3
    fi
    s=0
    if [ -f "$p" ] && [ ! -L "$p" ]; then s=$(wc -c < "$p") || exit 4; fi
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

/// 进程内唯一、shell-safe 的上传令牌，同时绑定远端候选目录与
/// 取消清理。远端探针会再独立校验字符集和长度。
fn put_transfer_id() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let epoch_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{:x}-{epoch_nanos:x}-{:x}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// 只枚举一个 transfer id 的 0..31 候选；不用 glob，不跟随链接，
/// 并显式跳过最终目标（即使它恰好长得像内部名）。
fn put_cleanup_command(dst: &str, transfer_id: &str) -> String {
    let parent = Path::new(dst)
        .parent()
        .and_then(Path::to_str)
        .filter(|parent| !parent.is_empty())
        .unwrap_or(".");
    let prefix = if parent == "/" {
        format!("/.ember-fs-put-{transfer_id}-")
    } else {
        format!("{parent}/.ember-fs-put-{transfer_id}-")
    };
    format!(
        "i=0; while [ \"$i\" -lt 32 ]; do d={}$i; i=$((i + 1)); [ \"$d\" = {} ] && continue; [ -d \"$d\" ] && [ ! -L \"$d\" ] || continue; rm -f \"$d/payload\"; rmdir \"$d\" 2>/dev/null || :; done",
        sq(&prefix),
        sq(dst)
    )
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

/// 一条 Ember 自身构造的远端 shell 命令（当前只用于最佳努力的
/// put 取消清理）。ssh 的命令仍是单个 argv；docker 显式走 `sh -c`。
fn remote_shell_command_argv(host: &RemoteHostConfig, command: &str) -> Vec<String> {
    if host.docker {
        let mut argv = docker_base_argv(host);
        argv.push("sh".to_string());
        argv.push("-c".to_string());
        argv.push(command.to_string());
        argv
    } else {
        let mut argv = ssh_base_argv(host);
        argv.push(command.to_string());
        argv
    }
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

fn checked_probe_argv(host: &RemoteHostConfig, op: &str, args: &[&str]) -> io::Result<Vec<String>> {
    validate_host_for_execution(host)?;
    Ok(probe_argv(host, op, args))
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

fn checked_sh_c_probe_argv(
    host: &RemoteHostConfig,
    op: &str,
    args: &[&str],
) -> io::Result<Vec<String>> {
    validate_host_for_execution(host)?;
    Ok(sh_c_probe_argv(host, op, args))
}

fn spawn_piped(argv: &[String]) -> io::Result<Child> {
    let Some((program, args)) = argv.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 独立进程组（子进程任组长）：kill 时整组回收，探针 fork 出去的
        // 子孙（rm -rf 半途、sh -c 下的 sleep）无法存活占管。
        command.process_group(0);
    }
    command.spawn()
}

/// 杀掉整个进程组（子进程在 spawn 时被设为组长）；非 Unix 退化为只杀
/// 直接子进程。只发信号、返回是否发出成功：直接子进程的回收由调用方的
/// try_wait/wait 路径完成。
fn kill_tree(child: &mut Child) -> bool {
    #[cfg(unix)]
    unsafe {
        // SAFETY: 对 spawn 时建立的进程组发一次 kill；pid 来自存活的
        // Child 句柄。
        libc::kill(-(child.id() as i32), libc::SIGKILL) == 0
    }
    #[cfg(not(unix))]
    child.kill().is_ok()
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
                                    if kill_tree(&mut child.lock()) {
                                        cancelled.store(true, Ordering::SeqCst);
                                    }
                                    break;
                                }
                                if std::time::Instant::now() >= deadline {
                                    if kill_tree(&mut child.lock()) {
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

    /// 传输超限/流错误时立即中止子进程（整组 kill，子孙一并回收）。
    fn kill(&self) {
        let _ = kill_tree(&mut self.child.lock());
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

/// 把子进程输出流读到 EOF，硬上限 max 字节：超出部分继续排空（子进程
/// 不会堵在满管道上），调用方把 truncated 视为错误。
fn read_bounded<R: Read>(reader: R, max: u64) -> io::Result<(Vec<u8>, bool)> {
    let mut limited = reader.take(max + 1);
    let mut bytes = Vec::new();
    limited.read_to_end(&mut bytes)?;
    if bytes.len() <= max as usize {
        return Ok((bytes, false));
    }
    bytes.truncate(max as usize);
    let mut rest = limited.into_inner();
    io::copy(&mut rest, &mut io::sink())?;
    Ok((bytes, true))
}

/// stderr 另开读者线程：子进程写满 stderr 管道而主线程在读 stdout 时，
/// 单线程顺序读会互相等待形成死锁。
fn spawn_stderr_reader(
    stderr: std::process::ChildStderr,
    max_out: u64,
) -> io::Result<std::thread::JoinHandle<(Vec<u8>, bool)>> {
    std::thread::Builder::new()
        .name("ember-fs-probe-stderr".to_string())
        .spawn(move || read_bounded(stderr, max_out).unwrap_or_default())
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

    // 超限字节继续排空（子进程不会堵在满管道上），截断在下方统一报错。
    let stdout_read = read_bounded(stdout_pipe, max_out);

    let (code, timed_out, cancelled) = monitored.wait()?;
    let (stderr, stderr_truncated) = stderr_reader.join().unwrap_or_default();

    let (stdout, stdout_truncated) = stdout_read?;
    // 被 kill（超时/取消）的进程输出本就不完整，截断错误只报给正常跑完
    // 却超额输出的进程。
    if (stdout_truncated || stderr_truncated) && !timed_out && !cancelled {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("command produced more than {max_out} bytes of output"),
        ));
    }
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
#[cfg(test)]
fn run_stream_to_file(
    argv: &[String],
    stdin_bytes: &[u8],
    dest: &Path,
    timeout: Duration,
    max_bytes: u64,
    control: TransferControl,
) -> io::Result<Capture> {
    // Reserve the staging name before starting a producer. O_EXCL refuses an
    // existing symlink instead of following it, and an open failure cannot
    // leave an unobserved child behind.
    let file = open_transfer_staging(dest)?;
    run_stream_to_open_file(argv, stdin_bytes, file, timeout, max_bytes, control)
}

fn open_transfer_staging(path: &Path) -> io::Result<std::fs::File> {
    let mut staging_options = std::fs::OpenOptions::new();
    staging_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        staging_options.mode(0o600);
    }
    staging_options.open(path)
}

/// Stream into an already exclusively reserved staging inode. Keeping name
/// selection separate lets production retry occupied short names without
/// ever starting the producer that would feed them.
fn run_stream_to_open_file(
    argv: &[String],
    stdin_bytes: &[u8],
    mut file: std::fs::File,
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
    // 传输路径的 stderr 仅作诊断：截断标志在这里无关紧要，保留封顶字节即可。
    let (stderr, _) = stderr_reader.join().unwrap_or_default();
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
    // 传输路径的 stderr 仅作诊断：截断标志在这里无关紧要，保留封顶字节即可。
    let (stderr, _) = stderr_reader.join().unwrap_or_default();
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
    run_probe_with_cancel(host, op, args, timeout, max_out, None)
}

fn run_probe_with_cancel(
    host: &RemoteHostConfig,
    op: &str,
    args: &[&str],
    timeout: Duration,
    max_out: u64,
    cancel: Option<Arc<AtomicBool>>,
) -> io::Result<Capture> {
    run_capture_with_cancel(
        &checked_probe_argv(host, op, args)?,
        PROBE_SCRIPT.as_bytes(),
        timeout,
        max_out,
        cancel,
    )
}

/// 探针退出码 → io 错误。脚本协议：0 正常，2 用法/路径非法，3 缺失，
/// 4 操作失败，13 权限，17 目标已存在，20 非目录；其余（含 127 =
/// 远端没有 sh）一律 Other。
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
            "directory no longer exists",
        )),
        Some(13) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "permission denied",
        )),
        Some(20) => Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "path is not a directory",
        )),
        Some(2) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            probe_stderr(&capture),
        )),
        _ => Err(io::Error::other(probe_stderr(&capture))),
    }
}

const MAX_INLINE_DIAGNOSTIC_CHARS: usize = 200;

fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn sensitive_key(key: &str) -> bool {
    matches!(
        key.trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
            .to_ascii_lowercase()
            .as_str(),
        "password"
            | "passwd"
            | "token"
            | "secret"
            | "authorization"
            | "proxyauthorization"
            | "proxy-authorization"
            | "apikey"
            | "api_key"
    )
}

/// Convert an untrusted subprocess/OS diagnostic into one bounded UI line.
/// Control and bidi-format characters cannot spoof adjacent chrome, common
/// credential assignments are redacted, and the limit is counted in Unicode
/// scalar values (never a byte index inside a multibyte character).
pub(crate) fn safe_inline_diagnostic(text: &str) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("remote operation failed");
    let cleaned: String = first
        .chars()
        .map(|ch| {
            if ch.is_control() || is_bidi_control(ch) {
                ' '
            } else {
                ch
            }
        })
        .collect();

    let mut redacted = Vec::new();
    let mut redact_next = false;
    for word in cleaned.split_whitespace() {
        if redact_next {
            redacted.push("<redacted>".to_string());
            redact_next = false;
            continue;
        }
        if let Some((key, _value)) = word.split_once('=') {
            if sensitive_key(key) {
                redacted.push(format!("{key}=<redacted>"));
                continue;
            }
        }
        if let Some(key) = word.strip_suffix(':') {
            if sensitive_key(key) {
                redacted.push(format!("{key}:"));
                redact_next = true;
                continue;
            }
        }
        redacted.push(word.to_string());
    }
    let redacted = redacted.join(" ");
    let mut chars = redacted.chars();
    let mut bounded: String = chars.by_ref().take(MAX_INLINE_DIAGNOSTIC_CHARS).collect();
    if chars.next().is_some() {
        bounded.push('…');
    }
    bounded
}

/// Stable, retry-oriented error text for Files UI. Raw remote stderr is never
/// required to decide the user action and is only admitted through the safe
/// inline boundary above.
pub(crate) fn user_facing_error(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::AlreadyExists => "target already exists".to_string(),
        io::ErrorKind::NotFound => "path not found or no longer available".to_string(),
        io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        io::ErrorKind::NotADirectory => "path is not a directory".to_string(),
        io::ErrorKind::TimedOut => "remote request timed out; retry".to_string(),
        io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::NotConnected
        | io::ErrorKind::BrokenPipe => "remote connection unavailable; retry".to_string(),
        io::ErrorKind::InvalidData => "remote returned invalid directory data".to_string(),
        io::ErrorKind::Interrupted => "operation cancelled".to_string(),
        _ => safe_inline_diagnostic(&error.to_string()),
    }
}

pub(crate) fn is_retryable_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::TimedOut
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::Other
    )
}

/// 从有界的 stderr 里取一行短消息用于 UI 展示。
fn probe_stderr(capture: &Capture) -> String {
    let text = String::from_utf8_lossy(&capture.stderr);
    let text = text.trim();
    if text.is_empty() {
        return format!("remote probe failed (exit {:?})", capture.status);
    }
    safe_inline_diagnostic(text)
}

/// 解析 `list` 的 stdout：NUL 分隔的 (type, name) 对。d → 目录，f/l → 文件
/// （符号链接不展开成目录）。与本地 scan_dir 同一策略：隐藏 dotfiles、目录
/// 在前且大小写不敏感排序。远端输出不可信：非 UTF-8/超长名称、空名、
/// `.`/`..`、带 `/` 或重复碰撞的名称一律跳过。至多保留
/// MAX_DIRECTORY_ENTRIES + 1 条 —— 多出的第 MAX+1 条让上层（扫描 worker）
/// 据此标记"目录已截断"。
#[allow(dead_code)] // compatibility wrapper and default-policy test surface
fn parse_list(bytes: &[u8], dir: &Path) -> Vec<Entry> {
    parse_list_with_hidden(bytes, dir, false)
}

fn parse_list_with_hidden(bytes: &[u8], dir: &Path, show_hidden: bool) -> Vec<Entry> {
    // A lossy-decoded remote name is not the same command operand. Invalid
    // UTF-8 is therefore skipped instead of displayed as U+FFFD and later sent
    // back to a potentially different path. Duplicate names are ambiguous
    // protocol output; drop every occurrence rather than choosing a type.
    let mut entries_by_name: BTreeMap<String, Option<Entry>> = BTreeMap::new();
    let mut tokens = bytes.split(|byte| *byte == 0);
    let mut scanned = 0usize;
    while let (Some(kind), Some(name)) = (tokens.next(), tokens.next()) {
        scanned += 1;
        if scanned > MAX_SCANNED_PAIRS {
            break;
        }
        if name.is_empty()
            || name.len() > MAX_REMOTE_NAME_BYTES
            || matches!(name, [b'.'] | [b'.', b'.'])
            || name.contains(&b'/')
        {
            continue;
        }
        let Ok(name) = std::str::from_utf8(name) else {
            continue;
        };
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let is_dir = match kind.first() {
            Some(b'd') => true,
            Some(b'f') | Some(b'l') => false,
            _ => continue,
        };
        use std::collections::btree_map::Entry as MapEntry;
        match entries_by_name.entry(name.to_string()) {
            MapEntry::Vacant(slot) => {
                slot.insert(Some(Entry {
                    path: dir.join(name),
                    name: name.to_string(),
                    is_dir,
                }));
            }
            MapEntry::Occupied(mut slot) => {
                slot.insert(None);
            }
        }
    }
    let mut entries: Vec<Entry> = entries_by_name.into_values().flatten().collect();
    sort_entries(&mut entries);
    entries.truncate(MAX_DIRECTORY_ENTRIES + 1);
    entries
}

/// 与 sidebar::scan_dir 相同的排序：目录在前，名称大小写不敏感。
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
}

/// 本机目录列举，与远端 [`parse_list`] 完全同策略（dotfiles / 排序 / 上限），
/// 这样本机与远程在文件树里的行为没有可见差异。
#[allow(dead_code)] // compatibility wrapper and default-policy test surface
fn local_list_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    local_list_dir_with_hidden(dir, false)
}

fn local_list_dir_with_hidden(dir: &Path, show_hidden: bool) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for (scanned, entry) in std::fs::read_dir(dir)?.enumerate() {
        if scanned >= MAX_SCANNED_PAIRS || entries.len() > MAX_DIRECTORY_ENTRIES {
            break;
        }
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
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
    crate::config::validate_remote_host_at(hosts, index)
        .map_err(|problem| io::Error::new(io::ErrorKind::InvalidInput, problem))
}

/// Resolve a remote location against the exact authority captured with the
/// request. A transient profile is self-contained and deliberately ignores
/// later configuration edits/reorders.
fn host_for_location<'a>(
    loc: &'a FsLocation,
    hosts: &'a [RemoteHostConfig],
) -> io::Result<Option<&'a RemoteHostConfig>> {
    match loc {
        FsLocation::Local => Ok(None),
        FsLocation::Remote(index) => host_at(hosts, *index).map(Some),
        FsLocation::Transient(host) => {
            validate_host_for_execution(host)?;
            Ok(Some(host))
        }
    }
}

pub(crate) fn split_ssh_control_path_args(args: &[String]) -> (Vec<String>, Option<String>) {
    let mut base = Vec::with_capacity(args.len());
    let mut control_path = None;
    let mut index = 0usize;
    while index < args.len() {
        let argument = &args[index];
        if argument == "-S" {
            if let Some(path) = args.get(index + 1) {
                control_path.get_or_insert_with(|| path.clone());
                index += 2;
                continue;
            }
        } else if let Some(path) = argument.strip_prefix("-S").filter(|path| !path.is_empty()) {
            control_path.get_or_insert_with(|| path.to_string());
            index += 1;
            continue;
        }
        let control_path_option = |option: &str| -> Option<String> {
            let (key, value) = option.split_once('=')?;
            key.eq_ignore_ascii_case("controlpath")
                .then(|| value.to_string())
        };
        if argument == "-o" {
            if let Some(option) = args.get(index + 1) {
                if let Some(path) = control_path_option(option) {
                    control_path.get_or_insert(path);
                    index += 2;
                    continue;
                }
                base.push(argument.clone());
                base.push(option.clone());
                index += 2;
                continue;
            }
        } else if let Some(option) = argument.strip_prefix("-o") {
            if let Some(path) = control_path_option(option) {
                control_path.get_or_insert(path);
                index += 1;
                continue;
            }
        }
        base.push(argument.clone());
        index += 1;
    }
    (base, control_path)
}

/// Whether two stable locations address the same filesystem namespace. SSH
/// display/deployment fields and ControlPath execution material do not change
/// that namespace; endpoint/authentication options do. Invalid locations fail
/// closed rather than being treated as equal.
pub fn same_files_namespace(
    left: &FsLocation,
    right: &FsLocation,
    hosts: &[RemoteHostConfig],
) -> bool {
    let Ok(left_host) = host_for_location(left, hosts) else {
        return false;
    };
    let Ok(right_host) = host_for_location(right, hosts) else {
        return false;
    };
    match (left_host, right_host) {
        (None, None) => true,
        (Some(left), Some(right)) if left.docker || right.docker => {
            left.docker && right.docker && left.host == right.host && left.user == right.user
        }
        (Some(left), Some(right)) => {
            left.host == right.host
                && left.user == right.user
                && split_ssh_control_path_args(&left.ssh_args).0
                    == split_ssh_control_path_args(&right.ssh_args).0
        }
        _ => false,
    }
}

/// Choose execution authority for a direct same-namespace copy/rename. A
/// current destination socket wins when present; otherwise preserve the
/// source clipboard's live/temporary socket instead of falling back to a
/// saved profile's possibly stale ControlPath.
pub fn same_namespace_execution_overlay<'a>(
    source: &'a SshExecutionOverlay,
    destination: &'a SshExecutionOverlay,
) -> &'a SshExecutionOverlay {
    if destination.is_empty() {
        source
    } else {
        destination
    }
}

fn validate_control_path(path: &str) -> io::Result<()> {
    let replayable_without_original_cwd = Path::new(path).is_absolute()
        || path
            .strip_prefix("~/")
            .is_some_and(|home_relative| !home_relative.is_empty());
    if path.is_empty()
        || path.len() > 512
        || path.chars().any(char::is_control)
        || jterm_core::review_input::contains_visual_spoofing(path)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH ControlPath is unsafe",
        ));
    }
    if !replayable_without_original_cwd {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH ControlPath must be absolute or use ~/…; a relative socket depends on the original shell directory, so use an absolute ControlPath or a saved remote profile",
        ));
    }
    Ok(())
}

/// Resolve and clone an endpoint's immutable base profile, then apply the
/// narrowly typed execution overlay. The augmented clone is revalidated by
/// Ember immediately before it can become process argv; the base location and
/// configuration remain unchanged.
fn execution_host_for_location(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
) -> io::Result<Option<RemoteHostConfig>> {
    let Some(mut host) = host_for_location(loc, hosts)?.cloned() else {
        if overlay.is_empty() {
            return Ok(None);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an SSH execution overlay cannot target Local Files",
        ));
    };
    if let Some(path) = overlay.control_path.as_deref() {
        if host.docker {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "an SSH execution overlay cannot target Docker Files",
            ));
        }
        validate_control_path(path)?;
        host.ssh_args = split_ssh_control_path_args(&host.ssh_args).0;
        host.ssh_args.push("-S".to_string());
        host.ssh_args.push(path.to_string());
    }
    validate_host_for_execution(&host)?;
    Ok(Some(host))
}

/// Public preflight used by the UI's atomic SSH-follow commit. It performs
/// the same endpoint resolution and overlay validation as every worker path
/// without contacting a host.
pub fn validate_execution_endpoint(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
) -> io::Result<()> {
    execution_host_for_location(loc, hosts, overlay).map(|_| ())
}

/// Defense-in-depth for private helpers that receive a host reference instead
/// of its config index. Public remote-fs entry points first use [`host_at`] so
/// the 128-entry boundary is enforced; this second check guarantees a future
/// internal caller still cannot turn an invalid draft into process argv.
fn validate_host_for_execution(host: &RemoteHostConfig) -> io::Result<()> {
    crate::config::validate_remote_host(host).map_err(|problem| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "remote host {}: {problem}",
                crate::config::remote_host_runtime_label(host)
            ),
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

#[cfg(target_os = "linux")]
fn atomic_rename_noreplace(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let src = std::ffi::CString::new(src.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let dst = std::ffi::CString::new(dst.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL")
    })?;
    // SAFETY: both C strings remain live for this single namespace syscall;
    // RENAME_NOREPLACE makes a concurrently-created destination an error.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            src.as_ptr(),
            libc::AT_FDCWD,
            dst.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP)
    ) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("atomic no-replace rename is unavailable: {error}"),
        ));
    }
    Err(error)
}

#[cfg(not(target_os = "linux"))]
fn atomic_rename_noreplace(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace rename requires Linux renameat2",
    ))
}

fn rename_noreplace_with(
    src: &Path,
    dst: &Path,
    commit: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    // Keep the early, path-specific AlreadyExists diagnostic; the commit is
    // still authoritative if another process creates dst after this check.
    ensure_absent(dst)?;
    commit(src, dst)
}

fn rename_noreplace(src: &Path, dst: &Path) -> io::Result<()> {
    rename_noreplace_with(src, dst, atomic_rename_noreplace)
}

fn probe_op(host: &RemoteHostConfig, op: &str, args: &[&str]) -> io::Result<()> {
    let capture = run_probe(host, op, args, PROBE_OP_TIMEOUT, MAX_SMALL_OUTPUT)?;
    probe_output(capture).map(|_| ())
}

/// 列举目录。返回的条目数最多 MAX_DIRECTORY_ENTRIES + 1（截断信号，见上）。
#[allow(dead_code)] // compatibility wrapper used by the library test surface
pub fn list_dir(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    dir: &Path,
) -> io::Result<Vec<Entry>> {
    list_dir_with_overlay(loc, hosts, &SshExecutionOverlay::default(), dir)
}

pub fn list_dir_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
    dir: &Path,
) -> io::Result<Vec<Entry>> {
    list_dir_with_overlay_and_hidden(loc, hosts, overlay, dir, false)
}

pub fn list_dir_with_overlay_and_hidden(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
    dir: &Path,
    show_hidden: bool,
) -> io::Result<Vec<Entry>> {
    list_dir_with_overlay_and_hidden_impl(loc, hosts, overlay, dir, show_hidden, None)
}

/// Cancellable listing used by the Files scan coordinator. Cancellation kills
/// the whole remote probe process group and is reported as Interrupted; the
/// caller's generation/revision gate then discards that retired result.
pub fn list_dir_with_overlay_and_hidden_control(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
    dir: &Path,
    show_hidden: bool,
    cancel: Arc<AtomicBool>,
) -> io::Result<Vec<Entry>> {
    list_dir_with_overlay_and_hidden_impl(loc, hosts, overlay, dir, show_hidden, Some(cancel))
}

fn list_dir_with_overlay_and_hidden_impl(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
    dir: &Path,
    show_hidden: bool,
    cancel: Option<Arc<AtomicBool>>,
) -> io::Result<Vec<Entry>> {
    let Some(host) = execution_host_for_location(loc, hosts, overlay)? else {
        return local_list_dir_with_hidden(dir, show_hidden);
    };
    require_absolute(dir)?;
    let probe_args = list_probe_args(dir, show_hidden)?;
    let capture = run_probe_with_cancel(
        &host,
        "list",
        &[
            probe_args[0].as_str(),
            probe_args[1].as_str(),
            probe_args[2].as_str(),
        ],
        PROBE_LIST_TIMEOUT,
        MAX_LIST_OUTPUT,
        cancel,
    )?;
    let output = probe_output(capture)?;
    Ok(parse_list_with_hidden(&output, dir, show_hidden))
}

fn list_probe_args(dir: &Path, show_hidden: bool) -> io::Result<[String; 3]> {
    require_absolute(dir)?;
    Ok([
        path_str(dir)?.to_string(),
        (MAX_DIRECTORY_ENTRIES + 1).to_string(),
        if show_hidden { "1" } else { "0" }.to_string(),
    ])
}

fn parse_home_output(output: &[u8]) -> io::Result<PathBuf> {
    let text = std::str::from_utf8(output).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "remote home is not valid UTF-8")
    })?;
    let text = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .or_else(|| text.strip_suffix('\r'))
        .unwrap_or(text);
    if text.is_empty() || text.contains(['\n', '\r', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote home has an invalid line shape",
        ));
    }
    let path = PathBuf::from(text);
    require_absolute(&path)?;
    Ok(path)
}

fn remote_start_dir(host: &RemoteHostConfig) -> io::Result<PathBuf> {
    let capture = run_probe(host, "home", &[], PROBE_LIST_TIMEOUT, MAX_SMALL_OUTPUT)?;
    let output = probe_output(capture)?;
    parse_home_output(&output)
}

fn remote_create_dir(host: &RemoteHostConfig, path: &Path) -> io::Result<()> {
    require_absolute(path)?;
    probe_op(host, "mkdir", &[path_str(path)?])
}

fn remote_create_file(host: &RemoteHostConfig, path: &Path) -> io::Result<()> {
    require_absolute(path)?;
    probe_op(host, "mkfile", &[path_str(path)?])
}

fn remote_delete(host: &RemoteHostConfig, path: &Path) -> io::Result<()> {
    require_absolute(path)?;
    probe_op(host, "rm", &[path_str(path)?])
}

fn remote_rename(host: &RemoteHostConfig, src: &Path, dst: &Path) -> io::Result<()> {
    require_absolute(src)?;
    require_absolute(dst)?;
    probe_op(host, "mv", &[path_str(src)?, path_str(dst)?])
}

fn remote_copy(host: &RemoteHostConfig, src: &Path, dst: &Path) -> io::Result<()> {
    require_absolute(src)?;
    require_absolute(dst)?;
    probe_op(host, "cp", &[path_str(src)?, path_str(dst)?])
}

/// 进入某个位置时的起始目录：本机沿用今天的行为（进程 cwd，失败回 `/`），
/// 远程取远端 `$HOME`（探针 `home`）。
pub fn start_dir(loc: &FsLocation, hosts: &[RemoteHostConfig]) -> io::Result<PathBuf> {
    start_dir_with_overlay(loc, hosts, &SshExecutionOverlay::default())
}

pub fn start_dir_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
) -> io::Result<PathBuf> {
    match execution_host_for_location(loc, hosts, overlay)? {
        None => Ok(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))),
        Some(host) => remote_start_dir(&host),
    }
}

/// 新建目录；已存在 → AlreadyExists。
#[allow(dead_code)] // compatibility wrapper used by the library test surface
pub fn create_dir(loc: &FsLocation, hosts: &[RemoteHostConfig], path: &Path) -> io::Result<()> {
    create_dir_with_overlay(loc, hosts, &SshExecutionOverlay::default(), path)
}

pub fn create_dir_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
    path: &Path,
) -> io::Result<()> {
    match execution_host_for_location(loc, hosts, overlay)? {
        None => std::fs::create_dir(path),
        Some(host) => remote_create_dir(&host, path),
    }
}

/// 新建空文件；已存在 → AlreadyExists。
#[allow(dead_code)] // compatibility wrapper used by the library test surface
pub fn create_file(loc: &FsLocation, hosts: &[RemoteHostConfig], path: &Path) -> io::Result<()> {
    create_file_with_overlay(loc, hosts, &SshExecutionOverlay::default(), path)
}

pub fn create_file_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
    path: &Path,
) -> io::Result<()> {
    match execution_host_for_location(loc, hosts, overlay)? {
        None => std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(|_| ()),
        Some(host) => remote_create_file(&host, path),
    }
}

/// 删除文件或目录（目录递归删除；符号链接按链接本身删）。拒绝删除 `/`。
#[allow(dead_code)] // compatibility wrapper used by the library test surface
pub fn delete(loc: &FsLocation, hosts: &[RemoteHostConfig], path: &Path) -> io::Result<()> {
    delete_with_overlay(loc, hosts, &SshExecutionOverlay::default(), path)
}

pub fn delete_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
    path: &Path,
) -> io::Result<()> {
    if path == Path::new("/") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to delete /",
        ));
    }
    match execution_host_for_location(loc, hosts, overlay)? {
        None => {
            // 与探针一致：目录（非符号链接）递归删除，其余按文件删。
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            }
        }
        Some(host) => remote_delete(&host, path),
    }
}

/// 重命名/移动；目标已存在 → AlreadyExists。
#[allow(dead_code)] // compatibility wrapper used by the library test surface
pub fn rename(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    rename_with_overlay(loc, hosts, &SshExecutionOverlay::default(), src, dst)
}

pub fn rename_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    match execution_host_for_location(loc, hosts, overlay)? {
        None => rename_noreplace(src, dst),
        Some(host) => remote_rename(&host, src, dst),
    }
}

/// 复制文件或目录（目录递归复制；符号链接按链接复制）；目标已存在 → AlreadyExists。
#[allow(dead_code)] // compatibility wrapper used by the library test surface
pub fn copy(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    copy_with_overlay(loc, hosts, &SshExecutionOverlay::default(), src, dst)
}

pub fn copy_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    overlay: &SshExecutionOverlay,
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    match execution_host_for_location(loc, hosts, overlay)? {
        None => {
            ensure_absent(dst)?;
            copy_recursive(src, dst, 0)
        }
        Some(host) => remote_copy(&host, src, dst),
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
        if depth == 0 {
            // 拒绝把目录复制进自己（cp /a /a/b）：src 与 dst 父目录都做
            // 规范化，符号链接别名也绕不过去；没有这层检查时递归复制会
            // 一路套娃直到撞上 MAX_COPY_DEPTH。
            let canonical = src.canonicalize()?;
            let dst_parent = dst
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("/"))
                .canonicalize()
                .unwrap_or_else(|_| dst.parent().map(Path::to_path_buf).unwrap_or_default());
            if dst_parent
                .join(dst.file_name().unwrap_or_default())
                .starts_with(&canonical)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot copy a directory into itself",
                ));
            }
        }
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
// 本地侧的"部分文件"用与锚点同目录、与 basename 长度无关的短隐藏名，
// 独占占位成功后才启动生产者；下载最终用同目录 no-replace rename 就位。

/// One exclusively-created, owner-only staging file. The fixed-size basename
/// keeps valid NAME_MAX-length entries transferable; Drop removes only the
/// candidate this instance successfully reserved.
struct StagedFile {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl StagedFile {
    fn beside(anchor: &Path) -> io::Result<(Self, std::fs::File)> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        Self::beside_with(anchor, || NEXT.fetch_add(1, Ordering::SeqCst))
    }

    fn beside_with(
        anchor: &Path,
        mut next: impl FnMut() -> usize,
    ) -> io::Result<(Self, std::fs::File)> {
        let parent = anchor
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        for _ in 0..32 {
            let path = parent.join(format!(".ember-fs-part-{}-{}", std::process::id(), next()));
            // A legitimate source/destination can have our hidden-name shape.
            // Never reserve the anchor itself, even when it is currently absent.
            if path.file_name() == anchor.file_name() {
                continue;
            }
            match open_transfer_staging(&path) {
                Ok(file) => {
                    use std::os::unix::fs::MetadataExt;

                    let metadata = match file.metadata() {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            drop(file);
                            let _ = std::fs::remove_file(&path);
                            return Err(error);
                        }
                    };
                    return Ok((
                        Self {
                            path,
                            device: metadata.dev(),
                            inode: metadata.ino(),
                        },
                        file,
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not reserve a private transfer staging path",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;

        // Do not unlink a path that was replaced after reservation. On a
        // successful publication the original path is simply absent here.
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.dev() == self.device && metadata.ino() == self.inode)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Private same-parent extraction root for one downloaded directory. Tar never
/// writes into the final namespace; Drop removes only this process-owned tree.
struct ExtractionDir {
    path: PathBuf,
    handle: std::fs::File,
}

impl ExtractionDir {
    fn beside(dst: &Path) -> io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        Self::beside_with(dst, || NEXT.fetch_add(1, Ordering::SeqCst))
    }

    fn beside_with(dst: &Path, mut next: impl FnMut() -> usize) -> io::Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

        let parent = dst
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        for _ in 0..32 {
            let path = parent.join(format!(
                ".ember-fs-extract-{}-{}",
                std::process::id(),
                next()
            ));
            if path.file_name() == dst.file_name() {
                continue;
            }
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&path) {
                Ok(()) => {
                    let handle = match std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                        .open(&path)
                    {
                        Ok(handle) => handle,
                        Err(error) => {
                            let _ = std::fs::remove_dir(&path);
                            return Err(error);
                        }
                    };
                    return Ok(Self { path, handle });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not reserve a private directory extraction path",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ExtractionDir {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;

        let Ok(expected) = self.handle.metadata() else {
            return;
        };
        if std::fs::symlink_metadata(&self.path).is_ok_and(|current| {
            current.file_type().is_dir()
                && current.dev() == expected.dev()
                && current.ino() == expected.ino()
        }) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn extracted_top_level(staging: &Path, dst: &Path) -> io::Result<PathBuf> {
    let expected_name = dst.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
    })?;
    let mut entries = std::fs::read_dir(staging)?;
    let entry = entries.next().transpose()?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "directory archive extracted no top-level entry",
        )
    })?;
    if entry.file_name() != expected_name || entries.next().transpose()?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory archive has an unexpected top-level shape",
        ));
    }
    let path = entry.path();
    if !std::fs::symlink_metadata(&path)?.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory archive top-level entry is not a directory",
        ));
    }
    Ok(path)
}

/// 部分文件通过原子 no-replace rename 就位；失败由调用方清理 temp。
fn finalize_part(temp: &Path, dst: &Path) -> io::Result<()> {
    rename_noreplace(temp, dst)
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

/// 远端 stat 探针的解析结果：类型（d/f/l）与大小（普通文件为字节数，其余 0）。
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
#[allow(dead_code)] // compatibility wrapper used by the library test surface
pub fn transfer(
    src_loc: &FsLocation,
    hosts: &[RemoteHostConfig],
    src: &Path,
    src_is_dir: bool,
    dst_loc: &FsLocation,
    dst_dir: &Path,
    control: TransferControl,
) -> io::Result<PathBuf> {
    let src_endpoint = FsEndpointSnapshot::new(src_loc.clone(), SshExecutionOverlay::default());
    let dst_endpoint = FsEndpointSnapshot::new(dst_loc.clone(), SshExecutionOverlay::default());
    transfer_with_overlays(
        &src_endpoint,
        hosts,
        src,
        src_is_dir,
        &dst_endpoint,
        dst_dir,
        control,
    )
}

pub fn transfer_with_overlays(
    src_endpoint: &FsEndpointSnapshot,
    hosts: &[RemoteHostConfig],
    src: &Path,
    src_is_dir: bool,
    dst_endpoint: &FsEndpointSnapshot,
    dst_dir: &Path,
    control: TransferControl,
) -> io::Result<PathBuf> {
    let name = src.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "source path has no file name")
    })?;
    let dst = dst_dir.join(name);
    if same_files_namespace(&src_endpoint.location, &dst_endpoint.location, hosts) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "same-location transfer should use copy/rename instead",
        ));
    }
    match (
        execution_host_for_location(&src_endpoint.location, hosts, &src_endpoint.overlay)?,
        execution_host_for_location(&dst_endpoint.location, hosts, &dst_endpoint.overlay)?,
    ) {
        (Some(host), None) => download(&host, src, src_is_dir, &dst, control),
        (None, Some(host)) => upload(&host, src, src_is_dir, dst_dir, &dst, control),
        (Some(src_host), Some(dst_host)) => relay(
            &src_host, src, src_is_dir, &dst_host, dst_dir, &dst, control,
        ),
        (None, None) => Err(io::Error::new(
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
    validate_host_for_execution(host)?;
    require_absolute(src)?;
    let arg = path_str(src)?;
    if src_is_dir {
        download_dir(
            &checked_probe_argv(host, "tar", &[arg])?,
            dst,
            MAX_TRANSFER_BYTES,
            control,
        )
    } else {
        download_file(
            &checked_probe_argv(host, "cat", &[arg])?,
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
    let (temp, file) = StagedFile::beside(dst)?;
    run_stream_to_open_file(
        cat_argv,
        PROBE_SCRIPT.as_bytes(),
        file,
        TRANSFER_TIMEOUT,
        max_bytes,
        control,
    )
    .and_then(probe_output_empty)
    .and_then(|()| finalize_part(temp.path(), dst))
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
    let (temp, file) = StagedFile::beside(dst)?;
    let cancel = control.cancel.clone();
    let downloaded = run_stream_to_open_file(
        tar_argv,
        PROBE_SCRIPT.as_bytes(),
        file,
        TRANSFER_TIMEOUT,
        max_bytes,
        control,
    )
    .and_then(probe_output_empty);
    downloaded.and_then(|()| {
        let staging = ExtractionDir::beside(dst)?;
        let argv = vec![
            "tar".to_string(),
            "xf".to_string(),
            path_str(temp.path())?.to_string(),
            "-C".to_string(),
            path_str(staging.path())?.to_string(),
        ];
        run_capture_with_cancel(&argv, &[], TRANSFER_TIMEOUT, MAX_SMALL_OUTPUT, cancel)
            .and_then(local_status)?;
        let extracted = extracted_top_level(staging.path(), dst)?;
        rename_noreplace(&extracted, dst)
    })
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
    validate_host_for_execution(host)?;
    require_absolute(dst_dir)?;
    if src_is_dir {
        upload_dir(host, src, dst_dir, dst, MAX_TRANSFER_BYTES, control)
    } else {
        // put 在远端读流之前先给出友好的 17；最终 hard-link publication
        // 才是原子 no-replace 的权威检查。
        let transfer_id = put_transfer_id();
        let argv = checked_sh_c_probe_argv(host, "put", &[path_str(dst)?, &transfer_id])?;
        let result = upload_file(&argv, src, MAX_TRANSFER_BYTES, control);
        if result.as_ref().is_err_and(put_cleanup_needed) {
            cleanup_remote_put(host, dst, &transfer_id);
        }
        result
    }
}

fn put_cleanup_needed(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
    )
}

/// 探针正常失败会自行删除 staging；只有整个进程组被取消/超时
/// kill 时才可能留下目录。此清理不继承已触发的取消令牌，错误仅记录。
fn cleanup_remote_put(host: &RemoteHostConfig, dst: &Path, transfer_id: &str) {
    if let Err(error) = validate_host_for_execution(host) {
        log::warn!("remote upload staging cleanup rejected by execution gate: {error}");
        return;
    }
    let Ok(dst) = path_str(dst) else {
        log::warn!("remote upload staging cleanup skipped a non-UTF-8 path");
        return;
    };
    let command = put_cleanup_command(dst, transfer_id);
    let argv = remote_shell_command_argv(host, &command);
    match run_capture(&argv, &[], PROBE_OP_TIMEOUT, MAX_SMALL_OUTPUT) {
        Ok(capture) if capture.status == Some(0) && !capture.timed_out && !capture.cancelled => {}
        Ok(capture) => log::warn!(
            "remote upload staging cleanup did not complete (status {:?}, timeout {})",
            capture.status,
            capture.timed_out
        ),
        Err(error) => log::warn!("remote upload staging cleanup failed to run: {error}"),
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
    let (temp, file) = StagedFile::beside(src)?;
    let packed = run_stream_to_open_file(
        &tar_argv,
        &[],
        file,
        TRANSFER_TIMEOUT,
        max_bytes,
        legs.control_for(0),
    )
    .and_then(local_status);
    packed.and_then(|()| {
        // 解包腿的字节数接着打包腿累计。untar v3 在解包前原子拒绝
        // 已存在的 <dir>/<name>（检查与解包之间仍有微秒级 TOCTOU 窗口，
        // 这是 tar 合并语义的协议极限，Friendly 错误由 17 映射给出）。
        let base = std::fs::metadata(temp.path())
            .map(|meta| meta.len())
            .unwrap_or(0);
        let untar_argv = checked_sh_c_probe_argv(host, "untar", &[path_str(dst_dir)?, name])?;
        run_stream_from_file(
            &untar_argv,
            temp.path(),
            TRANSFER_TIMEOUT,
            max_bytes,
            legs.control_for(base),
        )
        .and_then(probe_output_empty)
    })
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
    validate_host_for_execution(src_host)?;
    validate_host_for_execution(dst_host)?;
    require_absolute(src)?;
    require_absolute(dst_dir)?;
    // 下载腿会先烧掉流量，所以存在性预检提前做（文件靠 put 的 17 兜底
    // 也来得及，但那时下载已经完成，白传一份）。
    remote_ensure_absent(dst_host, dst)?;
    let legs = LegProgress::new(control);
    let relay_anchor = std::env::temp_dir().join("ember-fs-relay");
    let (temp, file) = StagedFile::beside(&relay_anchor)?;
    let src_arg = path_str(src)?;
    let download_op = if src_is_dir { "tar" } else { "cat" };
    run_stream_to_open_file(
        &checked_probe_argv(src_host, download_op, &[src_arg])?,
        PROBE_SCRIPT.as_bytes(),
        file,
        TRANSFER_TIMEOUT,
        MAX_TRANSFER_BYTES,
        legs.control_for(0),
    )
    .and_then(probe_output_empty)
    .and_then(|()| {
        // 上传腿的字节数接着下载腿累计。
        let base = std::fs::metadata(temp.path())
            .map(|meta| meta.len())
            .unwrap_or(0);
        let (upload_argv, transfer_id) = if src_is_dir {
            // tar 流的顶层名就是 src 的 basename（tar 探针 -C 父目录打包）。
            let name = src
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "source has no UTF-8 file name")
                })?;
            (
                checked_sh_c_probe_argv(dst_host, "untar", &[path_str(dst_dir)?, name])?,
                None,
            )
        } else {
            let transfer_id = put_transfer_id();
            let argv =
                checked_sh_c_probe_argv(dst_host, "put", &[path_str(dst)?, transfer_id.as_str()])?;
            (argv, Some(transfer_id))
        };
        let result = run_stream_from_file(
            &upload_argv,
            temp.path(),
            TRANSFER_TIMEOUT,
            MAX_TRANSFER_BYTES,
            legs.control_for(base),
        )
        .and_then(probe_output_empty);
        if let (Some(transfer_id), Err(error)) = (&transfer_id, &result) {
            if put_cleanup_needed(error) {
                cleanup_remote_put(dst_host, dst, transfer_id);
            }
        }
        result
    })
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
    fn production_list_probe_stamps_limit_and_hidden_policy() {
        assert_eq!(
            list_probe_args(Path::new("/remote/work"), false).unwrap(),
            [
                "/remote/work".to_string(),
                (MAX_DIRECTORY_ENTRIES + 1).to_string(),
                "0".to_string(),
            ]
        );
        assert_eq!(
            list_probe_args(Path::new("/remote/work"), true).unwrap()[2],
            "1"
        );
        assert!(list_probe_args(Path::new("relative"), false).is_err());
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
        // put/untar 走 sh -c 内联脚本：$1=op、$2=路径，put 的 $3=令牌；
        // stdin 整个留给载荷。
        let argv = sh_c_probe_argv(&docker_host(), "put", &["/tmp/dst file", "feed-1"]);
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
                "feed-1",
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

    #[test]
    fn remote_shell_cleanup_keeps_one_command_argument_per_transport() {
        let command = "i=0; while false; do :; done";
        let docker = remote_shell_command_argv(&docker_host(), command);
        assert_eq!(
            &docker[docker.len() - 3..],
            &["sh", "-c", command],
            "docker executes the generated command directly"
        );

        let ssh = remote_shell_command_argv(&ssh_host(), command);
        assert_eq!(ssh.last().map(String::as_str), Some(command));
        assert_eq!(
            ssh.iter().filter(|argument| *argument == command).count(),
            1,
            "ssh receives exactly one remote command element"
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

        let shown = parse_list_with_hidden(bytes, Path::new("/base"), true);
        let shown_names: Vec<&str> = shown.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(shown_names, vec![".visible", "kept.txt"]);
    }

    #[test]
    fn parse_list_keeps_exact_utf8_names_and_rejects_ambiguous_operands() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"f\0my file.txt\0");
        bytes.extend_from_slice(b"f\0line\nbreak\0");
        bytes.extend_from_slice(b"f\0bad\xffname\0");
        bytes.extend_from_slice(b"f\0duplicate\0d\0duplicate\0");
        let entries = parse_list(&bytes, Path::new("/base"));
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["line\nbreak", "my file.txt"]);
        assert!(entries.iter().all(|entry| !entry.name.contains('\u{fffd}')));
        assert!(entries.iter().all(|entry| entry.name != "duplicate"));
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

        let mut visually_deceptive = ssh_host();
        visually_deceptive.name.push('\u{202e}');
        let error = run_probe(
            &visually_deceptive,
            "home",
            &[],
            Duration::from_secs(1),
            MAX_SMALL_OUTPUT,
        )
        .expect_err("private probe boundary must recheck the app gate");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let error = list_dir(
            &FsLocation::Remote(0),
            &[visually_deceptive],
            Path::new("/"),
        )
        .expect_err("visual-spoofing host must fail before spawning ssh");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let over_limit = vec![ssh_host(); crate::config::MAX_REMOTE_HOSTS + 1];
        let error = list_dir(
            &FsLocation::Remote(crate::config::MAX_REMOTE_HOSTS),
            &over_limit,
            Path::new("/"),
        )
        .expect_err("129th host must remain inactive before spawning ssh");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("active limit"));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn atomic_rename_refuses_a_destination_created_after_preflight() {
        let dir = TestDir::new();
        let src = dir.join("source");
        let dst = dir.join("destination");
        std::fs::write(&src, b"source bytes").unwrap();

        let error = rename_noreplace_with(&src, &dst, |src, dst| {
            // Deterministically occupy the name after ensure_absent returned
            // but before the real production commit primitive runs.
            std::fs::write(dst, b"racing winner")?;
            atomic_rename_noreplace(src, dst)
        })
        .expect_err("the concurrent destination must win");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&src).unwrap(), b"source bytes");
        assert_eq!(std::fs::read(&dst).unwrap(), b"racing winner");
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
    fn local_copy_refuses_to_copy_a_directory_into_itself() {
        let dir = TestDir::new();
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();

        // 直接套娃：cp -r src src/inner。
        let error = copy(&FsLocation::Local, &[], &src, &src.join("inner"))
            .expect_err("copy into own subdirectory");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!src.join("inner").exists());

        // 符号链接别名：dst 父目录经链接指向 src，规范化后同样被拒。
        std::os::unix::fs::symlink(&src, dir.join("alias")).unwrap();
        let error = copy(
            &FsLocation::Local,
            &[],
            &src,
            &dir.join("alias").join("inner"),
        )
        .expect_err("copy into itself through a symlink alias");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        // 兄弟目录不受影响。
        copy(&FsLocation::Local, &[], &src, &dir.join("sibling")).unwrap();
        assert!(dir.join("sibling/nested").is_dir());
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
        let item = FsClipboardItem {
            path: PathBuf::from("/home/yj/notes.md"),
            is_dir: false,
        };
        assert_eq!(
            item.paste_destination(Path::new("/tmp/target")),
            Some(PathBuf::from("/tmp/target/notes.md"))
        );
        let root_item = FsClipboardItem {
            path: PathBuf::from("/"),
            is_dir: true,
        };
        assert_eq!(root_item.paste_destination(Path::new("/tmp")), None);
    }

    #[test]
    fn location_labels() {
        let hosts = vec![ssh_host(), docker_host()];
        assert_eq!(FsLocation::Local.label(&hosts), "Local");
        assert_eq!(FsLocation::Remote(0).label(&hosts), "ssh: devbox");
        assert_eq!(FsLocation::Remote(1).label(&hosts), "docker: myubuntu");
        assert!(FsLocation::Remote(9).label(&hosts).contains('#'));
        let transient = FsLocation::Transient(hosts[0].clone());
        assert_eq!(transient.label(&[]), "ssh: devbox (temporary)");
        assert_eq!(
            host_for_location(&transient, &[]).unwrap().unwrap(),
            &hosts[0]
        );
    }

    #[test]
    fn control_path_is_split_from_stable_ssh_args_in_supported_spellings() {
        for (args, expected_base, expected_path) in [
            (
                vec!["-p", "22", "-S", "/tmp/cm-%C"],
                vec!["-p", "22"],
                "/tmp/cm-%C",
            ),
            (
                vec!["-S/tmp/cm-joined-%C", "-p", "22"],
                vec!["-p", "22"],
                "/tmp/cm-joined-%C",
            ),
            (
                vec!["-o", "ControlPath=~/.ssh/cm-%C", "-p", "22"],
                vec!["-p", "22"],
                "~/.ssh/cm-%C",
            ),
            (
                vec!["-oControlPath=/tmp/cm-inline-%C", "-p", "22"],
                vec!["-p", "22"],
                "/tmp/cm-inline-%C",
            ),
        ] {
            let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
            let (base, path) = split_ssh_control_path_args(&args);
            assert_eq!(base, expected_base);
            assert_eq!(path.as_deref(), Some(expected_path));
        }
    }

    #[test]
    fn execution_overlay_overrides_profile_socket_without_mutating_identity() {
        let mut configured = ssh_host();
        configured
            .ssh_args
            .extend(["-o".to_string(), "ControlPath=/tmp/saved-%C".to_string()]);
        let original = configured.clone();
        let hosts = vec![configured];
        let overlay = SshExecutionOverlay::from_control_path(Some(
            "/run/user/1000/anvil/live-%C".to_string(),
        ));
        let execution = execution_host_for_location(&FsLocation::Remote(0), &hosts, &overlay)
            .unwrap()
            .unwrap();

        assert_eq!(
            hosts[0], original,
            "stable configured identity is immutable"
        );
        assert_eq!(
            execution.ssh_args,
            ["-p", "2222", "-S", "/run/user/1000/anvil/live-%C",]
        );
    }

    #[test]
    fn saved_and_transient_control_paths_share_one_files_namespace() {
        let mut saved = ssh_host();
        saved
            .ssh_args
            .extend(["-S".to_string(), "/tmp/saved-%C".to_string()]);
        let mut transient = ssh_host();
        transient.name = "observed".to_string();
        let hosts = vec![saved];
        assert!(same_files_namespace(
            &FsLocation::Remote(0),
            &FsLocation::Transient(transient.clone()),
            &hosts,
        ));
        assert!(same_files_namespace(
            &FsLocation::Transient(transient),
            &FsLocation::Remote(0),
            &hosts,
        ));
    }

    #[test]
    fn direct_same_namespace_operation_prefers_any_live_execution_socket() {
        let source = SshExecutionOverlay::from_control_path(Some("/tmp/source-%C".to_string()));
        let destination = SshExecutionOverlay::default();
        assert_eq!(
            same_namespace_execution_overlay(&source, &destination),
            &source,
            "a saved destination must not discard the transient clipboard socket",
        );

        let destination =
            SshExecutionOverlay::from_control_path(Some("/tmp/destination-%C".to_string()));
        assert_eq!(
            same_namespace_execution_overlay(&source, &destination),
            &destination,
            "the active destination socket is freshest when both sides have one",
        );
    }

    #[test]
    fn execution_overlay_rejects_local_docker_and_control_characters() {
        let overlay = SshExecutionOverlay::from_control_path(Some("/tmp/cm-%C".to_string()));
        assert!(validate_execution_endpoint(&FsLocation::Local, &[], &overlay).is_err());
        assert!(
            validate_execution_endpoint(&FsLocation::Transient(docker_host()), &[], &overlay,)
                .is_err()
        );
        let unsafe_overlay =
            SshExecutionOverlay::from_control_path(Some("/tmp/cm\nmalicious".to_string()));
        assert!(validate_execution_endpoint(
            &FsLocation::Transient(ssh_host()),
            &[],
            &unsafe_overlay,
        )
        .is_err());
    }

    #[test]
    fn execution_overlay_accepts_only_cwd_independent_control_paths() {
        let target = FsLocation::Transient(ssh_host());
        for accepted in ["/run/user/1000/ember/cm-%C", "~/.ssh/cm-%C"] {
            let overlay = SshExecutionOverlay::from_control_path(Some(accepted.to_string()));
            validate_execution_endpoint(&target, &[], &overlay).unwrap();
        }
        for rejected in ["./cm-%C", "cm-%C", "~other/.ssh/cm-%C", "~", "~/"] {
            let overlay = SshExecutionOverlay::from_control_path(Some(rejected.to_string()));
            let error = validate_execution_endpoint(&target, &[], &overlay).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(
                error.to_string().contains("absolute")
                    && error.to_string().contains("saved remote profile"),
                "{rejected:?}: {error}"
            );
        }
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
    fn run_capture_timeout_kills_the_whole_process_group() {
        // 直接子进程（sh）被 kill 后，持有 stdout 管道的子孙也必须一起死，
        // 否则读端迟迟等不到 EOF、run_capture 要多挂几十秒。
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 30 & wait".to_string(),
        ];
        let started = std::time::Instant::now();
        let capture = run_capture(&argv, &[], Duration::from_millis(150), 1024).unwrap();
        assert!(capture.timed_out);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "descendant survived the kill and held the pipes"
        );
    }

    #[test]
    fn run_capture_cancellation_retires_a_remote_probe_process_group() {
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 30 & wait".to_string(),
        ];
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        let setter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            trigger.store(true, Ordering::SeqCst);
        });
        let started = std::time::Instant::now();
        let capture =
            run_capture_with_cancel(&argv, &[], Duration::from_secs(30), 1024, Some(cancel))
                .unwrap();
        setter.join().unwrap();
        assert!(capture.cancelled);
        assert!(!capture.timed_out);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancelled probe descendants must not hold stdout/stderr open"
        );
    }

    #[test]
    fn read_bounded_reports_truncation_and_drains_the_rest() {
        let (bytes, truncated) = read_bounded(&b"abcdef"[..], 3).unwrap();
        assert_eq!((bytes.as_slice(), truncated), (&b"abc"[..], true));
        let (bytes, truncated) = read_bounded(&b"abc"[..], 3).unwrap();
        assert_eq!((bytes.as_slice(), truncated), (&b"abc"[..], false));
    }

    #[test]
    fn run_capture_errors_when_output_exceeds_the_cap() {
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "head -c 100000 /dev/zero | tr '\\0' 'a'".to_string(),
        ];
        let error = run_capture(&argv, &[], Duration::from_secs(5), 1024)
            .expect_err("over-cap output must be an error, not silent truncation");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn probe_output_maps_exit_codes() {
        let error = probe_output(capture(Some(17), "")).expect_err("17");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        let error = probe_output(capture(Some(3), "")).expect_err("3");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let error = probe_output(capture(Some(13), "hostile secret=credential")).expect_err("13");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "permission denied");
        let error = probe_output(capture(Some(20), "")).expect_err("20");
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
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
        assert!(is_retryable_error(&error));
        assert!(!is_retryable_error(&io::Error::new(
            io::ErrorKind::InvalidData,
            "bad protocol"
        )));
    }

    #[test]
    fn home_output_is_strict_utf8_single_line_and_absolute() {
        assert_eq!(
            parse_home_output(b"/remote/home\n").unwrap(),
            Path::new("/remote/home")
        );
        assert_eq!(
            parse_home_output(b"/remote/home\r\n").unwrap(),
            Path::new("/remote/home")
        );
        for invalid in [
            b"relative\n".as_slice(),
            b"/one\n/two\n".as_slice(),
            b"/one\0two\n".as_slice(),
            b"\xff\n".as_slice(),
            b"\n".as_slice(),
        ] {
            assert!(parse_home_output(invalid).is_err(), "invalid={invalid:?}");
        }
    }

    #[test]
    fn inline_remote_diagnostics_are_redacted_control_safe_and_unicode_bounded() {
        let diagnostic = format!(
            "permission denied\u{202e}\nignored token=super-secret password: hunter2 {}",
            "远".repeat(240)
        );
        let first = safe_inline_diagnostic(&diagnostic);
        assert!(!first.contains('\u{202e}'));
        assert!(!first.contains("super-secret"));
        assert!(!first.contains("hunter2"));
        // The first non-empty line is authoritative, so later attacker text
        // never leaks into a probe error either.
        assert_eq!(first, "permission denied");

        let long = safe_inline_diagnostic(&format!("token=super-secret {}", "远".repeat(240)));
        assert!(long.contains("token=<redacted>"));
        assert!(!long.contains("super-secret"));
        assert!(long.chars().count() <= MAX_INLINE_DIAGNOSTIC_CHARS + 1);
        assert!(long.ends_with('…'));

        let error = io::Error::other("Authorization: Bearer-credential\u{0007}");
        let visible = user_facing_error(&error);
        assert!(!visible.contains("Bearer-credential"));
        assert!(!visible.chars().any(char::is_control));
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
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("sub dir"), dir.join("linked dir")).unwrap();

        let dir_arg = dir.path().to_str().unwrap().to_string();
        let capture = run_probe_locally(&["list", &dir_arg]);
        assert_eq!(capture.status, Some(0), "stderr: {:?}", capture.stderr);
        let entries = parse_list(&capture.stdout, dir.path());
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // dotfile 在 Rust 侧过滤；空格名原样保留。
        assert_eq!(names, vec!["sub dir", "file.txt", "linked dir"]);
        assert!(entries[0].is_dir);
        #[cfg(unix)]
        assert!(
            !entries
                .iter()
                .find(|entry| entry.name == "linked dir")
                .unwrap()
                .is_dir,
            "a symlink to a directory must never become an expandable row"
        );

        let capture = run_probe_locally(&["list", &dir_arg, "2", "0"]);
        assert_eq!(capture.status, Some(0), "stderr: {:?}", capture.stderr);
        let limited = parse_list_with_hidden(&capture.stdout, dir.path(), true);
        assert_eq!(limited.len(), 2, "the remote probe enforces its row cap");
        assert!(limited.iter().all(|entry| !entry.name.starts_with('.')));

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

        let plain_file = dir.join("not-a-directory");
        std::fs::write(&plain_file, b"x").unwrap();
        assert_eq!(
            run_probe_locally(&["list", plain_file.to_str().unwrap()]).status,
            Some(20)
        );

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
    fn probe_creators_refuse_dangling_symlink_targets() {
        fn assert_refused(capture: &Capture, link: &Path, victim: &Path) {
            assert_eq!(capture.status, Some(17), "stderr: {:?}", capture.stderr);
            assert!(
                !victim.exists(),
                "probe followed dangling link and created {}",
                victim.display()
            );
            assert!(
                std::fs::symlink_metadata(link)
                    .expect("destination link must remain")
                    .file_type()
                    .is_symlink(),
                "probe replaced destination link {}",
                link.display()
            );
        }

        let dir = TestDir::new();
        let dangling = |name: &str| {
            let victim = dir.path().join(format!("outside-{name}"));
            let link = dir.path().join(format!("link-{name}"));
            std::os::unix::fs::symlink(&victim, &link).unwrap();
            let argument = link.to_str().unwrap().to_string();
            (victim, link, argument)
        };

        let (victim, link, argument) = dangling("mkdir");
        assert_refused(&run_probe_locally(&["mkdir", &argument]), &link, &victim);

        let (victim, link, argument) = dangling("mkfile");
        assert_refused(&run_probe_locally(&["mkfile", &argument]), &link, &victim);

        let move_source = dir.join("move-source");
        std::fs::write(&move_source, b"move").unwrap();
        let move_source_arg = move_source.to_str().unwrap().to_string();
        let (victim, link, argument) = dangling("move");
        assert_refused(
            &run_probe_locally(&["mv", &move_source_arg, &argument]),
            &link,
            &victim,
        );
        assert_eq!(std::fs::read(&move_source).unwrap(), b"move");

        let copy_source = dir.join("copy-source");
        std::fs::write(&copy_source, b"copy").unwrap();
        let copy_source_arg = copy_source.to_str().unwrap().to_string();
        let (victim, link, argument) = dangling("copy");
        assert_refused(
            &run_probe_locally(&["cp", &copy_source_arg, &argument]),
            &link,
            &victim,
        );
        assert_eq!(std::fs::read(&copy_source).unwrap(), b"copy");

        let (victim, link, argument) = dangling("put");
        let capture = run_capture(
            &sh_c_argv_locally("put", &[&argument, "feed-1"]),
            b"payload",
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_refused(&capture, &link, &victim);

        let extraction_dir = dir.join("extract");
        std::fs::create_dir(&extraction_dir).unwrap();
        let extraction_arg = extraction_dir.to_str().unwrap().to_string();
        let victim = dir.join("outside-untar");
        let link = extraction_dir.join("tree");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let capture = run_capture(
            &sh_c_argv_locally("untar", &[&extraction_arg, "tree"]),
            b"not consulted when the target is occupied",
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_refused(&capture, &link, &victim);
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
    fn probe_v6_put_writes_privately_and_refuses_existing() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new();
        let file = dir.join("upload.bin");
        let arg = file.to_str().unwrap().to_string();
        let payload = binary_sample();

        let capture = run_capture(
            &sh_c_argv_locally("put", &[&arg, "feed-1"]),
            &payload,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(0), "stderr: {:?}", capture.stderr);
        assert_eq!(std::fs::read(&file).unwrap(), payload);
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o077,
            0,
            "remote upload publication must remain owner-only"
        );

        // 已存在 → 17，且临时文件不残留。
        let capture = run_capture(
            &sh_c_argv_locally("put", &[&arg, "feed-2"]),
            &payload,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(17));
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".ember-fs-put-")
            })
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");

        // 目标目录不存在 → 4。
        let bad = dir.join("missing/file").to_str().unwrap().to_string();
        let capture = run_capture(
            &sh_c_argv_locally("put", &[&bad, "feed-3"]),
            &payload,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(4));

        // A filesystem-limit destination remains valid because the staging
        // component is fixed-size and lives beside it.
        let long = dir.path().join("x".repeat(255));
        let long_arg = long.to_str().unwrap().to_string();
        let capture = run_capture(
            &sh_c_argv_locally("put", &[&long_arg, "feed-4"]),
            &payload,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(0), "stderr: {:?}", capture.stderr);
        assert_eq!(std::fs::read(long).unwrap(), payload);

        let invalid_target = dir.join("invalid.bin");
        let invalid_arg = invalid_target.to_str().unwrap().to_string();
        let too_long = "a".repeat(97);
        for invalid_args in [
            vec![invalid_arg.as_str()],
            vec![invalid_arg.as_str(), "bad_token"],
            vec![invalid_arg.as_str(), &too_long],
        ] {
            let capture = run_capture(
                &sh_c_argv_locally("put", &invalid_args),
                &payload,
                Duration::from_secs(5),
                MAX_SMALL_OUTPUT,
            )
            .unwrap();
            assert_eq!(capture.status, Some(2));
        }
        assert!(!invalid_target.exists());
    }

    #[test]
    fn probe_v6_put_atomically_preserves_a_racing_destination() {
        let dir = TestDir::new();
        let file = dir.join("upload.bin");
        let arg = file.to_str().unwrap().to_string();
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(PROBE_SCRIPT)
            .arg("remote-fs-probe")
            .arg("put")
            .arg(&arg)
            .arg("feed-5")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();

        // The child creates private staging before blocking in `cat`. Hold
        // stdin open until that point, then make the final name win the race.
        let started = std::time::Instant::now();
        loop {
            let staging_exists = std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".ember-fs-put-")
                });
            if staging_exists {
                break;
            }
            if started.elapsed() > Duration::from_secs(2) {
                drop(stdin);
                let _ = child.kill();
                let _ = child.wait();
                panic!("put probe did not reserve staging");
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        std::fs::write(&file, b"keep").unwrap();
        stdin.write_all(&binary_sample()).unwrap();
        drop(stdin);
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(17));
        assert_eq!(std::fs::read(&file).unwrap(), b"keep");
        assert!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".ember-fs-put-")),
            "private staging must be cleaned after a lost publication race"
        );
    }

    #[test]
    fn put_transfer_ids_are_unique_shell_safe_and_bounded() {
        let first = put_transfer_id();
        let second = put_transfer_id();
        assert_ne!(first, second);
        for id in [first, second] {
            assert!(id.len() <= 96);
            assert!(
                id.bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-'),
                "unexpected transfer id: {id}"
            );
        }
    }

    #[test]
    fn put_cleanup_is_reserved_for_cancelled_or_timed_out_probes() {
        assert!(put_cleanup_needed(&cancelled_error()));
        assert!(put_cleanup_needed(&io::Error::new(
            io::ErrorKind::TimedOut,
            "timeout"
        )));
        assert!(!put_cleanup_needed(&io::Error::new(
            io::ErrorKind::AlreadyExists,
            "probe cleaned itself"
        )));
        assert!(!put_cleanup_needed(&io::Error::other("ordinary failure")));
    }

    #[test]
    fn put_cleanup_command_enumerates_only_token_candidates() {
        assert_eq!(
            put_cleanup_command("/dst/dir name", "feed-1"),
            "i=0; while [ \"$i\" -lt 32 ]; do d='/dst/.ember-fs-put-feed-1-'$i; i=$((i + 1)); [ \"$d\" = '/dst/dir name' ] && continue; [ -d \"$d\" ] && [ ! -L \"$d\" ] || continue; rm -f \"$d/payload\"; rmdir \"$d\" 2>/dev/null || :; done"
        );
        assert_eq!(
            put_cleanup_command("/dst/don't", "feed-1"),
            "i=0; while [ \"$i\" -lt 32 ]; do d='/dst/.ember-fs-put-feed-1-'$i; i=$((i + 1)); [ \"$d\" = '/dst/don'\\''t' ] && continue; [ -d \"$d\" ] && [ ! -L \"$d\" ] || continue; rm -f \"$d/payload\"; rmdir \"$d\" 2>/dev/null || :; done"
        );
    }

    #[test]
    fn put_cleanup_command_preserves_target_links_and_other_uploads() {
        let dir = TestDir::new();
        let stage =
            |token: &str, index: usize| dir.path().join(format!(".ember-fs-put-{token}-{index}"));
        for index in [0, 31] {
            std::fs::create_dir(stage("feed-1", index)).unwrap();
            std::fs::write(stage("feed-1", index).join("payload"), b"partial").unwrap();
        }
        let outside_range = stage("feed-1", 32);
        std::fs::create_dir(&outside_range).unwrap();
        std::fs::write(outside_range.join("payload"), b"keep").unwrap();
        let other_transfer = stage("beef-2", 0);
        std::fs::create_dir(&other_transfer).unwrap();
        std::fs::write(other_transfer.join("payload"), b"keep").unwrap();

        let victim = dir.join("victim");
        std::fs::create_dir(&victim).unwrap();
        std::fs::write(victim.join("payload"), b"keep").unwrap();
        let planted_link = stage("feed-1", 5);
        std::os::unix::fs::symlink(&victim, &planted_link).unwrap();

        // A final destination that happens to resemble an internal candidate
        // is explicitly excluded from cleanup.
        let dst = stage("feed-1", 7);
        std::fs::create_dir(&dst).unwrap();
        std::fs::write(dst.join("payload"), b"destination").unwrap();

        let command = put_cleanup_command(dst.to_str().unwrap(), "feed-1");
        let capture = run_capture(
            &["sh".to_string(), "-c".to_string(), command],
            &[],
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(0), "stderr: {:?}", capture.stderr);
        assert_eq!(std::fs::read(dst.join("payload")).unwrap(), b"destination");
        assert!(!stage("feed-1", 0).exists());
        assert!(!stage("feed-1", 31).exists());
        assert!(outside_range.is_dir(), "indices outside retries survive");
        assert!(other_transfer.is_dir(), "a concurrent upload survives");
        assert!(planted_link.is_symlink(), "cleanup refuses planted links");
        assert_eq!(std::fs::read(victim.join("payload")).unwrap(), b"keep");
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
    fn probe_tar_uses_root_as_the_parent_of_a_root_level_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new();
        let fake_tar = dir.join("tar");
        std::fs::write(&fake_tar, b"#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        let mut permissions = std::fs::metadata(&fake_tar).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_tar, permissions).unwrap();

        let argv = vec![
            "env".to_string(),
            format!("PATH={}:/usr/bin:/bin", dir.path().display()),
            "sh".to_string(),
            "-s".to_string(),
            "--".to_string(),
            "tar".to_string(),
            "/tmp".to_string(),
        ];
        let capture = run_capture(
            &argv,
            PROBE_SCRIPT.as_bytes(),
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
        )
        .unwrap();
        assert_eq!(capture.status, Some(0), "stderr: {:?}", capture.stderr);
        assert_eq!(
            capture.stdout, b"cf\n-\n-C\n/\ntmp\n",
            "the tar probe must never pass an empty root-level parent"
        );
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
        let dir_link = dir.join("dir-link");
        std::os::unix::fs::symlink(&sub, &dir_link).unwrap();
        let fifo = dir.join("fifo");
        let fifo_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(
            // SAFETY: `fifo_path` is a live NUL-terminated path and the mode
            // contains only ordinary permission bits.
            unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) },
            0,
            "mkfifo failed: {}",
            io::Error::last_os_error()
        );

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
        assert_eq!(
            parse_stat(&stat(&dir_link).stdout),
            Some(RemoteStat {
                kind: b'l',
                size: 0
            }),
            "a link to a directory must keep the list protocol's link type"
        );
        assert_eq!(
            parse_stat(&stat(&fifo).stdout),
            Some(RemoteStat {
                kind: b'f',
                size: 0
            }),
            "a FIFO must count as occupied without being opened for a size read"
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
    fn stream_staging_refuses_a_symlink_before_spawning() {
        let dir = TestDir::new();
        let victim = dir.join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        let staging = dir.join("staging");
        std::os::unix::fs::symlink(&victim, &staging).unwrap();
        let marker = dir.join("producer-started");
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf started > \"$1\"; printf payload".to_string(),
            "--".to_string(),
            marker.to_str().unwrap().to_string(),
        ];

        let error = run_stream_to_file(
            &argv,
            &[],
            &staging,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
            TransferControl::default(),
        )
        .expect_err("an occupied staging name must be refused");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert!(std::fs::symlink_metadata(&staging)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(
            !marker.exists(),
            "producer started before staging was reserved"
        );

        let safe_staging = dir.join("safe-staging");
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf payload".to_string(),
        ];
        let capture = run_stream_to_file(
            &argv,
            &[],
            &safe_staging,
            Duration::from_secs(5),
            MAX_SMALL_OUTPUT,
            TransferControl::default(),
        )
        .unwrap();
        assert_eq!(capture.status, Some(0));
        assert_eq!(std::fs::read(&safe_staging).unwrap(), b"payload");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&safe_staging)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0,
            "partial transfer content must remain owner-only"
        );
    }

    #[test]
    fn unique_transfer_staging_skips_aliases_and_planted_symlinks() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new();
        let pid = std::process::id();

        // If a legitimate target has our internal name shape, the candidate
        // bearing that exact basename is skipped without creating the target.
        let anchor = dir.path().join(format!(".ember-fs-part-{pid}-7"));
        let mut sequence = [7, 8].into_iter();
        let (staging, file) =
            StagedFile::beside_with(&anchor, || sequence.next().expect("one retry is enough"))
                .unwrap();
        let staging_path = staging.path().to_path_buf();
        assert_ne!(staging_path, anchor);
        assert_eq!(
            std::fs::metadata(&staging_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0,
            "reserved staging must remain owner-only"
        );
        drop(file);
        drop(staging);
        assert!(!anchor.exists());
        assert!(!staging_path.exists());

        // An occupied candidate is retried atomically. In particular, a
        // planted symlink remains a symlink and its target is untouched.
        let victim = dir.join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        let planted = dir.path().join(format!(".ember-fs-part-{pid}-11"));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        let ordinary_anchor = dir.join("download.bin");
        let mut sequence = [11, 12].into_iter();
        let (staging, file) = StagedFile::beside_with(&ordinary_anchor, || {
            sequence.next().expect("one retry is enough")
        })
        .unwrap();
        let expected_name = format!(".ember-fs-part-{pid}-12");
        assert_eq!(
            staging.path().file_name(),
            Some(std::ffi::OsStr::new(&expected_name))
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert!(std::fs::symlink_metadata(&planted)
            .unwrap()
            .file_type()
            .is_symlink());
        drop(file);
        drop(staging);
        assert!(std::fs::symlink_metadata(&planted)
            .unwrap()
            .file_type()
            .is_symlink());

        // Cleanup is inode-bound as well as path-bound: replacing a reserved
        // name cannot trick Drop into deleting the replacement.
        let replacement_anchor = dir.join("replacement.bin");
        let (staging, file) = StagedFile::beside(&replacement_anchor).unwrap();
        let staging_path = staging.path().to_path_buf();
        std::fs::remove_file(&staging_path).unwrap();
        std::fs::write(&staging_path, b"replacement").unwrap();
        drop(file);
        drop(staging);
        assert_eq!(std::fs::read(&staging_path).unwrap(), b"replacement");
    }

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
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".ember-fs-part-")
            })
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
            &sh_c_argv_locally("put", &[&dst_arg, "feed-6"]),
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
    fn download_accepts_a_name_at_the_component_limit() {
        let remote = TestDir::new();
        let src = remote.join("source.bin");
        std::fs::write(&src, binary_sample()).unwrap();
        let src_arg = src.to_str().unwrap().to_string();

        let local = TestDir::new();
        let dst = local.path().join("x".repeat(255));
        download_file(
            &sh_s_argv_locally("cat", &[&src_arg]),
            &dst,
            MAX_TRANSFER_BYTES,
            TransferControl::default(),
        )
        .unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), binary_sample());
        assert_eq!(dst.file_name().unwrap().as_encoded_bytes().len(), 255);
        assert_eq!(
            std::fs::read_dir(local.path()).unwrap().count(),
            1,
            "fixed-size staging names leave no litter"
        );
    }

    #[test]
    fn extraction_staging_is_private_collision_safe_and_identity_bound() {
        use std::os::unix::fs::PermissionsExt;

        let local = TestDir::new();
        let dst = local.join("tree");
        let pid = std::process::id();
        let victim = local.join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        let planted = local.path().join(format!(".ember-fs-extract-{pid}-11"));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        let mut sequence = [11, 12].into_iter();
        let staging =
            ExtractionDir::beside_with(&dst, || sequence.next().expect("one retry is enough"))
                .unwrap();
        assert_eq!(
            std::fs::metadata(staging.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0,
            "archive extraction must remain owner-only"
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        drop(staging);
        assert!(std::fs::symlink_metadata(&planted)
            .unwrap()
            .file_type()
            .is_symlink());

        let staging = ExtractionDir::beside(&dst).unwrap();
        let replaced_path = staging.path().to_path_buf();
        std::fs::remove_dir(&replaced_path).unwrap();
        std::fs::create_dir(&replaced_path).unwrap();
        std::fs::write(replaced_path.join("replacement"), b"survive").unwrap();
        drop(staging);
        assert_eq!(
            std::fs::read(replaced_path.join("replacement")).unwrap(),
            b"survive",
            "inode-bound cleanup must preserve a replacement directory"
        );
    }

    #[test]
    fn directory_download_publishes_once_and_preserves_a_racing_destination() {
        let remote = TestDir::new();
        let source = remote.join("tree");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("downloaded.txt"), b"downloaded").unwrap();
        let source_arg = source.to_str().unwrap().to_string();

        let local = TestDir::new();
        let dst = local.join("tree");
        download_dir(
            &sh_s_argv_locally("tar", &[&source_arg]),
            &dst,
            MAX_TRANSFER_BYTES,
            TransferControl::default(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(dst.join("downloaded.txt")).unwrap(),
            b"downloaded"
        );
        std::fs::remove_dir_all(&dst).unwrap();

        let tar_bytes = run_capture(
            &sh_s_argv_locally("tar", &[&source_arg]),
            PROBE_SCRIPT.as_bytes(),
            Duration::from_secs(5),
            MAX_LIST_OUTPUT,
        )
        .unwrap()
        .stdout;
        let tar_file = remote.join("tree.tar");
        std::fs::write(&tar_file, tar_bytes).unwrap();
        let producer = vec![
            "sh".to_string(),
            "-c".to_string(),
            "mkdir \"$1\" && printf keep > \"$1/marker\" && cat \"$2\"".to_string(),
            "--".to_string(),
            dst.to_str().unwrap().to_string(),
            tar_file.to_str().unwrap().to_string(),
        ];

        let error = download_dir(
            &producer,
            &dst,
            MAX_TRANSFER_BYTES,
            TransferControl::default(),
        )
        .expect_err("the destination created during streaming must win");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(dst.join("marker")).unwrap(), b"keep");
        assert!(
            !dst.join("downloaded.txt").exists(),
            "archive content merged into the racing destination"
        );
        let entries: Vec<_> = std::fs::read_dir(local.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, [std::ffi::OsString::from("tree")]);
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(&relay_temp).unwrap().permissions().mode() & 0o777,
                0o600,
                "publishing must preserve the private staging mode"
            );
        }
        let dst_arg = final_path.to_str().unwrap().to_string();
        upload_file(
            &sh_c_argv_locally("put", &[&dst_arg, "feed-7"]),
            &relay_temp,
            MAX_TRANSFER_BYTES,
            TransferControl::default(),
        )
        .unwrap();
        std::fs::remove_file(&relay_temp).unwrap();
        assert_eq!(std::fs::read(&final_path).unwrap(), binary_sample());
    }

    #[test]
    fn directory_stream_relay_round_trips_through_private_staging() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new();
        let remote_a = dir.join("remote-a");
        let source = remote_a.join("tree");
        std::fs::create_dir_all(source.join("sub")).unwrap();
        std::fs::write(source.join("sub/blob.bin"), binary_sample()).unwrap();
        let source_arg = source.to_str().unwrap().to_string();

        let relay_anchor = dir.join("relay-anchor");
        let (staging, file) = StagedFile::beside(&relay_anchor).unwrap();
        let staging_path = staging.path().to_path_buf();
        run_stream_to_open_file(
            &sh_s_argv_locally("tar", &[&source_arg]),
            PROBE_SCRIPT.as_bytes(),
            file,
            TRANSFER_TIMEOUT,
            MAX_TRANSFER_BYTES,
            TransferControl::default(),
        )
        .and_then(probe_output_empty)
        .unwrap();
        assert_eq!(
            std::fs::metadata(&staging_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0,
            "relay payloads remain owner-only between legs"
        );

        let remote_b = dir.join("remote-b");
        std::fs::create_dir(&remote_b).unwrap();
        let remote_b_arg = remote_b.to_str().unwrap().to_string();
        run_stream_from_file(
            &sh_c_argv_locally("untar", &[&remote_b_arg, "tree"]),
            &staging_path,
            TRANSFER_TIMEOUT,
            MAX_TRANSFER_BYTES,
            TransferControl::default(),
        )
        .and_then(probe_output_empty)
        .unwrap();
        drop(staging);

        assert!(!staging_path.exists(), "relay staging cleans up on success");
        assert_eq!(
            std::fs::read(remote_b.join("tree/sub/blob.bin")).unwrap(),
            binary_sample()
        );
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
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".ember-fs-part-")
            })
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
