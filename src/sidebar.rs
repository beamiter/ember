use crossbeam_channel::{Receiver, Sender, TryRecvError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use jterm_core::jsh_remote::RemoteHostConfig;

use crate::remote_fs::{self, FsLocation};

/// Keep directory rendering bounded while still allowing every entry to be
/// reached through the explicit "show more" row.
pub const DIRECTORY_PAGE_SIZE: usize = 64;
/// Hard cap for one directory scan. Pagination bounds rendering; this separate
/// cap bounds the worker's sort buffer for hostile or machine-generated trees.
pub const MAX_DIRECTORY_ENTRIES: usize = 16 * 1024;
const MAX_DIRECTORY_SCAN_ENTRIES: usize = MAX_DIRECTORY_ENTRIES * 4;
const SCAN_WORKERS: usize = 2;
const SCAN_RESULT_CAPACITY: usize = 8;
const OP_RESULT_CAPACITY: usize = 8;

type ScanFn = dyn Fn(&Path) -> io::Result<DirectoryListing> + Send + Sync + 'static;

/// 一次目录扫描的后端。本机走注入的 [`ScanFn`]（生产是 scan_dir，测试是虚拟
/// 扫描器）；远程走 remote_fs 探针。主机列表随请求携带快照：排队期间配置
/// 被改动，也不影响这次扫描落到哪台主机。
#[derive(Clone, Debug)]
enum ScanBackend {
    Local,
    Remote(
        Box<remote_fs::FsEndpointSnapshot>,
        Arc<Vec<RemoteHostConfig>>,
    ),
}

/// 侧边栏内容视图。命令时间线在两种 tab 栏布局下都可用；会话列表仅在
/// tab 栏位于侧边栏模式时可选。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarView {
    #[default]
    Files,
    Sessions,
    Commands,
    Tasks,
}

/// Terminal action offered by the Files header. Local browsing can safely
/// start an interactive shell at the exact tree root; a remote tree is not
/// bound to any existing PTY, so its action reconnects the selected profile
/// and lets that profile choose its normal starting directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilesTerminalTarget {
    Local(PathBuf),
    Remote {
        index: usize,
        overlay: remote_fs::SshExecutionOverlay,
    },
    Transient {
        host: RemoteHostConfig,
        overlay: remote_fs::SshExecutionOverlay,
    },
}

/// Stable identity for a Files UI intent that can outlive the tree frame that
/// created it (for example a rename or delete confirmation dialog). Raw remote
/// indices are deliberately not retained: a safe profile reorder remains the
/// same location, while an edited/replaced/ambiguous profile does not.
#[derive(Clone, Debug, PartialEq)]
enum FilesLocationIdentity {
    Local,
    Remote(RemoteHostConfig),
    Transient(RemoteHostConfig),
    InvalidRemote,
}

/// Capability-like stamp for delayed Files UI work. Both the root generation
/// and complete location identity must still match immediately before an
/// operation is dispatched; otherwise the old path is rejected fail closed.
#[derive(Clone, Debug, PartialEq)]
pub struct FilesIntentContext {
    generation: u64,
    tree_ui_generation: u64,
    operation_generation: u64,
    location: FilesLocationIdentity,
}

/// Keep hand-edited or previously-saved experimental views loadable while
/// ensuring the disabled feature never strands the sidebar on an unreachable
/// tab.
pub fn effective_view(configured: SidebarView, task_sidebar_enabled: bool) -> SidebarView {
    if configured == SidebarView::Tasks && !task_sidebar_enabled {
        SidebarView::Sessions
    } else {
        configured
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DirectoryLoadState {
    NotLoaded,
    Loading,
    Loaded,
    Error(String),
}

/// 文件树节点。目录的 children 只在用户展开它后由后台 worker 填充。
#[derive(Clone, Debug)]
pub struct FileTreeNode {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub children: Vec<FileTreeNode>,
    pub expanded: bool,
    visible_children: usize,
    entries_truncated: bool,
    load_state: DirectoryLoadState,
}

impl FileTreeNode {
    fn directory(path: PathBuf, name: String, expanded: bool) -> Self {
        Self {
            name,
            path,
            is_dir: true,
            children: Vec::new(),
            expanded,
            visible_children: 0,
            entries_truncated: false,
            load_state: DirectoryLoadState::NotLoaded,
        }
    }

    fn from_entry(entry: FileEntry) -> Self {
        if entry.is_dir {
            Self::directory(entry.path, entry.name, false)
        } else {
            Self {
                name: entry.name,
                path: entry.path,
                is_dir: false,
                children: Vec::new(),
                expanded: false,
                visible_children: 0,
                entries_truncated: false,
                load_state: DirectoryLoadState::Loaded,
            }
        }
    }

    pub fn visible_children(&self) -> &[FileTreeNode] {
        &self.children[..self.visible_children.min(self.children.len())]
    }

    pub fn remaining_children(&self) -> usize {
        self.children.len().saturating_sub(self.visible_children)
    }

    pub fn load_error(&self) -> Option<&str> {
        match &self.load_state {
            DirectoryLoadState::Error(error) => Some(error),
            _ => None,
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.load_state, DirectoryLoadState::Loading)
    }

    pub fn entries_truncated(&self) -> bool {
        self.entries_truncated
    }

    fn has_loading_descendant(&self) -> bool {
        self.is_loading()
            || self
                .children
                .iter()
                .any(FileTreeNode::has_loading_descendant)
    }

    fn collect_loading_paths(&self, paths: &mut Vec<PathBuf>) {
        if self.is_loading() {
            paths.push(self.path.clone());
        }
        for child in &self.children {
            child.collect_loading_paths(paths);
        }
    }

    /// 树内过滤视图（纯函数，不动原树）：名称大小写不敏感命中（含子串）的
    /// 节点 + 它们的全部祖先；祖先强制展开、命中目录的子树原样保留。
    /// 什么都不命中返回 None（调用方显示"无匹配"）。只作用于已加载的
    /// children，绝不触发新扫描。
    pub fn filtered(&self, query_lower: &str) -> Option<FileTreeNode> {
        let name_matches = self.name.to_lowercase().contains(query_lower);
        if name_matches {
            // 命中节点：整个子树原样保留（子树内不再过滤）。
            let mut node = self.clone();
            node.visible_children = node.children.len();
            node.entries_truncated = false;
            return Some(node);
        }
        let children: Vec<FileTreeNode> = self
            .children
            .iter()
            .filter_map(|child| child.filtered(query_lower))
            .collect();
        if children.is_empty() {
            return None;
        }
        let mut node = self.clone();
        node.children = children;
        node.visible_children = node.children.len();
        node.entries_truncated = false;
        if node.is_dir {
            // 祖先强制展开（原树的展开状态不受影响，清空过滤即恢复）。
            node.expanded = true;
        }
        Some(node)
    }
}

#[derive(Clone, Debug)]
struct FileEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

#[derive(Clone, Debug)]
struct DirectoryListing {
    entries: Vec<FileEntry>,
    truncated: bool,
}

impl DirectoryListing {
    #[cfg(test)]
    fn complete(entries: Vec<FileEntry>) -> Self {
        Self {
            entries,
            truncated: false,
        }
    }
}

#[derive(Clone, Debug)]
struct ScanRequest {
    generation: u64,
    path: PathBuf,
    backend: ScanBackend,
}

#[derive(Debug)]
struct ScanResult {
    generation: u64,
    path: PathBuf,
    entries: Result<DirectoryListing, String>,
}

/// 目录扫描服务。请求通道无界：用户动作只排队、绝不拒绝（unbounded 的
/// send 不阻塞，UI 线程安全）；真正并发的子进程数仍由 SCAN_WORKERS 个
/// worker 限定。结果通道保持有界，worker 在 UI 停读时自然背压。
#[derive(Debug)]
struct DirectoryScanService {
    request_tx: Sender<ScanRequest>,
    /// Kept by the UI solely so a root-generation change can discard queued
    /// work. Workers receive from clones of this same queue.
    request_rx: Receiver<ScanRequest>,
    result_rx: Receiver<ScanResult>,
}

impl DirectoryScanService {
    fn new(scanner: Arc<ScanFn>) -> io::Result<Self> {
        let (request_tx, request_rx) = crossbeam_channel::unbounded();
        let (result_tx, result_rx) = crossbeam_channel::bounded(SCAN_RESULT_CAPACITY);

        for worker_index in 0..SCAN_WORKERS {
            let requests = request_rx.clone();
            let results = result_tx.clone();
            let scanner = scanner.clone();
            std::thread::Builder::new()
                .name(format!("ember-file-tree-{worker_index}"))
                .spawn(move || scan_worker(requests, results, scanner))?;
        }
        drop(result_tx);

        Ok(Self {
            request_tx,
            request_rx,
            result_rx,
        })
    }

    fn request(&self, request: ScanRequest, supersede_queued: bool) -> Result<(), String> {
        if supersede_queued {
            while self.request_rx.try_recv().is_ok() {}
        }

        // 无界队列的 send 只在 worker 全部退出时失败；永不阻塞 UI 线程。
        self.request_tx
            .send(request)
            .map_err(|_| "directory scan workers stopped".to_string())
    }
}

/// 文件树支持的变更操作（右键菜单/对话框的载荷）。在操作 worker 上阻塞
/// 执行，本机与远程共用同一组 remote_fs 函数。
#[derive(Clone, Debug)]
pub enum FsOpKind {
    CreateDir(PathBuf),
    CreateFile(PathBuf),
    Delete(PathBuf),
    Rename { src: PathBuf, dst: PathBuf },
    Copy { src: PathBuf, dst: PathBuf },
}

impl FsOpKind {
    /// 操作成功后需要重新扫描的目录（去重、无父目录时为空）。
    fn affected_dirs(&self) -> Vec<PathBuf> {
        fn parent(path: &Path) -> Option<PathBuf> {
            path.parent().map(Path::to_path_buf)
        }
        let mut dirs = Vec::new();
        match self {
            FsOpKind::CreateDir(path)
            | FsOpKind::CreateFile(path)
            | FsOpKind::Delete(path)
            | FsOpKind::Copy { dst: path, .. } => dirs.extend(parent(path)),
            FsOpKind::Rename { src, dst } => {
                dirs.extend(parent(src));
                if let Some(dir) = parent(dst) {
                    if !dirs.contains(&dir) {
                        dirs.push(dir);
                    }
                }
            }
        }
        dirs
    }

    /// 状态栏的失败前缀（完整消息由 poll_op_results 拼上具体错误）。
    fn verb(&self) -> &'static str {
        match self {
            FsOpKind::CreateDir(_) => "新建文件夹失败",
            FsOpKind::CreateFile(_) => "新建文件失败",
            FsOpKind::Delete(_) => "删除失败",
            FsOpKind::Rename { .. } => "重命名失败",
            FsOpKind::Copy { .. } => "粘贴失败",
        }
    }

    /// 状态栏的成功消息。
    fn success_message(&self) -> String {
        match self {
            FsOpKind::CreateDir(path) => format!("已创建文件夹 {}", path.display()),
            FsOpKind::CreateFile(path) => format!("已创建文件 {}", path.display()),
            FsOpKind::Delete(path) => format!("已删除 {}", path.display()),
            FsOpKind::Rename { dst, .. } => format!("已重命名为 {}", dst.display()),
            FsOpKind::Copy { dst, .. } => format!("已粘贴到 {}", dst.display()),
        }
    }
}

/// 跨位置粘贴（下载/上传/中转）的载荷。复制成功后才按 cut 删源。
#[derive(Clone, Debug)]
pub struct FsTransfer {
    pub src_endpoint: remote_fs::FsEndpointSnapshot,
    pub src: PathBuf,
    pub src_is_dir: bool,
    pub dst_endpoint: remote_fs::FsEndpointSnapshot,
    /// 目标目录；最终路径 = dst_dir.join(源文件名)。
    pub dst_dir: PathBuf,
    pub cut: bool,
}

impl FsTransfer {
    /// 状态文案用的方向词。
    fn direction(&self) -> &'static str {
        match (
            self.src_endpoint.location.is_remote(),
            self.dst_endpoint.location.is_remote(),
        ) {
            (true, false) => "下载",
            (false, true) => "上传",
            _ => "传输",
        }
    }
}

/// 批量操作（多选粘贴 / 批量删除）：一个 worker 任务逐项执行、跳过失败、
/// 汇总上报。跨位置粘贴逐条复用 remote_fs::transfer（round-2/4 的机制）。
#[derive(Clone, Debug)]
pub enum BatchIntent {
    /// 粘贴：`items`（路径 + 是否目录）从 src_loc 落位到 dst_loc 的 dst_dir。
    /// 同位置逐项 copy/rename（cut）；跨位置逐项 transfer（cut = 成功后删源，
    /// 只删复制成功的源）。
    Paste {
        src_endpoint: Box<remote_fs::FsEndpointSnapshot>,
        dst_endpoint: Box<remote_fs::FsEndpointSnapshot>,
        dst_dir: PathBuf,
        items: Vec<(PathBuf, bool)>,
        cut: bool,
    },
    /// 批量删除（同一位置内）。
    Delete {
        endpoint: Box<remote_fs::FsEndpointSnapshot>,
        items: Vec<PathBuf>,
    },
}

/// 批量操作的账目：成功数 + 逐项失败 + 非致命警告（cut 删源失败等）。
#[derive(Clone, Debug, Default)]
pub struct BatchOutcome {
    pub succeeded: usize,
    /// （条目完整路径， 错误）；显示时取文件名。
    pub failed: Vec<(PathBuf, String)>,
    pub warnings: Vec<String>,
}

impl BatchOutcome {
    /// 状态栏汇总文案。
    pub fn summary(&self, verb: &str, total: usize) -> String {
        fn display(path: &Path) -> String {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        }
        let mut message = if self.failed.is_empty() {
            format!("已{verb} {total} 项")
        } else {
            let first = &self.failed[0];
            format!(
                "{total} 项中 {} 项失败：{}：{}",
                self.failed.len(),
                display(&first.0),
                first.1
            )
        };
        for warning in &self.warnings {
            message.push_str(&format!("；{warning}"));
        }
        message
    }
}

/// 操作 worker 的内部请求种类：除公开的文件操作外，还有切换位置时的
/// 起始目录解析（远程 home 目录不能阻塞 UI 线程）和跨位置传输。
#[derive(Clone, Debug)]
enum OpRequestKind {
    StartDir,
    Fs(FsOpKind),
    Transfer(Box<FsTransfer>),
    Batch(BatchIntent),
}

#[derive(Clone, Debug)]
struct FsOpRequest {
    /// Stable Files-location authority. Unlike `scan_generation`, this only
    /// changes when Local/remote identity changes, so refreshing the visible
    /// tree cannot strand operation bookkeeping.
    authority_generation: u64,
    location: FsLocation,
    overlay: remote_fs::SshExecutionOverlay,
    hosts: Arc<Vec<RemoteHostConfig>>,
    kind: OpRequestKind,
    /// The exact Copy/Cut user intent this paste was dispatched from. Results
    /// may clear/shrink the clipboard only while this token is still current.
    clipboard_intent: Option<u64>,
    /// 传输取消令牌（仅 Transfer 携带）：排队时 worker 跳过执行，
    /// 传输途中由 watchdog 按超时同一路径 kill。
    cancel_token: Option<Arc<AtomicBool>>,
}

#[derive(Debug)]
struct FsOpResult {
    authority_generation: u64,
    kind: OpRequestKind,
    clipboard_intent: Option<u64>,
    /// StartDir/Transfer 成功时携带路径；文件操作成功为 Ok(None)；
    /// Batch 恒为 Ok(None)（成败细节在 batch_outcome）。
    outcome: Result<Option<PathBuf>, String>,
    /// 部分成功提示（跨位置 cut：复制成功但删源失败）。
    warning: Option<String>,
    /// 批量操作的逐项账目。
    batch_outcome: Option<BatchOutcome>,
    /// 取消语义（中性，非错误）。
    cancelled: bool,
    /// 传输令牌（把 Done/Progress 映射回面板上的在途条目）。
    cancel_token: Option<Arc<AtomicBool>>,
}

/// 操作 worker 发给 UI 的消息：进度（有损、通道满就丢）与完成（必达，
/// Box 避免枚举被 FsOpResult 撑大）。
#[derive(Debug)]
enum OpEvent {
    Progress {
        authority_generation: u64,
        token: Arc<AtomicBool>,
        bytes: u64,
    },
    Done(Box<FsOpResult>),
}

/// 在途/排队中的传输条目：面板忙碌行的数据 + 取消令牌的载体。
#[derive(Debug)]
struct TransferTrack {
    token: Arc<AtomicBool>,
    direction: &'static str,
    name: String,
    total: Option<u64>,
    bytes: u64,
}

/// 面板忙碌行数据（第一个在途传输）。
#[derive(Clone, Debug)]
pub struct TransferStatus {
    pub direction: &'static str,
    pub name: String,
    pub bytes: u64,
    pub total: Option<u64>,
}

/// 文件操作 worker：单线程串行执行（操作之间本就有先后语义，比如
/// cut-paste 不能被后续操作抢跑）。请求队列与扫描服务同契约：无界队列
/// 只缓冲用户动作，并发由这唯一一个 worker 限定。
#[derive(Debug)]
struct FsOpService {
    request_tx: Sender<FsOpRequest>,
    result_rx: Receiver<OpEvent>,
}

impl FsOpService {
    fn new() -> io::Result<Self> {
        let (request_tx, request_rx) = crossbeam_channel::unbounded();
        let (result_tx, result_rx) = crossbeam_channel::bounded(OP_RESULT_CAPACITY);
        std::thread::Builder::new()
            .name("ember-fs-op".to_string())
            .spawn(move || op_worker(request_rx, result_tx))?;
        Ok(Self {
            request_tx,
            result_rx,
        })
    }

    fn request(&self, request: FsOpRequest) -> Result<(), String> {
        // 无界队列的 send 只在 worker 退出时失败；永不阻塞 UI 线程。
        self.request_tx
            .send(request)
            .map_err(|_| "file operation worker stopped".to_string())
    }
}

fn op_worker(requests: Receiver<FsOpRequest>, results: Sender<OpEvent>) {
    while let Ok(request) = requests.recv() {
        // 排队期间已被取消（用户点了取消）：按取消收尾，绝不执行。
        if request
            .cancel_token
            .as_ref()
            .is_some_and(|token| token.load(Ordering::SeqCst))
        {
            if results
                .send(OpEvent::Done(Box::new(FsOpResult {
                    authority_generation: request.authority_generation,
                    kind: request.kind,
                    clipboard_intent: request.clipboard_intent,
                    outcome: Ok(None),
                    warning: None,
                    batch_outcome: None,
                    cancelled: true,
                    cancel_token: request.cancel_token,
                })))
                .is_err()
            {
                break;
            }
            continue;
        }
        let execution = execute_op(&request, &results);
        let cancelled = execution
            .as_ref()
            .err()
            .is_some_and(remote_fs::is_cancelled_error);
        if results
            .send(OpEvent::Done(Box::new(FsOpResult {
                authority_generation: request.authority_generation,
                kind: request.kind,
                clipboard_intent: request.clipboard_intent,
                outcome: execution
                    .as_ref()
                    .map(|done| done.path.clone())
                    .map_err(|error| error.to_string()),
                warning: execution
                    .as_ref()
                    .ok()
                    .and_then(|done| done.warning.clone()),
                batch_outcome: execution.as_ref().ok().and_then(|done| done.batch.clone()),
                cancelled,
                cancel_token: request.cancel_token,
            })))
            .is_err()
        {
            break;
        }
    }
}

/// execute_op 的正面结果：可选路径 + 部分成功提示 + 批量账目。
struct OpDone {
    path: Option<PathBuf>,
    warning: Option<String>,
    batch: Option<BatchOutcome>,
}

impl OpDone {
    fn plain(path: Option<PathBuf>) -> Self {
        Self {
            path,
            warning: None,
            batch: None,
        }
    }
}

/// 批量操作：逐项执行、跳过失败（含 AlreadyExists）、汇总账目。
/// 同位置 cut 粘贴 = rename（原子移动）；跨位置 cut = transfer 成功后删源，
/// 只删复制成功的源。
fn execute_batch(hosts: &[RemoteHostConfig], batch: &BatchIntent) -> BatchOutcome {
    let mut outcome = BatchOutcome::default();
    match batch {
        BatchIntent::Paste {
            src_endpoint,
            dst_endpoint,
            dst_dir,
            items,
            cut,
        } => {
            for (src, is_dir) in items {
                let Some(name) = src.file_name() else {
                    outcome
                        .failed
                        .push((src.clone(), "源路径没有文件名".to_string()));
                    continue;
                };
                let dst = dst_dir.join(name);
                let name = name.to_string_lossy().into_owned();
                let same_namespace = remote_fs::same_files_namespace(
                    &src_endpoint.location,
                    &dst_endpoint.location,
                    hosts,
                );
                let same_namespace_overlay = remote_fs::same_namespace_execution_overlay(
                    &src_endpoint.overlay,
                    &dst_endpoint.overlay,
                );
                let result = if same_namespace {
                    if *cut {
                        remote_fs::rename_with_overlay(
                            &dst_endpoint.location,
                            hosts,
                            same_namespace_overlay,
                            src,
                            &dst,
                        )
                    } else {
                        remote_fs::copy_with_overlay(
                            &dst_endpoint.location,
                            hosts,
                            same_namespace_overlay,
                            src,
                            &dst,
                        )
                    }
                } else {
                    remote_fs::transfer_with_overlays(
                        src_endpoint,
                        hosts,
                        src,
                        *is_dir,
                        dst_endpoint,
                        dst_dir,
                        remote_fs::TransferControl::default(),
                    )
                    .map(|_| ())
                };
                match result {
                    Ok(()) => {
                        outcome.succeeded += 1;
                        if *cut && !same_namespace {
                            // 跨位置 cut：复制成功后删源；删源失败记警告、不回滚。
                            if let Err(error) = remote_fs::delete_with_overlay(
                                &src_endpoint.location,
                                hosts,
                                &src_endpoint.overlay,
                                src,
                            ) {
                                outcome
                                    .warnings
                                    .push(format!("{name}：源删除失败（已保留）：{error}"));
                            }
                        }
                    }
                    Err(error) => outcome.failed.push((src.clone(), error.to_string())),
                }
            }
        }
        BatchIntent::Delete { endpoint, items } => {
            for path in items {
                match remote_fs::delete_with_overlay(
                    &endpoint.location,
                    hosts,
                    &endpoint.overlay,
                    path,
                ) {
                    Ok(()) => outcome.succeeded += 1,
                    Err(error) => outcome.failed.push((path.clone(), error.to_string())),
                }
            }
        }
    }
    outcome
}

fn execute_op(request: &FsOpRequest, events: &Sender<OpEvent>) -> io::Result<OpDone> {
    let location = &request.location;
    let hosts = request.hosts.as_slice();
    let overlay = &request.overlay;
    match &request.kind {
        OpRequestKind::StartDir => remote_fs::start_dir_with_overlay(location, hosts, overlay)
            .map(|dir| OpDone::plain(Some(dir))),
        OpRequestKind::Fs(FsOpKind::CreateDir(path)) => {
            remote_fs::create_dir_with_overlay(location, hosts, overlay, path)
                .map(|_| OpDone::plain(None))
        }
        OpRequestKind::Fs(FsOpKind::CreateFile(path)) => {
            remote_fs::create_file_with_overlay(location, hosts, overlay, path)
                .map(|_| OpDone::plain(None))
        }
        OpRequestKind::Fs(FsOpKind::Delete(path)) => {
            remote_fs::delete_with_overlay(location, hosts, overlay, path)
                .map(|_| OpDone::plain(None))
        }
        OpRequestKind::Fs(FsOpKind::Rename { src, dst }) => {
            remote_fs::rename_with_overlay(location, hosts, overlay, src, dst)
                .map(|_| OpDone::plain(None))
        }
        OpRequestKind::Fs(FsOpKind::Copy { src, dst }) => {
            remote_fs::copy_with_overlay(location, hosts, overlay, src, dst)
                .map(|_| OpDone::plain(None))
        }
        OpRequestKind::Transfer(transfer) => {
            // 进度回报：节流后经 OpEvent 通道发给 UI（有损，通道满就丢）。
            let sink = request.cancel_token.as_ref().map(|token| {
                let token = token.clone();
                let authority_generation = request.authority_generation;
                let events = events.clone();
                remote_fs::ProgressSink::new(move |bytes| {
                    let _ = events.try_send(OpEvent::Progress {
                        authority_generation,
                        token: token.clone(),
                        bytes,
                    });
                })
            });
            let control = remote_fs::TransferControl {
                progress: sink,
                cancel: request.cancel_token.clone(),
            };
            let dst = remote_fs::transfer_with_overlays(
                &transfer.src_endpoint,
                hosts,
                &transfer.src,
                transfer.src_is_dir,
                &transfer.dst_endpoint,
                &transfer.dst_dir,
                control,
            )?;
            let mut warning = None;
            if transfer.cut {
                // 跨位置 cut = 复制成功后删源；删源失败按部分成功如实上报，
                // 复制的成果不回滚、剪贴板照样清空（粘贴动作本身已完成）。
                if let Err(error) = remote_fs::delete_with_overlay(
                    &transfer.src_endpoint.location,
                    hosts,
                    &transfer.src_endpoint.overlay,
                    &transfer.src,
                ) {
                    warning = Some(format!("源删除失败（已保留）：{error}"));
                }
            }
            Ok(OpDone {
                path: Some(dst),
                warning,
                batch: None,
            })
        }
        OpRequestKind::Batch(batch) => Ok(OpDone {
            path: None,
            warning: None,
            batch: Some(execute_batch(hosts, batch)),
        }),
    }
}

fn scan_worker(requests: Receiver<ScanRequest>, results: Sender<ScanResult>, scanner: Arc<ScanFn>) {
    while let Ok(request) = requests.recv() {
        let listing = match &request.backend {
            ScanBackend::Local => scanner(&request.path),
            ScanBackend::Remote(endpoint, hosts) => {
                remote_fs::list_dir_with_overlay(
                    &endpoint.location,
                    hosts,
                    &endpoint.overlay,
                    &request.path,
                )
                .map(|entries| {
                    DirectoryListing {
                        entries: entries
                            .into_iter()
                            .map(|entry| FileEntry {
                                name: entry.name,
                                path: entry.path,
                                is_dir: entry.is_dir,
                            })
                            .collect(),
                        // 截断由下方统一处理：remote_fs 会多带一条作为信号。
                        truncated: false,
                    }
                })
            }
        };
        let entries = listing
            .map(|mut listing| {
                if listing.entries.len() > MAX_DIRECTORY_ENTRIES {
                    listing.entries.truncate(MAX_DIRECTORY_ENTRIES);
                    listing.truncated = true;
                }
                listing
            })
            .map_err(|error| error.to_string());
        if results
            .send(ScanResult {
                generation: request.generation,
                path: request.path,
                entries,
            })
            .is_err()
        {
            break;
        }
    }
}

fn scan_dir(dir: &Path) -> io::Result<DirectoryListing> {
    let mut entries = Vec::new();
    let mut truncated = false;
    for (scanned, entry) in std::fs::read_dir(dir)?.enumerate() {
        if scanned == MAX_DIRECTORY_SCAN_ENTRIES {
            truncated = true;
            break;
        }
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();

        // Keep the existing product behavior: dotfiles are deliberately
        // hidden. Unlike the former implementation, visible entries are never
        // silently cut off after item 20.
        if name.starts_with('.') {
            continue;
        }

        if entries.len() == MAX_DIRECTORY_ENTRIES {
            truncated = true;
            break;
        }

        entries.push(FileEntry {
            name,
            path: entry.path(),
            is_dir: entry.file_type()?.is_dir(),
        });
    }

    entries.sort_by_cached_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
    Ok(DirectoryListing { entries, truncated })
}

/// Resolve one index from an old host snapshot into the new active prefix.
/// Equality covers the complete shared profile (including fields not surfaced
/// in Settings). A duplicate is intentionally ambiguous rather than choosing
/// whichever identical row happens to come first.
fn unique_remote_profile_index(
    previous_hosts: &[RemoteHostConfig],
    previous_index: usize,
    hosts: &[RemoteHostConfig],
) -> Option<usize> {
    let profile = previous_hosts.get(previous_index)?;
    let mut matches = hosts
        .iter()
        .take(crate::config::MAX_REMOTE_HOSTS)
        .enumerate()
        .filter_map(|(index, candidate)| (candidate == profile).then_some(index));
    let index = matches.next()?;
    if matches.next().is_some() || crate::config::validate_remote_host_at(hosts, index).is_err() {
        return None;
    }
    Some(index)
}

/// 侧边栏状态
#[derive(Debug)]
pub struct Sidebar {
    pub visible: bool,
    pub width: f32,
    pub current_dir: PathBuf,
    pub root: Option<FileTreeNode>,
    pub selected_path: Option<PathBuf>,
    /// 多选集合（有序，路径 → 是否目录）。空 = 无选中；`selected_path` 是
    /// 主选中行（最后点击），兼作 shift 范围选择的锚点。
    pub selection: BTreeMap<PathBuf, bool>,
    /// 树内过滤：漏斗按钮展开输入行；非空时对已加载的树做客户端过滤
    /// （不触发新扫描；本机与远程一致）。
    pub filter_open: bool,
    pub filter: String,
    /// 当前侧边栏视图。
    pub view: SidebarView,
    /// 文件操作剪贴板（Copy/Cut → Paste；同位置 copy/rename，跨位置传输）。
    pub clipboard: Option<remote_fs::FsClipboard>,
    /// Identity of the current user Copy/Cut action. Payload equality is not
    /// identity: an old slow paste must never clear a later identical action.
    clipboard_intent: Option<u64>,
    next_clipboard_intent: u64,
    scan_generation: u64,
    /// User-visible tree interactions that do not require a rescan (collapse
    /// and pagination) still revoke an automatic SSH probe's commit authority.
    tree_ui_generation: u64,
    /// Monotonic user/file-operation intent. Automatic SSH following captures
    /// this separately from tree scans so a paste/delete started during a slow
    /// probe is never cancelled by the eventual location commit.
    operation_generation: u64,
    /// Advances synchronously for every explicit Files/chrome interaction.
    /// This is distinct from backend generations: a no-op Refresh, cancelled
    /// dialog, or view/location ABA still consumes a same-frame SSH follow.
    user_intent_generation: u64,
    /// Changes only when the Files backend authority changes. Tree refreshes
    /// and cwd/root changes deliberately do not invalidate operation cleanup.
    authority_generation: u64,
    scan_service: Option<DirectoryScanService>,
    worker_error: Option<String>,
    worker_error_reported: bool,
    worker_disconnect_reported: bool,
    /// 文件树当前浏览的位置（本机或某台远程主机）。
    location: FsLocation,
    /// Execution-only connection material for the active endpoint. This is
    /// intentionally outside `FsLocation` and every identity comparison.
    execution_overlay: remote_fs::SshExecutionOverlay,
    /// 远程主机配置快照，随扫描/操作请求携带。
    remote_hosts: Arc<Vec<RemoteHostConfig>>,
    op_service: Option<FsOpService>,
    op_worker_error: Option<String>,
    op_worker_error_reported: bool,
    op_disconnect_reported: bool,
    pending_ops: usize,
    /// 远程位置的起始目录还在 worker 上解析。
    start_dir_pending: bool,
    /// 起始目录解析失败（连不上主机等）时留在面板上的错误。
    location_error: Option<String>,
    /// 在途/排队中的传输：忙碌行数据 + 取消令牌。请求队列无界后不再
    /// 由容量限长；面板只展示第一个在途条目。
    transfer_tracks: Vec<TransferTrack>,
}

impl Sidebar {
    pub fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        Self::with_scanner(current_dir, Arc::new(scan_dir))
    }

    fn with_scanner(current_dir: PathBuf, scanner: Arc<ScanFn>) -> Self {
        let (scan_service, worker_error) = match DirectoryScanService::new(scanner) {
            Ok(service) => (Some(service), None),
            Err(error) => (
                None,
                Some(format!("could not start directory scan workers: {error}")),
            ),
        };
        let (op_service, op_worker_error) = match FsOpService::new() {
            Ok(service) => (Some(service), None),
            Err(error) => (
                None,
                Some(format!(
                    "could not start the file operation worker: {error}"
                )),
            ),
        };
        let root = Some(Self::root_node(&current_dir));

        Self {
            visible: true,
            width: 200.0,
            current_dir,
            root,
            selected_path: None,
            selection: BTreeMap::new(),
            filter_open: false,
            filter: String::new(),
            view: SidebarView::default(),
            clipboard: None,
            clipboard_intent: None,
            next_clipboard_intent: 0,
            scan_generation: 0,
            tree_ui_generation: 0,
            authority_generation: 0,
            operation_generation: 0,
            user_intent_generation: 0,
            scan_service,
            worker_error,
            worker_error_reported: false,
            worker_disconnect_reported: false,
            location: FsLocation::Local,
            execution_overlay: remote_fs::SshExecutionOverlay::default(),
            remote_hosts: Arc::new(Vec::new()),
            op_service,
            op_worker_error,
            op_worker_error_reported: false,
            op_disconnect_reported: false,
            pending_ops: 0,
            start_dir_pending: false,
            location_error: None,
            transfer_tracks: Vec::new(),
        }
    }

    fn root_node(path: &Path) -> FileTreeNode {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("/")
            .to_string();
        FileTreeNode::directory(path.to_path_buf(), name, true)
    }

    pub fn set_current_dir(&mut self, path: PathBuf) -> Option<String> {
        if self.current_dir == path {
            return None;
        }
        self.current_dir = path;
        self.selected_path = None;
        self.selection.clear();
        self.start_root_scan()
    }

    /// 文件树当前浏览的位置（本机或某台远程主机）。
    pub fn location(&self) -> &FsLocation {
        &self.location
    }

    pub fn execution_overlay(&self) -> &remote_fs::SshExecutionOverlay {
        &self.execution_overlay
    }

    /// Update worker/terminal execution for the same Files identity without
    /// resetting its root, loaded rows, selection, or expansion state. Old
    /// in-flight scans keep immutable snapshots but are retired by generation;
    /// directories that were loading are immediately reissued on the new
    /// endpoint. File operations must be idle at the SSH-follow call site.
    pub fn set_execution_overlay(
        &mut self,
        overlay: remote_fs::SshExecutionOverlay,
    ) -> Result<(), String> {
        remote_fs::validate_execution_endpoint(&self.location, &self.remote_hosts, &overlay)
            .map_err(|error| error.to_string())?;
        if self.execution_overlay != overlay {
            let mut loading_paths = Vec::new();
            if let Some(root) = &self.root {
                root.collect_loading_paths(&mut loading_paths);
            }
            self.execution_overlay = overlay;
            self.authority_generation = self.authority_generation.wrapping_add(1);
            self.scan_generation = self.scan_generation.wrapping_add(1);
            for (index, path) in loading_paths.into_iter().enumerate() {
                // The first replacement request discards queued work carrying
                // the old socket. Already-running workers may finish, but the
                // generation gate rejects those results before tree mutation.
                let _ = self.enqueue_scan(path, index == 0);
            }
        }
        Ok(())
    }

    /// Finish a staged same-namespace socket upgrade. Probe failure is
    /// checked before validation or mutation, so the old tree and execution
    /// overlay remain authoritative. A successful probe deliberately ignores
    /// its home path: this is an in-place transport rebind, not navigation.
    pub fn finish_probed_execution_overlay(
        &mut self,
        overlay: remote_fs::SshExecutionOverlay,
        probe: Result<PathBuf, String>,
    ) -> Result<(), String> {
        let _probed_home = probe?;
        self.set_execution_overlay(overlay)
    }

    /// 起始目录解析失败（连不上主机等）时留在面板上的错误。
    pub fn location_error(&self) -> Option<&str> {
        self.location_error.as_deref()
    }

    /// 远程位置的起始目录还在 worker 上解析。
    pub fn is_starting(&self) -> bool {
        self.start_dir_pending
    }

    /// Replace the Files clipboard as a fresh user intent. Even an identical
    /// payload receives a distinct token, preventing slow-operation ABA races.
    pub fn set_clipboard(&mut self, clipboard: remote_fs::FsClipboard) {
        self.next_clipboard_intent = self.next_clipboard_intent.wrapping_add(1);
        if self.next_clipboard_intent == 0 {
            self.next_clipboard_intent = 1;
        }
        self.clipboard_intent = Some(self.next_clipboard_intent);
        self.clipboard = Some(clipboard);
    }

    fn clear_clipboard(&mut self) {
        self.clipboard = None;
        self.clipboard_intent = None;
    }

    fn clipboard_intent_for_clear(&self, clear_on_success: bool) -> Option<u64> {
        clear_on_success.then_some(self.clipboard_intent).flatten()
    }

    fn clipboard_matches(&self, intent: Option<u64>) -> bool {
        intent.is_some() && intent == self.clipboard_intent && self.clipboard.is_some()
    }

    fn clear_clipboard_if_matches(&mut self, intent: Option<u64>) {
        if self.clipboard_matches(intent) {
            self.clear_clipboard();
        }
    }

    /// 是否有还在 worker 上执行的文件操作（含起始目录解析）。
    pub fn has_pending_op(&self) -> bool {
        self.pending_ops > 0
    }

    /// 同步远程主机配置快照。`FsLocation::Remote` 的下标只在一份配置快照
    /// 内有意义；配置增删/重排后，必须按旧 profile 的完整身份唯一重映射。
    /// 找不到或出现重复候选时 fail closed 回 Local，避免旧树上的路径操作被
    /// 静默发给另一台主机。返回值是可直接展示的恢复提示。
    pub fn set_remote_hosts(&mut self, hosts: &[RemoteHostConfig]) -> Option<String> {
        if self.remote_hosts.as_slice() == hosts {
            return None;
        }

        let previous_hosts = Arc::clone(&self.remote_hosts);
        let remap = |index| unique_remote_profile_index(&previous_hosts, index, hosts);
        let remapped_location = match &self.location {
            FsLocation::Local => Some(FsLocation::Local),
            FsLocation::Remote(index) => remap(*index).map(FsLocation::Remote),
            FsLocation::Transient(host) => Some(FsLocation::Transient(host.clone())),
        };
        let remapped_clipboard = self.clipboard.as_ref().and_then(|clipboard| {
            if let FsLocation::Remote(index) = &clipboard.loc {
                Some(remap(*index))
            } else {
                None
            }
        });

        self.remote_hosts = Arc::new(hosts.to_vec());
        // The active tree and clipboard source are independent authorities.
        // Reconcile the clipboard even if the tree itself must fall back.
        let clipboard_notice = match remapped_clipboard {
            Some(Some(index)) => {
                if let Some(clipboard) = &mut self.clipboard {
                    clipboard.loc = FsLocation::Remote(index);
                }
                None
            }
            Some(None) => {
                self.clear_clipboard();
                Some(
                    "远端文件剪贴板来源 profile 已被删除、更改或不再唯一；已清除剪贴板".to_string(),
                )
            }
            None => None,
        };
        match remapped_location {
            Some(location) => self.location = location,
            None => {
                let local_refresh_error = self.set_location(FsLocation::Local);
                let mut message =
                    "所选远端 Files profile 已被删除、更改或不再唯一；已返回 Local".to_string();
                if let Some(clipboard_notice) = clipboard_notice {
                    message.push_str(&format!("；{clipboard_notice}"));
                }
                if let Some(error) = local_refresh_error {
                    message.push_str(&format!("（本地文件树刷新失败：{error}）"));
                }
                return Some(message);
            }
        }

        clipboard_notice
    }

    /// Current Files-header terminal action, if the local tree has a usable
    /// root. Remote actions deliberately omit the independently browsed Files
    /// path, but retain the immutable execution overlay: a saved target may be
    /// using a freshly observed live ControlPath that its profile does not own.
    pub fn files_terminal_target(&self) -> Option<FilesTerminalTarget> {
        match &self.location {
            FsLocation::Local if !self.current_dir.as_os_str().is_empty() => {
                Some(FilesTerminalTarget::Local(self.current_dir.clone()))
            }
            FsLocation::Local => None,
            FsLocation::Remote(index) => Some(FilesTerminalTarget::Remote {
                index: *index,
                overlay: self.execution_overlay.clone(),
            }),
            FsLocation::Transient(host) => Some(FilesTerminalTarget::Transient {
                host: host.clone(),
                overlay: self.execution_overlay.clone(),
            }),
        }
    }

    fn files_location_identity(&self) -> FilesLocationIdentity {
        match &self.location {
            FsLocation::Local => FilesLocationIdentity::Local,
            FsLocation::Remote(index) => self
                .remote_hosts
                .get(*index)
                .cloned()
                .map(FilesLocationIdentity::Remote)
                .unwrap_or(FilesLocationIdentity::InvalidRemote),
            FsLocation::Transient(host) => FilesLocationIdentity::Transient(host.clone()),
        }
    }

    /// Stamp a menu/dialog intent against the exact tree root and location
    /// visible to the user. A uniquely remapped identical remote profile stays
    /// valid across configuration reorder; every root/location change does not.
    pub fn files_intent_context(&self) -> FilesIntentContext {
        FilesIntentContext {
            generation: self.scan_generation,
            tree_ui_generation: self.tree_ui_generation,
            operation_generation: self.operation_generation,
            location: self.files_location_identity(),
        }
    }

    pub fn note_files_user_intent(&mut self) {
        self.user_intent_generation = self.user_intent_generation.wrapping_add(1);
        if self.user_intent_generation == 0 {
            self.user_intent_generation = 1;
        }
    }

    pub fn files_user_intent_generation(&self) -> u64 {
        self.user_intent_generation
    }

    #[cfg(test)]
    #[allow(dead_code)] // The binary-only SSH follow tests consume this; lib tests do not.
    pub(crate) fn test_files_intent_context(generation: u64) -> FilesIntentContext {
        FilesIntentContext {
            generation,
            tree_ui_generation: 0,
            operation_generation: 0,
            location: FilesLocationIdentity::Local,
        }
    }

    /// Revalidate a delayed Files intent immediately before dispatching it.
    pub fn files_intent_is_current(&self, context: &FilesIntentContext) -> bool {
        context.generation == self.scan_generation
            && context.tree_ui_generation == self.tree_ui_generation
            && context.operation_generation == self.operation_generation
            && context.location != FilesLocationIdentity::InvalidRemote
            && context.location == self.files_location_identity()
    }

    /// 切换浏览位置：作废旧扫描与 location authority、清空树，再解析新位置
    /// 的起始目录。本机当场解析；远程经操作 worker 异步解析，期间面板显示
    /// "正在连接"。
    pub fn set_location(&mut self, location: FsLocation) -> Option<String> {
        if self.location == location {
            return None;
        }
        self.location = location;
        self.execution_overlay = remote_fs::SshExecutionOverlay::default();
        self.selected_path = None;
        self.selection.clear();
        self.location_error = None;
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.authority_generation = self.authority_generation.wrapping_add(1);
        self.root = None;
        self.current_dir = PathBuf::new();
        self.start_dir_pending = false;
        // 离开当前位置即放弃在途/排队的传输：置令牌（worker 按取消收尾），
        // 面板条目直接清空；迟到的 Progress/Done 会被 authority 检查丢弃。
        for track in self.transfer_tracks.drain(..) {
            track.token.store(true, Ordering::SeqCst);
        }
        if matches!(self.location, FsLocation::Local) {
            self.current_dir = remote_fs::start_dir(&self.location, &self.remote_hosts)
                .unwrap_or_else(|_| PathBuf::from("/"));
            self.start_root_scan()
        } else {
            self.start_dir_pending = true;
            self.enqueue_op(OpRequestKind::StartDir, None, None)
        }
    }

    /// Commit a process-observed SSH location only after its sidecar `home`
    /// probe and every UI/session authority check succeeded. The location may
    /// be a uniquely matched saved profile or a transient stable identity.
    /// Unlike `set_location`, this installs the already-probed root in one UI
    /// mutation, so a slow or failed connection never blanks the tree that the
    /// user was looking at.
    pub fn commit_probed_location(
        &mut self,
        location: FsLocation,
        overlay: remote_fs::SshExecutionOverlay,
        current_dir: PathBuf,
    ) -> Result<Option<String>, String> {
        if matches!(location, FsLocation::Local) {
            return Err("an observed SSH target cannot commit Local Files".to_string());
        }
        remote_fs::validate_execution_endpoint(&location, &self.remote_hosts, &overlay)
            .map_err(|error| format!("observed SSH endpoint is invalid: {error}"))?;
        if !current_dir.is_absolute() {
            return Err("observed SSH home is not an absolute path".to_string());
        }

        self.location = location;
        self.execution_overlay = overlay;
        self.current_dir = current_dir;
        self.selected_path = None;
        self.selection.clear();
        self.location_error = None;
        self.authority_generation = self.authority_generation.wrapping_add(1);
        self.start_dir_pending = false;
        for track in self.transfer_tracks.drain(..) {
            track.token.store(true, Ordering::SeqCst);
        }
        Ok(self.start_root_scan())
    }

    #[cfg(test)]
    fn commit_probed_transient(
        &mut self,
        host: RemoteHostConfig,
        current_dir: PathBuf,
    ) -> Result<Option<String>, String> {
        self.commit_probed_location(
            FsLocation::Transient(host),
            remote_fs::SshExecutionOverlay::default(),
            current_dir,
        )
    }

    /// UI 入口：请求一个文件变更操作（CreateDir/CreateFile/Delete/Rename/
    /// Copy）。cut-paste 传 clear_clipboard_on_success = true，成功后清空
    /// 剪贴板；失败则保留，方便用户换个目录重试。
    pub fn request_fs_op(
        &mut self,
        kind: FsOpKind,
        clear_clipboard_on_success: bool,
    ) -> Option<String> {
        let intent = self.clipboard_intent_for_clear(clear_clipboard_on_success);
        self.enqueue_op(OpRequestKind::Fs(kind), intent, None)
    }

    /// Same-namespace paste may cross the stable saved/transient presentation
    /// boundary. Execute its direct copy/rename through the live socket carried
    /// by the source clipboard or current destination, without changing the
    /// active tree's stable identity.
    pub fn request_fs_op_with_overlay(
        &mut self,
        kind: FsOpKind,
        clear_clipboard_on_success: bool,
        overlay: remote_fs::SshExecutionOverlay,
    ) -> Option<String> {
        let intent = self.clipboard_intent_for_clear(clear_clipboard_on_success);
        self.enqueue_op_with_overlay(OpRequestKind::Fs(kind), intent, None, overlay)
    }

    /// UI 入口：批量操作（多选粘贴/批量删除）。逐项执行、跳过失败、
    /// 汇总上报；cut 粘贴部分失败时剪贴板收缩为失败项（便于重试）。
    pub fn request_batch(
        &mut self,
        batch: BatchIntent,
        clear_clipboard_on_success: bool,
    ) -> Option<String> {
        let intent = self.clipboard_intent_for_clear(clear_clipboard_on_success);
        self.enqueue_op(OpRequestKind::Batch(batch), intent, None)
    }

    /// UI 入口：跨位置传输（下载/上传/中转）。cut 在复制成功后经 delete 删源，
    /// 删源失败按部分成功上报（warning 进状态栏）。每个传输都带取消令牌和
    /// 面板在途条目（进度/取消按钮的数据来源）。
    pub fn request_transfer(
        &mut self,
        transfer: FsTransfer,
        clear_clipboard_on_success: bool,
    ) -> Option<String> {
        let token = Arc::new(AtomicBool::new(false));
        let name = transfer
            .src
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| transfer.src.display().to_string());
        // 上传（本地文件）可以用 metadata 显示 X / Y MiB；下载/中转只有已传字节。
        let total = if !transfer.src_is_dir
            && matches!(&transfer.src_endpoint.location, FsLocation::Local)
        {
            std::fs::metadata(&transfer.src).ok().map(|meta| meta.len())
        } else {
            None
        };
        let track = TransferTrack {
            token: token.clone(),
            direction: transfer.direction(),
            name,
            total,
            bytes: 0,
        };
        let intent = self.clipboard_intent_for_clear(clear_clipboard_on_success);
        let result = self.enqueue_op(
            OpRequestKind::Transfer(Box::new(transfer)),
            intent,
            Some(token),
        );
        if result.is_none() {
            self.transfer_tracks.push(track);
        }
        result
    }

    /// 面板忙碌行数据（第一个在途传输）。
    pub fn transfer_status(&self) -> Option<TransferStatus> {
        self.transfer_tracks.first().map(|track| TransferStatus {
            direction: track.direction,
            name: track.name.clone(),
            bytes: track.bytes,
            total: track.total,
        })
    }

    /// 取消全部在途/排队传输（通常只有一个）：置令牌。排队中的由 worker
    /// 取请求时按取消收尾；传输途中的由 watchdog 按超时同一路径 kill。
    /// 与完成竞争的取消是 no-op。返回新标记的数量。
    pub fn cancel_transfers(&mut self) -> usize {
        let mut marked = 0;
        for track in &self.transfer_tracks {
            if !track.token.swap(true, Ordering::SeqCst) {
                marked += 1;
            }
        }
        marked
    }

    fn enqueue_op(
        &mut self,
        kind: OpRequestKind,
        clipboard_intent: Option<u64>,
        cancel_token: Option<Arc<AtomicBool>>,
    ) -> Option<String> {
        self.enqueue_op_with_overlay(
            kind,
            clipboard_intent,
            cancel_token,
            self.execution_overlay.clone(),
        )
    }

    fn enqueue_op_with_overlay(
        &mut self,
        kind: OpRequestKind,
        clipboard_intent: Option<u64>,
        cancel_token: Option<Arc<AtomicBool>>,
        overlay: remote_fs::SshExecutionOverlay,
    ) -> Option<String> {
        self.operation_generation = self.operation_generation.wrapping_add(1);
        let Some(service) = &self.op_service else {
            self.start_dir_pending = false;
            return Some(
                self.op_worker_error
                    .clone()
                    .unwrap_or_else(|| "file operation worker is unavailable".to_string()),
            );
        };
        let request = FsOpRequest {
            authority_generation: self.authority_generation,
            location: self.location.clone(),
            overlay,
            hosts: self.remote_hosts.clone(),
            kind,
            clipboard_intent,
            cancel_token,
        };
        match service.request(request) {
            Ok(()) => {
                self.pending_ops += 1;
                None
            }
            Err(error) => {
                self.start_dir_pending = false;
                Some(error)
            }
        }
    }

    /// 收割操作 worker 的消息，不阻塞 UI 线程。Progress 只更新在途条目的
    /// 字节数；StartDir 成功会落地 current_dir 并发起根扫描；文件操作成功
    /// 会安排受影响目录的重新扫描；取消按中性"已取消"上报。返回的字符串
    /// 可直接进状态栏。
    pub fn poll_op_results(&mut self) -> Vec<String> {
        let mut messages = Vec::new();
        let Some(service) = &self.op_service else {
            if self.op_worker_error_reported {
                return messages;
            }
            self.op_worker_error_reported = true;
            return self.op_worker_error.clone().into_iter().collect();
        };
        let receiver = service.result_rx.clone();
        loop {
            match receiver.try_recv() {
                Ok(OpEvent::Progress {
                    authority_generation,
                    token,
                    bytes,
                }) => {
                    // A refresh/root scan keeps the same location authority,
                    // while leaving Local/remote invalidates late progress.
                    if authority_generation != self.authority_generation {
                        continue;
                    }
                    if let Some(track) = self
                        .transfer_tracks
                        .iter_mut()
                        .find(|track| Arc::ptr_eq(&track.token, &token))
                    {
                        track.bytes = bytes;
                    }
                }
                Ok(OpEvent::Done(result)) => {
                    self.pending_ops = self.pending_ops.saturating_sub(1);
                    // Always retire the matching transfer row before any
                    // authority/presentation gate. Refresh bumps the scan
                    // generation, and must not leave a permanent Cancel row.
                    if let Some(token) = &result.cancel_token {
                        self.transfer_tracks
                            .retain(|track| !Arc::ptr_eq(&track.token, token));
                    }
                    // Leaving the Files authority makes all remaining UI and
                    // clipboard effects stale. A mere tree refresh does not.
                    if result.authority_generation != self.authority_generation {
                        continue;
                    }
                    // 取消是中性结果：不清剪贴板（cut 可重试），不刷新目录。
                    if result.cancelled {
                        let direction = match &result.kind {
                            OpRequestKind::Transfer(transfer) => transfer.direction(),
                            _ => "操作",
                        };
                        messages.push(format!("已取消{direction}"));
                        continue;
                    }
                    match result.kind {
                        OpRequestKind::StartDir => {
                            self.start_dir_pending = false;
                            match result.outcome {
                                Ok(Some(dir)) => {
                                    self.current_dir = dir;
                                    if let Some(error) = self.start_root_scan() {
                                        messages.push(format!("文件树读取失败：{error}"));
                                    }
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    let label = self.location.label(&self.remote_hosts);
                                    let local_refresh_error = self.set_location(FsLocation::Local);
                                    let mut message =
                                        format!("无法进入 {label}：{error}；已返回 Local");
                                    if let Some(refresh_error) = local_refresh_error {
                                        message.push_str(&format!(
                                            "（本地文件树刷新失败：{refresh_error}）"
                                        ));
                                    }
                                    messages.push(message);
                                }
                            }
                        }
                        OpRequestKind::Fs(kind) => match result.outcome {
                            Ok(_) => {
                                self.clear_clipboard_if_matches(result.clipboard_intent);
                                for dir in kind.affected_dirs() {
                                    if let Some(error) = self.refresh_loaded_node(&dir) {
                                        messages.push(format!("文件树刷新失败：{error}"));
                                    }
                                }
                                messages.push(kind.success_message());
                            }
                            Err(error) => {
                                messages.push(format!("{}：{error}", kind.verb()));
                            }
                        },
                        OpRequestKind::Transfer(transfer) => match result.outcome {
                            Ok(dst) => {
                                self.clear_clipboard_if_matches(result.clipboard_intent);
                                // 落位目录可能正是当前显示的目录，重新扫描它。
                                if let Some(error) = self.refresh_loaded_node(&transfer.dst_dir) {
                                    messages.push(format!("文件树刷新失败：{error}"));
                                }
                                let mut message = match dst {
                                    Some(dst) => {
                                        format!("已{}到 {}", transfer.direction(), dst.display())
                                    }
                                    None => format!("已{}", transfer.direction()),
                                };
                                if let Some(warning) = result.warning {
                                    message.push_str(&format!("；{warning}"));
                                }
                                messages.push(message);
                            }
                            Err(error) => {
                                messages.push(format!("{}失败：{error}", transfer.direction()));
                            }
                        },
                        OpRequestKind::Batch(batch) => {
                            let Some(outcome) = result.batch_outcome else {
                                continue;
                            };
                            let (verb, total) = match &batch {
                                BatchIntent::Paste { items, .. } => ("粘贴", items.len()),
                                BatchIntent::Delete { items, .. } => ("删除", items.len()),
                            };
                            // 批量粘贴刷新落点目录；批量删除刷新每个条目的父目录。
                            let mut dirs: Vec<PathBuf> = Vec::new();
                            match &batch {
                                BatchIntent::Paste { dst_dir, .. } => dirs.push(dst_dir.clone()),
                                BatchIntent::Delete { items, .. } => {
                                    for path in items {
                                        if let Some(parent) = path.parent() {
                                            let parent = parent.to_path_buf();
                                            if !dirs.contains(&parent) {
                                                dirs.push(parent);
                                            }
                                        }
                                    }
                                }
                            }
                            for dir in dirs {
                                if let Some(error) = self.refresh_loaded_node(&dir) {
                                    messages.push(format!("文件树刷新失败:{error}"));
                                }
                            }
                            // cut 粘贴：全成功清剪贴板；部分失败收缩为失败项（便于重试）。
                            if result.clipboard_intent.is_some() {
                                if outcome.failed.is_empty() {
                                    self.clear_clipboard_if_matches(result.clipboard_intent);
                                } else if self.clipboard_matches(result.clipboard_intent) {
                                    let failed: Vec<&Path> = outcome
                                        .failed
                                        .iter()
                                        .map(|(path, _)| path.as_path())
                                        .collect();
                                    if let Some(clipboard) = &mut self.clipboard {
                                        clipboard
                                            .items
                                            .retain(|item| failed.contains(&item.path.as_path()));
                                    }
                                }
                            }
                            messages.push(outcome.summary(verb, total));
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.op_disconnect_reported && self.pending_ops > 0 {
                        self.op_disconnect_reported = true;
                        self.pending_ops = 0;
                        self.start_dir_pending = false;
                        self.transfer_tracks.clear();
                        messages.push("file operation worker stopped".to_string());
                    }
                    break;
                }
            }
        }
        messages
    }

    /// 重新扫描一个已加载的节点（文件操作成功后调用）。尚未加载过的目录
    /// 不主动扫描：折叠时内容不可见，下次展开自然会重新拉取。
    pub fn refresh_loaded_node(&mut self, path: &Path) -> Option<String> {
        let loaded = self
            .find_node_mut(path)
            .is_some_and(|node| matches!(node.load_state, DirectoryLoadState::Loaded));
        if !loaded {
            return None;
        }
        if let Some(node) = self.find_node_mut(path) {
            node.load_state = DirectoryLoadState::Loading;
        }
        self.enqueue_scan(path.to_path_buf(), false)
    }

    // ---- 多选 ----

    /// 单选：选中集变为仅此行，该行成为范围选择的锚点。
    pub fn select_single(&mut self, path: &Path, is_dir: bool) {
        self.selected_path = Some(path.to_path_buf());
        self.selection = BTreeMap::from([(path.to_path_buf(), is_dir)]);
    }

    /// ctrl+点击：切换该行的选中状态，锚点跟到该行。
    pub fn select_toggle(&mut self, path: &Path, is_dir: bool) {
        self.selected_path = Some(path.to_path_buf());
        if self.selection.contains_key(path) {
            self.selection.remove(path);
        } else {
            self.selection.insert(path.to_path_buf(), is_dir);
        }
    }

    /// shift+点击：锚点（上次点击行）到目标行在可见行序中的闭区间。
    /// 锚点不动，连续 shift+点击以首次点击为基准扩展；锚点不在可见行序里
    /// （树变了）就退化为单选。
    pub fn select_range(
        &mut self,
        row_order: &[(PathBuf, bool)],
        target: &Path,
        target_is_dir: bool,
    ) {
        let Some(anchor) = self
            .selected_path
            .clone()
            .filter(|anchor| row_order.iter().any(|(path, _)| path == anchor))
        else {
            self.select_single(target, target_is_dir);
            return;
        };
        let (Some(lo), Some(hi)) = (
            row_order.iter().position(|(path, _)| *path == anchor),
            row_order.iter().position(|(path, _)| path == target),
        ) else {
            self.select_single(target, target_is_dir);
            return;
        };
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        // 锚点保持不动：选中集换成闭区间，selected_path 仍是锚点。
        self.selection = row_order[lo..=hi].iter().cloned().collect();
    }

    /// 右键菜单的目标集：点在选中集内 → 整个选中集；否则选中集变为该行。
    /// 返回（新选中集，菜单目标列表）。
    pub fn resolve_menu_targets(
        selection: &BTreeMap<PathBuf, bool>,
        clicked: &Path,
        clicked_is_dir: bool,
    ) -> (BTreeMap<PathBuf, bool>, Vec<(PathBuf, bool)>) {
        if selection.contains_key(clicked) {
            (
                selection.clone(),
                selection
                    .iter()
                    .map(|(path, dir)| (path.clone(), *dir))
                    .collect(),
            )
        } else {
            let single = BTreeMap::from([(clicked.to_path_buf(), clicked_is_dir)]);
            let targets = single
                .iter()
                .map(|(path, dir)| (path.clone(), *dir))
                .collect();
            (single, targets)
        }
    }

    /// 切换节点展开状态，并只在第一次展开（或错误后重试）时请求扫描。
    pub fn toggle_node(&mut self, path: &Path) -> Option<String> {
        let mut should_scan = false;
        let mut interacted = false;
        if let Some(node) = self.find_node_mut(path) {
            interacted = true;
            node.expanded = !node.expanded;
            if node.expanded
                && matches!(
                    node.load_state,
                    DirectoryLoadState::NotLoaded | DirectoryLoadState::Error(_)
                )
            {
                node.children.clear();
                node.visible_children = 0;
                node.entries_truncated = false;
                node.load_state = DirectoryLoadState::Loading;
                should_scan = true;
            }
        }
        if interacted {
            self.tree_ui_generation = self.tree_ui_generation.wrapping_add(1);
        }

        if should_scan {
            self.enqueue_scan(path.to_path_buf(), false)
        } else {
            None
        }
    }

    pub fn show_more(&mut self, path: &Path) {
        let mut interacted = false;
        if let Some(node) = self.find_node_mut(path) {
            interacted = true;
            node.visible_children = node
                .visible_children
                .saturating_add(DIRECTORY_PAGE_SIZE)
                .min(node.children.len());
        }
        if interacted {
            self.tree_ui_generation = self.tree_ui_generation.wrapping_add(1);
        }
    }

    /// 刷新当前目录。旧 generation 的排队请求会被移除；已经在 worker
    /// 中执行的请求允许自然结束，但结果不会写回新树。
    pub fn refresh(&mut self) -> Option<String> {
        self.start_root_scan()
    }

    fn start_root_scan(&mut self) -> Option<String> {
        // 远程位置的起始目录还在解析时 current_dir 为空，等 StartDir 结果
        // 落地后再扫，否则会把空路径发给探针。
        if self.current_dir.as_os_str().is_empty() {
            return None;
        }
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.root = Some(Self::root_node(&self.current_dir));
        if let Some(root) = &mut self.root {
            root.load_state = DirectoryLoadState::Loading;
        }
        self.enqueue_scan(self.current_dir.clone(), true)
    }

    fn enqueue_scan(&mut self, path: PathBuf, supersede_queued: bool) -> Option<String> {
        let backend = match &self.location {
            FsLocation::Local => ScanBackend::Local,
            location => ScanBackend::Remote(
                Box::new(remote_fs::FsEndpointSnapshot::new(
                    location.clone(),
                    self.execution_overlay.clone(),
                )),
                self.remote_hosts.clone(),
            ),
        };
        let result = match &self.scan_service {
            Some(service) => service.request(
                ScanRequest {
                    generation: self.scan_generation,
                    path: path.clone(),
                    backend,
                },
                supersede_queued,
            ),
            None => Err(self
                .worker_error
                .clone()
                .unwrap_or_else(|| "directory scan workers are unavailable".to_string())),
        };

        match result {
            Ok(()) => None,
            Err(error) => {
                self.set_node_error(&path, error.clone());
                Some(format!("{}: {error}", path.display()))
            }
        }
    }

    /// Drain completed worker messages without blocking the UI thread. Returned
    /// strings are suitable for the application's status bar; the same error
    /// also remains attached to the affected tree node.
    pub fn poll_scan_results(&mut self) -> Vec<String> {
        let Some(service) = &self.scan_service else {
            if self.worker_error_reported {
                return Vec::new();
            }
            self.worker_error_reported = true;
            return self.worker_error.clone().into_iter().collect();
        };
        let receiver = service.result_rx.clone();
        let mut errors = Vec::new();

        loop {
            match receiver.try_recv() {
                Ok(result) => {
                    if result.generation != self.scan_generation {
                        continue;
                    }
                    let Some(node) = self.find_node_mut(&result.path) else {
                        continue;
                    };
                    match result.entries {
                        Ok(listing) => {
                            node.children = listing
                                .entries
                                .into_iter()
                                .map(FileTreeNode::from_entry)
                                .collect();
                            node.visible_children = node.children.len().min(DIRECTORY_PAGE_SIZE);
                            node.entries_truncated = listing.truncated;
                            node.load_state = DirectoryLoadState::Loaded;
                        }
                        Err(error) => {
                            node.children.clear();
                            node.visible_children = 0;
                            node.entries_truncated = false;
                            node.load_state = DirectoryLoadState::Error(error.clone());
                            errors.push(format!("{}: {error}", result.path.display()));
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.worker_disconnect_reported && self.has_pending_scan() {
                        self.worker_disconnect_reported = true;
                        let error = "directory scan workers stopped".to_string();
                        self.mark_loading_nodes_failed(&error);
                        errors.push(error);
                    }
                    break;
                }
            }
        }
        errors
    }

    pub fn has_pending_scan(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(FileTreeNode::has_loading_descendant)
    }

    fn find_node_mut(&mut self, path: &Path) -> Option<&mut FileTreeNode> {
        fn find<'a>(node: &'a mut FileTreeNode, path: &Path) -> Option<&'a mut FileTreeNode> {
            if node.path == path {
                return Some(node);
            }
            node.children.iter_mut().find_map(|child| find(child, path))
        }

        self.root.as_mut().and_then(|root| find(root, path))
    }

    fn set_node_error(&mut self, path: &Path, error: String) {
        if let Some(node) = self.find_node_mut(path) {
            node.load_state = DirectoryLoadState::Error(error);
        }
    }

    fn mark_loading_nodes_failed(&mut self, error: &str) {
        fn mark(node: &mut FileTreeNode, error: &str) {
            if node.is_loading() {
                node.load_state = DirectoryLoadState::Error(error.to_string());
            }
            for child in &mut node.children {
                mark(child, error);
            }
        }
        if let Some(root) = &mut self.root {
            mark(root, error);
        }
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

// ---- 拖放导入（OS 文件管理器 → 文件树） ----

/// 一次拖放最多接受的条目数。
pub const MAX_DROP_ITEMS: usize = 256;
/// 预统计目录大小的递归深度上限。
const MAX_DROP_WALK_DEPTH: usize = 64;

/// 拖放导入计划里的单个条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropPlanItem {
    /// 本机位置：递归复制（round-1 本地复制器）。
    Copy {
        src: PathBuf,
        dst: PathBuf,
        is_dir: bool,
    },
    /// 远程位置：走传输机制上传（进度/取消/状态同粘贴）。
    Upload {
        src: PathBuf,
        dst_dir: PathBuf,
        is_dir: bool,
    },
}

/// 拖放导入计划（纯函数产物，可测）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DropPlan {
    /// 通过校验、可分派的条目。
    pub items: Vec<DropPlanItem>,
    /// 因目标已存在而被拒绝的源路径（仅 Local 位置能就地预检；
    /// Remote 由 worker 的 17/AlreadyExists 逐条兜底）。
    pub refused_existing: Vec<PathBuf>,
    /// 全部条目的总字节数（有界预统计）。
    pub total_bytes: u64,
}

/// 预统计一个拖放路径的大小：文件取 metadata 长度；目录递归求和，
/// 深度超过 64 的部分与符号链接都计 0（symlink_metadata 不跟随链接，
/// 链接本身由 tar/复制器按链接传输，体量可忽略）。预算耗尽提前收尾。
/// 读不到的条目计 0（lenient：权限问题交给真正的传输去报错）。
fn measure_dropped_path(path: &Path, depth: usize, budget: &mut u64) -> u64 {
    if depth > MAX_DROP_WALK_DEPTH || *budget == 0 {
        return 0;
    }
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return 0;
        };
        let mut sum = 0u64;
        for entry in entries.flatten() {
            if *budget == 0 {
                break;
            }
            sum += measure_dropped_path(&entry.path(), depth + 1, budget);
        }
        sum
    } else if metadata.file_type().is_symlink() {
        0
    } else {
        let size = metadata.len().min(*budget);
        *budget -= size;
        size
    }
}

/// 拖放导入计划：dropped（本机绝对路径）+ 落点目录 + 当前位置 → 每项
/// Copy/Upload 计划。超过条目数/总字节帽整批拒绝（Err 文案直接进状态栏）。
pub fn plan_drop(
    dropped: &[PathBuf],
    target_dir: &Path,
    location: &FsLocation,
) -> Result<DropPlan, String> {
    plan_drop_with_limits(
        dropped,
        target_dir,
        location,
        MAX_DROP_ITEMS,
        remote_fs::MAX_TRANSFER_BYTES,
    )
}

fn plan_drop_with_limits(
    dropped: &[PathBuf],
    target_dir: &Path,
    location: &FsLocation,
    max_items: usize,
    max_bytes: u64,
) -> Result<DropPlan, String> {
    // 只要本机绝对路径、且得有文件名（"/" 这类没法按名落位）。
    let candidates: Vec<&PathBuf> = dropped
        .iter()
        .filter(|path| path.is_absolute() && path.file_name().is_some())
        .collect();
    if candidates.is_empty() {
        return Err("拖放内容里没有可导入的本地路径".to_string());
    }
    if candidates.len() > max_items {
        return Err(format!(
            "拖放条目过多（{} > {max_items}），已整批拒绝",
            candidates.len()
        ));
    }
    // 预算是帽 +1：预算内按真实大小记账，这样一旦真实总量越帽，
    // 累计值必然超过 max_bytes（预算钳制只用于提前停止遍历，不掩盖越帽）。
    let mut budget = max_bytes.saturating_add(1);
    let mut plan = DropPlan::default();
    for src in candidates {
        plan.total_bytes += measure_dropped_path(src, 0, &mut budget);
        if plan.total_bytes > max_bytes {
            return Err(format!(
                "拖放内容总计超过 {}，已整批拒绝",
                remote_fs::format_bytes(max_bytes)
            ));
        }
        let name = src.file_name().expect("filtered above");
        // 与预统计同一语义：符号链接不跟随（is_dir 对链接为假）。
        let is_dir = std::fs::symlink_metadata(src)
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false);
        match location {
            FsLocation::Local => {
                let dst = target_dir.join(name);
                if std::fs::symlink_metadata(&dst).is_ok() {
                    plan.refused_existing.push(src.clone());
                    continue;
                }
                plan.items.push(DropPlanItem::Copy {
                    src: src.clone(),
                    dst,
                    is_dir,
                });
            }
            FsLocation::Remote(_) | FsLocation::Transient(_) => {
                plan.items.push(DropPlanItem::Upload {
                    src: src.clone(),
                    dst_dir: target_dir.to_path_buf(),
                    is_dir,
                });
            }
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    #[test]
    fn tasks_view_round_trips_and_falls_back_when_feature_is_disabled() {
        let serialized = serde_json::to_string(&SidebarView::Tasks).unwrap();
        assert_eq!(serialized, "\"tasks\"");
        assert_eq!(
            serde_json::from_str::<SidebarView>(&serialized).unwrap(),
            SidebarView::Tasks
        );
        assert_eq!(
            effective_view(SidebarView::Tasks, false),
            SidebarView::Sessions
        );
        assert_eq!(effective_view(SidebarView::Tasks, true), SidebarView::Tasks);
    }

    #[test]
    fn files_terminal_target_uses_the_local_root_but_only_the_remote_profile() {
        let mut sidebar = Sidebar::with_scanner(
            PathBuf::from("/work/tree-root"),
            Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>,
        );
        assert_eq!(
            sidebar.files_terminal_target(),
            Some(FilesTerminalTarget::Local(PathBuf::from("/work/tree-root")))
        );

        sidebar.location = FsLocation::Remote(7);
        sidebar.current_dir = PathBuf::from("/independent/remote/browse/path");
        assert_eq!(
            sidebar.files_terminal_target(),
            Some(FilesTerminalTarget::Remote {
                index: 7,
                overlay: remote_fs::SshExecutionOverlay::default(),
            }),
            "a remote terminal must use its profile default, never the Files browse path"
        );

        let live_overlay = remote_fs::SshExecutionOverlay::from_control_path(Some(
            "/run/user/1000/ember/live-%C".to_string(),
        ));
        sidebar.execution_overlay = live_overlay.clone();
        assert_eq!(
            sidebar.files_terminal_target(),
            Some(FilesTerminalTarget::Remote {
                index: 7,
                overlay: live_overlay,
            }),
            "a saved Files target must carry its live execution socket into the terminal action"
        );

        let transient = crate::config::default_remote_hosts()[0].clone();
        sidebar.location = FsLocation::Transient(transient.clone());
        sidebar.execution_overlay = remote_fs::SshExecutionOverlay::default();
        assert_eq!(
            sidebar.files_terminal_target(),
            Some(FilesTerminalTarget::Transient {
                host: transient,
                overlay: remote_fs::SshExecutionOverlay::default(),
            })
        );
    }

    #[test]
    fn explicit_follow_intent_epoch_does_not_invalidate_delayed_file_menu_context() {
        let mut sidebar = Sidebar::with_scanner(
            PathBuf::from("/work/tree-root"),
            Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>,
        );
        let context = sidebar.files_intent_context();
        let before = sidebar.files_user_intent_generation();
        sidebar.note_files_user_intent();
        assert_ne!(sidebar.files_user_intent_generation(), before);
        assert!(
            sidebar.files_intent_is_current(&context),
            "SSH follow dedupe must not make a just-opened file dialog stale"
        );
    }

    #[test]
    fn remote_profile_identity_remap_requires_one_active_exact_match() {
        let profiles = crate::config::default_remote_hosts();
        let first = profiles[0].clone();
        let second = profiles[1].clone();
        assert_eq!(
            unique_remote_profile_index(&profiles, 1, &[second.clone(), first.clone()]),
            Some(0)
        );
        assert_eq!(unique_remote_profile_index(&profiles, 0, &[second]), None);
        assert_eq!(
            unique_remote_profile_index(&profiles, 0, &[first.clone(), first]),
            None,
            "duplicate full profiles are ambiguous even when one retains the old index"
        );
    }

    #[test]
    fn remote_profile_reorder_remaps_location_and_clipboard_without_resetting_tree() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/virtual/local"), scanner);
        let profiles = crate::config::default_remote_hosts();
        assert!(sidebar.set_remote_hosts(&profiles).is_none());
        sidebar.location = FsLocation::Remote(1);
        sidebar.current_dir = PathBuf::from("/remote/home");
        sidebar.select_single(Path::new("/remote/home/kept.txt"), false);
        sidebar.set_clipboard(remote_fs::FsClipboard {
            loc: FsLocation::Remote(0),
            overlay: remote_fs::SshExecutionOverlay::default(),
            items: vec![remote_fs::FsClipboardItem {
                path: PathBuf::from("/remote/source.txt"),
                is_dir: false,
            }],
            cut: false,
        });
        let generation = sidebar.scan_generation;
        let clipboard_intent = sidebar.clipboard_intent;
        let delayed_intent = sidebar.files_intent_context();

        let reordered = vec![profiles[1].clone(), profiles[0].clone()];
        assert!(sidebar.set_remote_hosts(&reordered).is_none());
        assert_eq!(sidebar.location(), &FsLocation::Remote(0));
        assert_eq!(
            sidebar.clipboard.as_ref().map(|clipboard| &clipboard.loc),
            Some(&FsLocation::Remote(1))
        );
        assert!(sidebar
            .selection
            .contains_key(Path::new("/remote/home/kept.txt")));
        assert_eq!(sidebar.scan_generation, generation);
        assert_eq!(sidebar.clipboard_intent, clipboard_intent);
        assert_eq!(sidebar.current_dir, PathBuf::from("/remote/home"));
        assert!(
            sidebar.files_intent_is_current(&delayed_intent),
            "a unique full-profile reorder must not invalidate a dialog for the same host"
        );
    }

    #[test]
    fn config_replacement_does_not_retarget_transient_tree_or_clipboard() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/virtual/local"), scanner);
        let profiles = crate::config::default_remote_hosts();
        assert!(sidebar.set_remote_hosts(&profiles).is_none());
        let tree_profile = profiles[0].clone();
        let mut clipboard_profile = profiles[0].clone();
        clipboard_profile.name = "temporary clipboard host".to_string();
        clipboard_profile.host = "clipboard.example.test".to_string();
        let clipboard_overlay = remote_fs::SshExecutionOverlay::from_control_path(Some(
            "/run/user/1000/anvil/clipboard-%C".to_string(),
        ));
        sidebar.location = FsLocation::Transient(tree_profile.clone());
        sidebar.current_dir = PathBuf::from("/transient/home");
        sidebar.set_clipboard(remote_fs::FsClipboard {
            loc: FsLocation::Transient(clipboard_profile.clone()),
            overlay: clipboard_overlay.clone(),
            items: vec![remote_fs::FsClipboardItem {
                path: PathBuf::from("/other/source.txt"),
                is_dir: false,
            }],
            cut: false,
        });
        let generation = sidebar.scan_generation;
        let intent = sidebar.files_intent_context();
        let clipboard_intent = sidebar.clipboard_intent;

        let mut replacement = profiles[0].clone();
        replacement.host = "replacement.example.test".to_string();
        assert!(sidebar.set_remote_hosts(&[replacement]).is_none());
        assert_eq!(
            sidebar.location(),
            &FsLocation::Transient(tree_profile),
            "a transient tree never aliases a configured row"
        );
        let clipboard = sidebar.clipboard.as_ref().unwrap();
        assert_eq!(
            clipboard.loc,
            FsLocation::Transient(clipboard_profile),
            "the independently frozen clipboard source also survives config replacement"
        );
        assert_eq!(clipboard.overlay, clipboard_overlay);
        assert_eq!(sidebar.clipboard_intent, clipboard_intent);
        assert_eq!(sidebar.scan_generation, generation);
        assert_eq!(sidebar.current_dir, PathBuf::from("/transient/home"));
        assert!(sidebar.files_intent_is_current(&intent));
    }

    #[test]
    fn same_target_socket_upgrade_preserves_tree_and_reissues_only_loading_rows() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/virtual/local"), scanner);
        let profiles = crate::config::default_remote_hosts();
        assert!(sidebar.set_remote_hosts(&profiles).is_none());
        sidebar.location = FsLocation::Remote(0);
        sidebar.current_dir = PathBuf::from("/remote/root");

        let mut root = Sidebar::root_node(&sidebar.current_dir);
        root.load_state = DirectoryLoadState::Loaded;
        let mut expanded = FileTreeNode::directory(
            PathBuf::from("/remote/root/expanded"),
            "expanded".to_string(),
            true,
        );
        expanded.load_state = DirectoryLoadState::Loading;
        expanded.children.push(FileTreeNode::from_entry(FileEntry {
            name: "already-loaded.txt".to_string(),
            path: PathBuf::from("/remote/root/expanded/already-loaded.txt"),
            is_dir: false,
        }));
        expanded.visible_children = 1;
        root.children.push(expanded);
        root.visible_children = 1;
        sidebar.root = Some(root);

        // Replace the production scan workers with a deterministic channel so
        // both the old-generation rejection and new-overlay request are exact.
        let (request_tx, request_rx) = crossbeam_channel::bounded(4);
        let (result_tx, result_rx) = crossbeam_channel::bounded(4);
        sidebar.scan_service = Some(DirectoryScanService {
            request_tx,
            request_rx: request_rx.clone(),
            result_rx,
        });
        let old_generation = sidebar.scan_generation;
        let old_root = sidebar.current_dir.clone();
        let live_overlay = remote_fs::SshExecutionOverlay::from_control_path(Some(
            "/run/user/1000/ember/live-%C".to_string(),
        ));

        sidebar
            .finish_probed_execution_overlay(
                live_overlay.clone(),
                Ok(PathBuf::from("/different/probed/home")),
            )
            .unwrap();
        assert_eq!(sidebar.current_dir, old_root);
        assert_eq!(sidebar.location(), &FsLocation::Remote(0));
        assert_ne!(sidebar.scan_generation, old_generation);
        let expanded = &sidebar.root.as_ref().unwrap().children[0];
        assert!(expanded.expanded);
        assert_eq!(expanded.children[0].name, "already-loaded.txt");

        let replacement = request_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("loading directory is reissued on the new socket");
        assert_eq!(replacement.generation, sidebar.scan_generation);
        assert_eq!(replacement.path, PathBuf::from("/remote/root/expanded"));
        match &replacement.backend {
            ScanBackend::Remote(endpoint, _) => assert_eq!(endpoint.overlay, live_overlay),
            ScanBackend::Local => panic!("remote loading row must stay remote"),
        }

        result_tx
            .send(ScanResult {
                generation: old_generation,
                path: replacement.path.clone(),
                entries: Ok(DirectoryListing::complete(vec![FileEntry {
                    name: "stale.txt".to_string(),
                    path: replacement.path.join("stale.txt"),
                    is_dir: false,
                }])),
            })
            .unwrap();
        assert!(sidebar.poll_scan_results().is_empty());
        assert_eq!(
            sidebar.root.as_ref().unwrap().children[0].children[0].name,
            "already-loaded.txt",
            "the old socket may not overwrite loaded rows"
        );

        result_tx
            .send(ScanResult {
                generation: replacement.generation,
                path: replacement.path.clone(),
                entries: Ok(DirectoryListing::complete(vec![FileEntry {
                    name: "fresh.txt".to_string(),
                    path: replacement.path.join("fresh.txt"),
                    is_dir: false,
                }])),
            })
            .unwrap();
        assert!(sidebar.poll_scan_results().is_empty());
        let expanded = &sidebar.root.as_ref().unwrap().children[0];
        assert!(expanded.expanded);
        assert_eq!(expanded.children[0].name, "fresh.txt");
    }

    #[test]
    fn failed_same_target_socket_probe_keeps_old_overlay_and_tree_untouched() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/virtual/local"), scanner);
        let profiles = crate::config::default_remote_hosts();
        assert!(sidebar.set_remote_hosts(&profiles).is_none());
        sidebar.location = FsLocation::Remote(0);
        sidebar.current_dir = PathBuf::from("/remote/kept-root");
        let old_overlay = remote_fs::SshExecutionOverlay::from_control_path(Some(
            "/run/user/1000/ember/old-%C".to_string(),
        ));
        sidebar.execution_overlay = old_overlay.clone();
        let mut root = Sidebar::root_node(&sidebar.current_dir);
        root.load_state = DirectoryLoadState::Loaded;
        root.children.push(FileTreeNode::from_entry(FileEntry {
            name: "kept.txt".to_string(),
            path: PathBuf::from("/remote/kept-root/kept.txt"),
            is_dir: false,
        }));
        root.visible_children = 1;
        sidebar.root = Some(root);
        let scan_generation = sidebar.scan_generation;
        let authority_generation = sidebar.authority_generation;

        let new_overlay = remote_fs::SshExecutionOverlay::from_control_path(Some(
            "/run/user/1000/ember/unverified-new-%C".to_string(),
        ));
        assert!(sidebar
            .finish_probed_execution_overlay(
                new_overlay,
                Err("new socket rejected the BatchMode probe".to_string()),
            )
            .is_err());

        assert_eq!(sidebar.execution_overlay(), &old_overlay);
        assert_eq!(sidebar.current_dir, PathBuf::from("/remote/kept-root"));
        assert_eq!(sidebar.scan_generation, scan_generation);
        assert_eq!(sidebar.authority_generation, authority_generation);
        let root = sidebar.root.as_ref().unwrap();
        assert_eq!(root.children[0].name, "kept.txt");
        assert_eq!(root.load_state, DirectoryLoadState::Loaded);
    }

    #[test]
    fn rejected_transient_commit_preserves_the_existing_tree() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/existing/root"), scanner);
        let location = sidebar.location().clone();
        let generation = sidebar.scan_generation;
        let authority = sidebar.authority_generation;

        let profile = crate::config::default_remote_hosts()[0].clone();
        assert!(sidebar
            .commit_probed_transient(profile, PathBuf::from("relative/home"))
            .is_err());
        assert_eq!(sidebar.location(), &location);
        assert_eq!(sidebar.current_dir, PathBuf::from("/existing/root"));
        assert_eq!(sidebar.scan_generation, generation);
        assert_eq!(sidebar.authority_generation, authority);
    }

    #[test]
    fn changed_remote_profile_falls_back_local_and_invalidates_remote_state() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/virtual/local"), scanner);
        let profiles = crate::config::default_remote_hosts();
        assert!(sidebar.set_remote_hosts(&profiles).is_none());
        sidebar.location = FsLocation::Remote(0);
        sidebar.current_dir = PathBuf::from("/remote/home");
        sidebar.root = Some(Sidebar::root_node(&sidebar.current_dir));
        sidebar.select_single(Path::new("/remote/home/stale.txt"), false);
        sidebar.set_clipboard(remote_fs::FsClipboard {
            loc: FsLocation::Remote(0),
            overlay: remote_fs::SshExecutionOverlay::default(),
            items: vec![remote_fs::FsClipboardItem {
                path: PathBuf::from("/remote/home/stale.txt"),
                is_dir: false,
            }],
            cut: true,
        });
        let token = Arc::new(AtomicBool::new(false));
        sidebar.transfer_tracks.push(TransferTrack {
            token: Arc::clone(&token),
            direction: "下载",
            name: "stale.txt".to_string(),
            total: None,
            bytes: 0,
        });
        let generation = sidebar.scan_generation;
        let delayed_intent = sidebar.files_intent_context();
        let mut changed = profiles;
        changed[0].host = "replacement.example.test".to_string();

        let notice = sidebar
            .set_remote_hosts(&changed)
            .expect("unsafe index reuse must be visible");
        assert!(notice.contains("已返回 Local"));
        assert_eq!(sidebar.location(), &FsLocation::Local);
        assert!(sidebar.selection.is_empty());
        assert!(sidebar.selected_path.is_none());
        assert!(sidebar.clipboard.is_none());
        assert!(sidebar.transfer_tracks.is_empty());
        assert!(token.load(Ordering::SeqCst));
        assert_ne!(sidebar.scan_generation, generation);
        assert_eq!(sidebar.current_dir, std::env::current_dir().unwrap());
        assert!(
            !sidebar.files_intent_is_current(&delayed_intent),
            "a dialog from the replaced remote must not dispatch against Local"
        );
    }

    #[test]
    fn removed_active_remote_preserves_an_independently_valid_clipboard_source() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/virtual/local"), scanner);
        let profiles = crate::config::default_remote_hosts();
        assert!(sidebar.set_remote_hosts(&profiles).is_none());
        sidebar.location = FsLocation::Remote(0);
        sidebar.current_dir = PathBuf::from("/remote-a/home");
        sidebar.set_clipboard(remote_fs::FsClipboard {
            loc: FsLocation::Remote(1),
            overlay: remote_fs::SshExecutionOverlay::default(),
            items: vec![remote_fs::FsClipboardItem {
                path: PathBuf::from("/remote-b/kept.txt"),
                is_dir: false,
            }],
            cut: true,
        });
        let clipboard_intent = sidebar.clipboard_intent;

        // A disappears while B moves from index 1 to 0. The tree authority A
        // must fail closed, but B remains independently provable.
        let notice = sidebar
            .set_remote_hosts(&[profiles[1].clone()])
            .expect("removed active tree authority must be visible");

        assert!(notice.contains("已返回 Local"));
        assert_eq!(sidebar.location(), &FsLocation::Local);
        assert_eq!(sidebar.clipboard_intent, clipboard_intent);
        let clipboard = sidebar
            .clipboard
            .as_ref()
            .expect("independently valid B clipboard survives A fallback");
        assert_eq!(clipboard.loc, FsLocation::Remote(0));
        assert_eq!(clipboard.items[0].path, PathBuf::from("/remote-b/kept.txt"));
    }

    #[test]
    fn files_intent_context_is_invalidated_when_the_tree_root_changes() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/first/root"), scanner);
        let delayed_intent = sidebar.files_intent_context();

        assert!(sidebar
            .set_current_dir(PathBuf::from("/second/root"))
            .is_none());
        assert!(!sidebar.files_intent_is_current(&delayed_intent));
    }

    fn entry(parent: &Path, name: impl Into<String>, is_dir: bool) -> FileEntry {
        let name = name.into();
        FileEntry {
            path: parent.join(&name),
            name,
            is_dir,
        }
    }

    fn poll_until_loaded(sidebar: &mut Sidebar) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while sidebar.has_pending_scan() && Instant::now() < deadline {
            sidebar.poll_scan_results();
            std::thread::sleep(Duration::from_millis(1));
        }
        sidebar.poll_scan_results();
        assert!(!sidebar.has_pending_scan(), "directory scan timed out");
    }

    #[test]
    fn sidebar_creation_does_not_scan_synchronously() {
        let sidebar = Sidebar::new();
        assert!(sidebar.visible);
        let root = sidebar.root.as_ref().expect("root node");
        assert!(root.children.is_empty());
        assert!(!root.is_loading());
    }

    #[test]
    fn stale_slow_scan_cannot_replace_a_new_generation() {
        let old = PathBuf::from("/virtual/slow");
        let new = PathBuf::from("/virtual/new");
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let scanner = {
            let old = old.clone();
            let new = new.clone();
            let started = started.clone();
            let release = release.clone();
            Arc::new(move |path: &Path| {
                if path == old {
                    started.wait();
                    release.wait();
                    Ok(DirectoryListing::complete(vec![entry(
                        path,
                        "stale.txt",
                        false,
                    )]))
                } else if path == new {
                    Ok(DirectoryListing::complete(vec![entry(
                        path,
                        "fresh.txt",
                        false,
                    )]))
                } else {
                    Err(io::Error::new(io::ErrorKind::NotFound, "unexpected path"))
                }
            }) as Arc<ScanFn>
        };

        let mut sidebar = Sidebar::with_scanner(old, scanner);
        assert!(sidebar.refresh().is_none());
        started.wait();
        assert!(sidebar.set_current_dir(new.clone()).is_none());
        release.wait();
        poll_until_loaded(&mut sidebar);

        assert_eq!(sidebar.current_dir, new);
        let root = sidebar.root.as_ref().expect("new root");
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "fresh.txt");
    }

    #[test]
    fn scan_requests_queue_instead_of_failing_while_workers_are_busy() {
        // 旧有界队列（容量 8）在 worker 全忙时会拒绝第 9 个请求；现在用户
        // 动作只排队，并发仍由 SCAN_WORKERS 限定。闸门挡住 worker，让队列
        // 可观测地填满，全程不做真实文件系统/ssh 工作。
        let (gate_tx, gate_rx) = crossbeam_channel::bounded::<()>(0);
        let scanner = Arc::new(move |_: &Path| {
            let _ = gate_rx.recv();
            Ok(DirectoryListing::complete(vec![]))
        }) as Arc<ScanFn>;
        let service = DirectoryScanService::new(scanner).expect("scan workers spawn");

        const TOTAL: usize = SCAN_WORKERS + 16;
        for index in 0..TOTAL {
            service
                .request(
                    ScanRequest {
                        generation: 0,
                        path: PathBuf::from(format!("/virtual/queued/{index}")),
                        backend: ScanBackend::Local,
                    },
                    false,
                )
                .expect("queued scan requests must not be rejected");
        }
        // worker 最多拿走 SCAN_WORKERS 个，其余必须仍在队列里（超出旧容量）。
        assert!(service.request_rx.len() >= TOTAL - SCAN_WORKERS);

        // 关闭闸门放行，worker 清空队列并送达全部结果。
        drop(gate_tx);
        for _ in 0..TOTAL {
            service
                .result_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("every queued scan completes once the gate opens");
        }
    }

    #[test]
    fn fs_op_requests_queue_instead_of_failing_while_the_worker_is_busy() {
        // 与扫描服务同一契约：无界队列缓冲用户动作。不启动真 worker，
        // 队列只进不出，可确定性地观察它涨过旧容量 8。
        let (request_tx, request_rx) = crossbeam_channel::unbounded();
        let (_result_tx, result_rx) = crossbeam_channel::bounded(OP_RESULT_CAPACITY);
        let service = FsOpService {
            request_tx,
            result_rx,
        };

        const TOTAL: usize = 16;
        for _ in 0..TOTAL {
            service
                .request(FsOpRequest {
                    authority_generation: 0,
                    location: FsLocation::Local,
                    overlay: remote_fs::SshExecutionOverlay::default(),
                    hosts: Arc::new(Vec::new()),
                    kind: OpRequestKind::StartDir,
                    clipboard_intent: None,
                    cancel_token: None,
                })
                .expect("queued file operations must not be rejected");
        }
        assert_eq!(request_rx.len(), TOTAL);
    }

    #[test]
    fn every_entry_in_a_wide_directory_is_reachable_through_show_more() {
        let root_path = PathBuf::from("/virtual/wide");
        let scanner = Arc::new(move |path: &Path| {
            Ok(DirectoryListing::complete(
                (0..145)
                    .map(|index| entry(path, format!("file-{index:03}.txt"), false))
                    .collect(),
            ))
        }) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(root_path.clone(), scanner);

        assert!(sidebar.refresh().is_none());
        poll_until_loaded(&mut sidebar);
        let root = sidebar.root.as_ref().expect("loaded root");
        assert_eq!(root.children.len(), 145);
        assert_eq!(root.visible_children().len(), DIRECTORY_PAGE_SIZE);
        assert_eq!(root.remaining_children(), 145 - DIRECTORY_PAGE_SIZE);

        sidebar.show_more(&root_path);
        sidebar.show_more(&root_path);
        let root = sidebar.root.as_ref().expect("loaded root");
        assert_eq!(root.visible_children().len(), 145);
        assert_eq!(root.remaining_children(), 0);
        assert_eq!(root.visible_children().last().unwrap().name, "file-144.txt");
    }

    #[test]
    fn scan_errors_remain_visible_on_the_node() {
        let root_path = PathBuf::from("/virtual/denied");
        let scanner = Arc::new(|_: &Path| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "permission denied",
            ))
        }) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(root_path, scanner);

        assert!(sidebar.refresh().is_none());
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut errors = Vec::new();
        while errors.is_empty() && Instant::now() < deadline {
            errors.extend(sidebar.poll_scan_results());
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(errors
            .iter()
            .any(|error| error.contains("permission denied")));
        assert_eq!(
            sidebar.root.as_ref().and_then(FileTreeNode::load_error),
            Some("permission denied")
        );
    }

    #[test]
    fn one_hostile_directory_cannot_grow_the_retained_sort_buffer_without_bound() {
        let root_path = PathBuf::from("/virtual/huge");
        let scanner = Arc::new(move |path: &Path| {
            Ok(DirectoryListing::complete(
                (0..MAX_DIRECTORY_ENTRIES + 100)
                    .map(|index| entry(path, format!("file-{index:05}"), false))
                    .collect(),
            ))
        }) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(root_path, scanner);

        assert!(sidebar.refresh().is_none());
        poll_until_loaded(&mut sidebar);
        let root = sidebar.root.as_ref().unwrap();
        assert_eq!(root.children.len(), MAX_DIRECTORY_ENTRIES);
        assert!(root.entries_truncated());
    }

    fn poll_ops_until(sidebar: &mut Sidebar, done: impl Fn(&Sidebar) -> bool) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut messages = Vec::new();
        while !done(sidebar) && Instant::now() < deadline {
            messages.extend(sidebar.poll_op_results());
            std::thread::sleep(Duration::from_millis(1));
        }
        messages.extend(sidebar.poll_op_results());
        messages
    }

    /// Replace the real worker with deterministic request/result channels so
    /// race tests can change UI state after dispatch but before completion.
    /// The request channel is unbounded to match the production service.
    fn controlled_op_service(sidebar: &mut Sidebar) -> (Receiver<FsOpRequest>, Sender<OpEvent>) {
        let (request_tx, request_rx) = crossbeam_channel::unbounded();
        let (result_tx, result_rx) = crossbeam_channel::bounded(OP_RESULT_CAPACITY);
        sidebar.op_service = Some(FsOpService {
            request_tx,
            result_rx,
        });
        (request_rx, result_tx)
    }

    fn complete_request(
        results: &Sender<OpEvent>,
        request: FsOpRequest,
        outcome: Result<Option<PathBuf>, String>,
        batch_outcome: Option<BatchOutcome>,
    ) {
        results
            .send(OpEvent::Done(Box::new(FsOpResult {
                authority_generation: request.authority_generation,
                kind: request.kind,
                clipboard_intent: request.clipboard_intent,
                outcome,
                warning: None,
                batch_outcome,
                cancelled: false,
                cancel_token: request.cancel_token,
            })))
            .unwrap();
    }

    /// 唯一临时目录，Drop 时递归清理（本机文件操作测试用真实文件系统）。
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "ember-sidebar-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir(&path).expect("create test dir");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn failed_remote_start_dir_reports_and_recovers_to_local_without_network() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/virtual/local"), scanner);
        // 没配置任何主机：Remote(0) 的起始目录解析会在 worker 上立即失败，
        // 错误必须可见，并自动回到 Local，而不是卡在空远端树或触网。
        assert!(sidebar.set_location(FsLocation::Remote(0)).is_none());
        assert!(sidebar.is_starting());
        assert!(sidebar.has_pending_op());

        let messages = poll_ops_until(&mut sidebar, |sidebar| {
            sidebar.location() == &FsLocation::Local && !sidebar.has_pending_op()
        });
        assert!(
            messages
                .iter()
                .any(|message| message.contains("已返回 Local")),
            "messages so far: {messages:?}"
        );
        assert_eq!(sidebar.location(), &FsLocation::Local);
        assert_eq!(sidebar.current_dir, std::env::current_dir().unwrap());
        assert!(sidebar.location_error().is_none());
        assert!(!sidebar.is_starting());
        assert!(!sidebar.has_pending_op());
        poll_until_loaded(&mut sidebar);
    }

    #[test]
    fn a_stale_start_dir_result_cannot_hijack_a_newer_location() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/virtual/local"), scanner);
        // Remote(0) 的解析还在路上就切走：迟到结果必须按 generation 丢弃。
        assert!(sidebar.set_location(FsLocation::Remote(0)).is_none());
        assert!(sidebar.set_location(FsLocation::Local).is_none());

        let _ = poll_ops_until(&mut sidebar, |sidebar| !sidebar.has_pending_op());
        assert!(sidebar.location_error().is_none());
        assert_eq!(sidebar.current_dir, std::env::current_dir().unwrap());
        poll_until_loaded(&mut sidebar);
    }

    #[test]
    fn local_file_operations_run_on_the_op_worker_and_refresh_the_tree() {
        let dir = TestDir::new();
        std::fs::write(dir.0.join("existing.txt"), b"data").unwrap();
        let mut sidebar = Sidebar::with_scanner(dir.0.clone(), Arc::new(scan_dir) as Arc<ScanFn>);
        assert!(sidebar.refresh().is_none());
        poll_until_loaded(&mut sidebar);
        assert_eq!(
            sidebar.root.as_ref().unwrap().children.len(),
            1,
            "temp dir starts with exactly one entry"
        );

        // 新建文件夹：成功后根目录被重新扫描，新条目可见。
        let sub = dir.0.join("sub");
        assert!(sidebar
            .request_fs_op(FsOpKind::CreateDir(sub.clone()), false)
            .is_none());
        let messages = poll_ops_until(&mut sidebar, |sidebar| !sidebar.has_pending_op());
        assert!(
            messages
                .iter()
                .any(|message| message.contains("已创建文件夹")),
            "messages: {messages:?}"
        );
        poll_until_loaded(&mut sidebar);
        let names: Vec<&str> = sidebar
            .root
            .as_ref()
            .unwrap()
            .children
            .iter()
            .map(|child| child.name.as_str())
            .collect();
        assert_eq!(names, vec!["sub", "existing.txt"]);

        // 重名冲突：AlreadyExists 要变成状态栏错误消息。
        assert!(sidebar
            .request_fs_op(FsOpKind::CreateDir(sub.clone()), false)
            .is_none());
        let messages = poll_ops_until(&mut sidebar, |sidebar| !sidebar.has_pending_op());
        assert!(
            messages
                .iter()
                .any(|message| message.contains("新建文件夹失败") && message.contains("exists")),
            "messages: {messages:?}"
        );

        // 重命名 + 删除同样经 worker 落地。
        let renamed = dir.0.join("renamed.txt");
        assert!(sidebar
            .request_fs_op(
                FsOpKind::Rename {
                    src: dir.0.join("existing.txt"),
                    dst: renamed.clone(),
                },
                false,
            )
            .is_none());
        let _ = poll_ops_until(&mut sidebar, |sidebar| !sidebar.has_pending_op());
        assert!(renamed.exists());
        assert!(sidebar
            .request_fs_op(FsOpKind::Delete(sub.clone()), false)
            .is_none());
        let _ = poll_ops_until(&mut sidebar, |sidebar| !sidebar.has_pending_op());
        assert!(!sub.exists());

        // cut-paste 成功后剪贴板被清空。
        sidebar.set_clipboard(remote_fs::FsClipboard {
            loc: FsLocation::Local,
            overlay: remote_fs::SshExecutionOverlay::default(),
            items: vec![remote_fs::FsClipboardItem {
                path: renamed.clone(),
                is_dir: false,
            }],
            cut: true,
        });
        let moved = sub.with_file_name("moved.txt");
        assert!(sidebar
            .request_fs_op(
                FsOpKind::Rename {
                    src: renamed.clone(),
                    dst: moved.clone(),
                },
                true,
            )
            .is_none());
        let _ = poll_ops_until(&mut sidebar, |sidebar| !sidebar.has_pending_op());
        assert!(moved.exists());
        assert!(sidebar.clipboard.is_none());
    }

    #[test]
    fn old_completion_cannot_clear_a_new_identical_clipboard_intent() {
        let dir = TestDir::new();
        let src = dir.0.join("source.txt");
        let dst = dir.0.join("copy.txt");
        let mut sidebar = Sidebar::with_scanner(dir.0.clone(), Arc::new(scan_dir) as Arc<ScanFn>);
        let (requests, results) = controlled_op_service(&mut sidebar);
        let clipboard = remote_fs::FsClipboard {
            loc: FsLocation::Local,
            overlay: remote_fs::SshExecutionOverlay::default(),
            items: vec![remote_fs::FsClipboardItem {
                path: src.clone(),
                is_dir: false,
            }],
            cut: true,
        };

        sidebar.set_clipboard(clipboard.clone());
        let old_intent = sidebar.clipboard_intent;
        assert!(sidebar
            .request_fs_op(FsOpKind::Copy { src, dst }, true)
            .is_none());
        let request = requests.recv().unwrap();
        assert_eq!(request.clipboard_intent, old_intent);

        // Same payload, distinct user action: payload equality must not let
        // the old completion erase this replacement.
        sidebar.set_clipboard(clipboard.clone());
        assert_ne!(sidebar.clipboard_intent, old_intent);
        complete_request(&results, request, Ok(None), None);
        let messages = sidebar.poll_op_results();

        assert_eq!(sidebar.clipboard.as_ref(), Some(&clipboard));
        assert!(messages.iter().any(|message| message.contains("已粘贴")));
    }

    #[test]
    fn saved_destination_uses_transient_clipboard_socket_for_direct_cut_rename() {
        let dir = TestDir::new();
        let mut sidebar = Sidebar::with_scanner(dir.0.clone(), Arc::new(scan_dir) as Arc<ScanFn>);
        let saved = crate::config::default_remote_hosts()[0].clone();
        assert!(sidebar
            .set_remote_hosts(std::slice::from_ref(&saved))
            .is_none());
        sidebar.location = FsLocation::Remote(0);
        let (requests, results) = controlled_op_service(&mut sidebar);

        let mut transient = saved.clone();
        transient.name = "process-observed".to_string();
        let live_overlay = remote_fs::SshExecutionOverlay::from_control_path(Some(
            "/run/user/1000/anvil/live-%C".to_string(),
        ));
        sidebar.set_clipboard(remote_fs::FsClipboard {
            loc: FsLocation::Transient(transient),
            overlay: live_overlay.clone(),
            items: vec![remote_fs::FsClipboardItem {
                path: PathBuf::from("/remote/source.txt"),
                is_dir: false,
            }],
            cut: true,
        });
        assert!(remote_fs::same_files_namespace(
            &sidebar.clipboard.as_ref().unwrap().loc,
            sidebar.location(),
            &sidebar.remote_hosts,
        ));

        assert!(sidebar
            .request_fs_op_with_overlay(
                FsOpKind::Rename {
                    src: PathBuf::from("/remote/source.txt"),
                    dst: PathBuf::from("/remote/dst/source.txt"),
                },
                true,
                live_overlay.clone(),
            )
            .is_none());
        let request = requests.recv().unwrap();
        assert_eq!(request.location, FsLocation::Remote(0));
        assert_eq!(request.overlay, live_overlay);
        assert!(matches!(
            &request.kind,
            OpRequestKind::Fs(FsOpKind::Rename { .. })
        ));

        complete_request(&results, request, Ok(None), None);
        let _ = sidebar.poll_op_results();
        assert!(sidebar.clipboard.is_none());
    }

    #[test]
    fn old_partial_batch_cannot_shrink_a_new_identical_clipboard_intent() {
        let dir = TestDir::new();
        let a = dir.0.join("a.txt");
        let b = dir.0.join("b.txt");
        let dst = dir.0.join("dst");
        let mut sidebar = Sidebar::with_scanner(dir.0.clone(), Arc::new(scan_dir) as Arc<ScanFn>);
        let (requests, results) = controlled_op_service(&mut sidebar);
        let clipboard = remote_fs::FsClipboard {
            loc: FsLocation::Local,
            overlay: remote_fs::SshExecutionOverlay::default(),
            items: vec![
                remote_fs::FsClipboardItem {
                    path: a.clone(),
                    is_dir: false,
                },
                remote_fs::FsClipboardItem {
                    path: b.clone(),
                    is_dir: false,
                },
            ],
            cut: true,
        };
        let batch = BatchIntent::Paste {
            src_endpoint: Box::new(remote_fs::FsEndpointSnapshot::new(
                FsLocation::Local,
                remote_fs::SshExecutionOverlay::default(),
            )),
            dst_endpoint: Box::new(remote_fs::FsEndpointSnapshot::new(
                FsLocation::Local,
                remote_fs::SshExecutionOverlay::default(),
            )),
            dst_dir: dst,
            items: vec![(a.clone(), false), (b.clone(), false)],
            cut: true,
        };

        sidebar.set_clipboard(clipboard.clone());
        let old_intent = sidebar.clipboard_intent;
        assert!(sidebar.request_batch(batch, true).is_none());
        let request = requests.recv().unwrap();
        sidebar.set_clipboard(clipboard.clone());
        assert_ne!(sidebar.clipboard_intent, old_intent);
        complete_request(
            &results,
            request,
            Ok(None),
            Some(BatchOutcome {
                succeeded: 1,
                failed: vec![(b, "collision".to_string())],
                warnings: Vec::new(),
            }),
        );
        let _ = sidebar.poll_op_results();

        assert_eq!(sidebar.clipboard.as_ref(), Some(&clipboard));
        assert_eq!(sidebar.clipboard.as_ref().unwrap().items.len(), 2);
    }

    #[test]
    fn refresh_during_transfer_keeps_progress_and_retires_the_exact_track() {
        let dir = TestDir::new();
        let src = dir.0.join("source.bin");
        let mut sidebar = Sidebar::with_scanner(dir.0.clone(), Arc::new(scan_dir) as Arc<ScanFn>);
        let (requests, results) = controlled_op_service(&mut sidebar);
        sidebar.set_clipboard(remote_fs::FsClipboard {
            loc: FsLocation::Local,
            overlay: remote_fs::SshExecutionOverlay::default(),
            items: vec![remote_fs::FsClipboardItem {
                path: src.clone(),
                is_dir: false,
            }],
            cut: true,
        });
        let transfer = FsTransfer {
            src_endpoint: remote_fs::FsEndpointSnapshot::new(
                FsLocation::Local,
                remote_fs::SshExecutionOverlay::default(),
            ),
            src,
            src_is_dir: false,
            dst_endpoint: remote_fs::FsEndpointSnapshot::new(
                FsLocation::Remote(0),
                remote_fs::SshExecutionOverlay::default(),
            ),
            dst_dir: PathBuf::from("/remote/dst"),
            cut: true,
        };

        assert!(sidebar.request_transfer(transfer, true).is_none());
        let request = requests.recv().unwrap();
        let transfer_token = request.cancel_token.as_ref().unwrap().clone();
        let scan_generation = sidebar.scan_generation;
        let authority_generation = sidebar.authority_generation;
        assert!(sidebar.refresh().is_none());
        assert_ne!(sidebar.scan_generation, scan_generation);
        assert_eq!(sidebar.authority_generation, authority_generation);

        results
            .send(OpEvent::Progress {
                authority_generation: request.authority_generation,
                token: transfer_token,
                bytes: 4096,
            })
            .unwrap();
        let _ = sidebar.poll_op_results();
        assert_eq!(
            sidebar.transfer_status().map(|status| status.bytes),
            Some(4096)
        );

        complete_request(
            &results,
            request,
            Ok(Some(PathBuf::from("/remote/dst/source.bin"))),
            None,
        );
        let messages = sidebar.poll_op_results();

        assert!(!sidebar.has_pending_op());
        assert!(sidebar.transfer_status().is_none());
        assert!(sidebar.clipboard.is_none());
        assert!(messages.iter().any(|message| message.contains("已上传")));
    }

    #[test]
    fn a_failed_transfer_keeps_clipboard_and_never_deletes_the_source() {
        let dir = TestDir::new();
        let src = dir.0.join("keep.txt");
        std::fs::write(&src, b"data").unwrap();
        let mut sidebar = Sidebar::with_scanner(dir.0.clone(), Arc::new(scan_dir) as Arc<ScanFn>);
        assert!(sidebar.refresh().is_none());
        poll_until_loaded(&mut sidebar);

        // 本机 → 未配置的 Remote(9)：传输在 worker 上失败，错误进状态栏；
        // cut 的删源只在复制成功后发生 —— 剪贴板保留、源文件原样还在。
        sidebar.set_clipboard(remote_fs::FsClipboard {
            loc: FsLocation::Local,
            overlay: remote_fs::SshExecutionOverlay::default(),
            items: vec![remote_fs::FsClipboardItem {
                path: src.clone(),
                is_dir: false,
            }],
            cut: true,
        });
        let transfer = FsTransfer {
            src_endpoint: remote_fs::FsEndpointSnapshot::new(
                FsLocation::Local,
                remote_fs::SshExecutionOverlay::default(),
            ),
            src: src.clone(),
            src_is_dir: false,
            dst_endpoint: remote_fs::FsEndpointSnapshot::new(
                FsLocation::Remote(9),
                remote_fs::SshExecutionOverlay::default(),
            ),
            dst_dir: PathBuf::from("/tmp"),
            cut: true,
        };
        assert!(sidebar.request_transfer(transfer, true).is_none());
        assert!(sidebar.has_pending_op());
        let messages = poll_ops_until(&mut sidebar, |sidebar| !sidebar.has_pending_op());
        assert!(
            messages.iter().any(|message| message.contains("上传失败")),
            "messages: {messages:?}"
        );
        assert!(
            sidebar.clipboard.is_some(),
            "failed cut-paste keeps the clipboard"
        );
        assert!(src.exists(), "failed transfer never deletes the source");
    }

    #[test]
    fn same_location_transfer_is_rejected_before_contacting_a_host() {
        let dir = TestDir::new();
        let src = dir.0.join("file.txt");
        std::fs::write(&src, b"data").unwrap();
        let mut sidebar = Sidebar::with_scanner(dir.0.clone(), Arc::new(scan_dir) as Arc<ScanFn>);

        // 本机 → 本机不是传输（那是 copy/rename），worker 如实报错且不触网。
        let transfer = FsTransfer {
            src_endpoint: remote_fs::FsEndpointSnapshot::new(
                FsLocation::Local,
                remote_fs::SshExecutionOverlay::default(),
            ),
            src: src.clone(),
            src_is_dir: false,
            dst_endpoint: remote_fs::FsEndpointSnapshot::new(
                FsLocation::Local,
                remote_fs::SshExecutionOverlay::default(),
            ),
            dst_dir: dir.0.clone(),
            cut: false,
        };
        assert!(sidebar.request_transfer(transfer, false).is_none());
        let messages = poll_ops_until(&mut sidebar, |sidebar| !sidebar.has_pending_op());
        assert!(
            messages
                .iter()
                .any(|message| message.contains("传输失败") && message.contains("copy/rename")),
            "messages: {messages:?}"
        );
        assert!(src.exists());
    }

    #[test]
    fn a_transfer_cancelled_while_queued_is_reported_as_cancelled_not_error() {
        // 直接驱动 FsOpService：预置取消令牌，worker 取请求时必须按取消
        // 收尾（中性、非错误），绝不开始执行（Remote(9) 若执行必报错）。
        let service = FsOpService::new().expect("op service");
        let token = Arc::new(AtomicBool::new(true));
        let request = FsOpRequest {
            authority_generation: 0,
            location: FsLocation::Local,
            overlay: remote_fs::SshExecutionOverlay::default(),
            hosts: Arc::new(Vec::new()),
            kind: OpRequestKind::Transfer(Box::new(FsTransfer {
                src_endpoint: remote_fs::FsEndpointSnapshot::new(
                    FsLocation::Local,
                    remote_fs::SshExecutionOverlay::default(),
                ),
                src: PathBuf::from("/nonexistent-source.bin"),
                src_is_dir: false,
                dst_endpoint: remote_fs::FsEndpointSnapshot::new(
                    FsLocation::Remote(9),
                    remote_fs::SshExecutionOverlay::default(),
                ),
                dst_dir: PathBuf::from("/tmp"),
                cut: false,
            })),
            clipboard_intent: None,
            cancel_token: Some(token.clone()),
        };
        service.request(request).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match service.result_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(OpEvent::Done(result)) => {
                    assert!(result.cancelled, "queued cancel must surface as cancelled");
                    assert!(result.outcome.is_ok(), "cancelled is neutral, not an error");
                    assert!(Arc::ptr_eq(result.cancel_token.as_ref().unwrap(), &token));
                    break;
                }
                Ok(OpEvent::Progress { .. }) => {
                    panic!("cancelled-before-start must never stream")
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(_) => panic!("timed out waiting for the cancelled result"),
            }
        }
    }

    #[test]
    fn cancel_marks_tokens_once_and_set_location_abandons_in_flight_transfers() {
        let dir = TestDir::new();
        let mut sidebar = Sidebar::with_scanner(dir.0.clone(), Arc::new(scan_dir) as Arc<ScanFn>);
        let make_transfer = || FsTransfer {
            src_endpoint: remote_fs::FsEndpointSnapshot::new(
                FsLocation::Local,
                remote_fs::SshExecutionOverlay::default(),
            ),
            src: dir.0.join("file.bin"),
            src_is_dir: false,
            dst_endpoint: remote_fs::FsEndpointSnapshot::new(
                FsLocation::Remote(9),
                remote_fs::SshExecutionOverlay::default(),
            ),
            dst_dir: PathBuf::from("/tmp"),
            cut: false,
        };

        assert!(sidebar.request_transfer(make_transfer(), false).is_none());
        assert!(sidebar.transfer_status().is_some());
        // 取消是幂等的：重复调用不再新标记。
        assert_eq!(sidebar.cancel_transfers(), 1);
        assert_eq!(sidebar.cancel_transfers(), 0);
        // 与完成竞争的取消是 no-op：worker 的 Done 到达后在途条目被摘除。
        let _ = poll_ops_until(&mut sidebar, |sidebar| sidebar.transfer_status().is_none());
        assert!(sidebar.transfer_status().is_none());

        // 切换位置即放弃在途传输：条目立即清空，迟到的结果被 generation 丢弃。
        assert!(sidebar.request_transfer(make_transfer(), false).is_none());
        assert!(sidebar.transfer_status().is_some());
        assert!(sidebar.set_location(FsLocation::Remote(0)).is_none());
        assert!(sidebar.transfer_status().is_none());
        let _ = poll_ops_until(&mut sidebar, |sidebar| !sidebar.has_pending_op());
        assert!(sidebar.transfer_status().is_none());
        assert!(sidebar.set_location(FsLocation::Local).is_none());
    }

    // ---- 拖放导入计划 ----

    #[test]
    fn plan_drop_builds_copy_items_for_local_and_uploads_for_remote() {
        let dir = TestDir::new();
        let src = dir.0.join("inbox");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"aa").unwrap();
        std::fs::write(dir.0.join("b.bin"), b"bbb").unwrap();
        let target = dir.0.join("target");
        std::fs::create_dir(&target).unwrap();
        let dropped = vec![src.clone(), dir.0.join("b.bin")];

        let plan = plan_drop(&dropped, &target, &FsLocation::Local).unwrap();
        assert_eq!(plan.total_bytes, 5);
        assert!(plan.refused_existing.is_empty());
        assert_eq!(
            plan.items,
            vec![
                DropPlanItem::Copy {
                    src: src.clone(),
                    dst: target.join("inbox"),
                    is_dir: true,
                },
                DropPlanItem::Copy {
                    src: dir.0.join("b.bin"),
                    dst: target.join("b.bin"),
                    is_dir: false,
                },
            ]
        );

        let plan = plan_drop(&dropped, &target, &FsLocation::Remote(0)).unwrap();
        let expected_uploads = vec![
            DropPlanItem::Upload {
                src: src.clone(),
                dst_dir: target.clone(),
                is_dir: true,
            },
            DropPlanItem::Upload {
                src: dir.0.join("b.bin"),
                dst_dir: target.clone(),
                is_dir: false,
            },
        ];
        assert_eq!(plan.items, expected_uploads);
        let transient = FsLocation::Transient(crate::config::default_remote_hosts()[0].clone());
        let transient_plan = plan_drop(&dropped, &target, &transient).unwrap();
        assert_eq!(
            transient_plan.items, expected_uploads,
            "transient SSH is a remote upload backend too"
        );
        // Remote 位置不做就地预检（worker 的 17/AlreadyExists 兜底）。
        assert!(plan.refused_existing.is_empty());
    }

    #[test]
    fn plan_drop_flags_existing_targets_and_skips_unusable_paths() {
        let dir = TestDir::new();
        let src = dir.0.join("a.txt");
        std::fs::write(&src, b"aa").unwrap();
        let target = dir.0.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("a.txt"), b"old").unwrap();

        let dropped = vec![
            src.clone(),
            PathBuf::from("relative/file.txt"),
            PathBuf::from("/"),
        ];
        let plan = plan_drop(&dropped, &target, &FsLocation::Local).unwrap();
        assert!(plan.items.is_empty());
        assert_eq!(plan.refused_existing, vec![src]);

        // 全部不可用 → 整批拒绝。
        let dropped = vec![PathBuf::from("relative/file.txt")];
        assert!(plan_drop(&dropped, &target, &FsLocation::Local).is_err());
    }

    #[test]
    fn plan_drop_refuses_oversized_batches_wholesale() {
        let dir = TestDir::new();
        std::fs::write(dir.0.join("a.bin"), vec![1u8; 100]).unwrap();
        std::fs::write(dir.0.join("b.bin"), vec![2u8; 100]).unwrap();
        let dropped = vec![dir.0.join("a.bin"), dir.0.join("b.bin")];
        let target = dir.0.join("target");
        std::fs::create_dir(&target).unwrap();

        // 字节帽：150 字节的帽容不下 200 字节 → 整批拒绝。
        let error =
            plan_drop_with_limits(&dropped, &target, &FsLocation::Local, 256, 150).unwrap_err();
        assert!(error.contains("整批拒绝"), "{error}");
        // 条目帽：1 条容不下 2 条。
        let error =
            plan_drop_with_limits(&dropped, &target, &FsLocation::Local, 1, u64::MAX).unwrap_err();
        assert!(error.contains("拖放条目过多"), "{error}");
        // 正好贴帽可以通过。
        let plan = plan_drop_with_limits(&dropped, &target, &FsLocation::Local, 2, 200).unwrap();
        assert_eq!(plan.total_bytes, 200);
        assert_eq!(plan.items.len(), 2);
    }

    #[test]
    fn drop_size_walk_caps_depth_and_never_follows_symlinked_dirs() {
        let dir = TestDir::new();
        // 70 层深树：超过 64 层的部分不计入。
        let mut deep = dir.0.clone();
        for level in 0..70 {
            deep = deep.join(format!("d{level}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("bottom.bin"), vec![9u8; 1000]).unwrap();
        // 浅层文件正常计入。
        std::fs::write(dir.0.join("top.bin"), vec![1u8; 100]).unwrap();
        // 符号链接目录不跟随：链接目标在被遍历的树之外，大文件不计入。
        let outside = TestDir::new();
        std::fs::write(outside.0.join("big.bin"), vec![7u8; 5000]).unwrap();
        std::os::unix::fs::symlink(&outside.0, dir.0.join("link")).unwrap();

        let dropped = vec![dir.0.clone()];
        let target = dir.0.join("target");
        std::fs::create_dir(&target).unwrap();
        let plan = plan_drop(&dropped, &target, &FsLocation::Local).unwrap();
        // 只有 top.bin 的 100 字节 + target 目录本身（0）；bottom.bin
        // 在 70 层深处（超帽），big.bin 隔着符号链接（不跟随）。
        assert_eq!(plan.total_bytes, 100, "plan: {plan:?}");
    }

    // ---- 多选 ----

    fn p(name: &str) -> PathBuf {
        PathBuf::from(format!("/virtual/{name}"))
    }

    fn paths(selection: &BTreeMap<PathBuf, bool>) -> Vec<&Path> {
        selection.keys().map(PathBuf::as_path).collect()
    }

    #[test]
    fn select_single_toggle_and_range_semantics() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/virtual"), scanner);

        // 单选：选中集收缩为一个，锚点跟过去。
        sidebar.select_single(&p("a"), false);
        assert_eq!(paths(&sidebar.selection), [Path::new("/virtual/a")]);
        assert_eq!(
            sidebar.selected_path.as_deref(),
            Some(Path::new("/virtual/a"))
        );

        // ctrl 切换：加选 b、再点 a 取消 a。
        sidebar.select_toggle(&p("b"), true);
        assert_eq!(sidebar.selection.len(), 2);
        sidebar.select_toggle(&p("a"), false);
        assert_eq!(paths(&sidebar.selection), [Path::new("/virtual/b")]);
        // 锚点是最后点击的行。
        assert_eq!(
            sidebar.selected_path.as_deref(),
            Some(Path::new("/virtual/a"))
        );

        // shift 范围：锚点 a（不在选中集里了，但仍是锚点）→ d 的闭区间。
        let rows: Vec<(PathBuf, bool)> = ["a", "b", "c", "d"]
            .iter()
            .map(|name| (p(name), false))
            .collect();
        sidebar.select_single(&p("a"), false);
        sidebar.select_range(&rows, &p("c"), false);
        assert_eq!(
            paths(&sidebar.selection),
            [
                Path::new("/virtual/a"),
                Path::new("/virtual/b"),
                Path::new("/virtual/c")
            ]
        );
        // 锚点不动，连续 shift 扩展。
        assert_eq!(
            sidebar.selected_path.as_deref(),
            Some(Path::new("/virtual/a"))
        );
        sidebar.select_range(&rows, &p("d"), true);
        assert_eq!(sidebar.selection.len(), 4);

        // 反向范围（从下往上点）同样工作。
        sidebar.select_single(&p("d"), false);
        sidebar.select_range(&rows, &p("b"), false);
        assert_eq!(sidebar.selection.len(), 3);

        // 锚点不在可见行序里（树变了）→ 退化为单选。
        sidebar.set_current_dir(p("elsewhere"));
        sidebar.select_range(&rows, &p("c"), false);
        assert_eq!(paths(&sidebar.selection), [Path::new("/virtual/c")]);
    }

    #[test]
    fn resolve_menu_targets_inside_vs_outside_the_selection() {
        let mut selection = BTreeMap::from([(p("a"), false), (p("b"), true)]);
        // 点在选中集内：目标是整个选中集，选中集不变。
        let (new_selection, targets) = Sidebar::resolve_menu_targets(&selection, &p("a"), false);
        assert_eq!(new_selection, selection);
        assert_eq!(targets.len(), 2);

        // 点在选中集外：选中集收缩为该行，目标只有它。
        let (new_selection, targets) = Sidebar::resolve_menu_targets(&selection, &p("c"), true);
        assert_eq!(new_selection, BTreeMap::from([(p("c"), true)]));
        assert_eq!(targets, vec![(p("c"), true)]);
        selection.clear();
        let (_, targets) = Sidebar::resolve_menu_targets(&selection, &p("z"), false);
        assert_eq!(targets, vec![(p("z"), false)]);
    }

    // ---- 树内过滤 ----

    fn filter_fixture() -> FileTreeNode {
        // root/
        //   src/      -> main.rs, lib.rs
        //   target/   -> app.bin
        //   README.md
        let mut root =
            FileTreeNode::directory(PathBuf::from("/virtual"), "virtual".to_string(), true);
        let mut src =
            FileTreeNode::directory(PathBuf::from("/virtual/src"), "src".to_string(), false);
        src.children = vec![
            FileTreeNode::from_entry(FileEntry {
                name: "main.rs".to_string(),
                path: PathBuf::from("/virtual/src/main.rs"),
                is_dir: false,
            }),
            FileTreeNode::from_entry(FileEntry {
                name: "lib.rs".to_string(),
                path: PathBuf::from("/virtual/src/lib.rs"),
                is_dir: false,
            }),
        ];
        src.visible_children = 2;
        let mut target = FileTreeNode::directory(
            PathBuf::from("/virtual/target"),
            "target".to_string(),
            false,
        );
        target.children = vec![FileTreeNode::from_entry(FileEntry {
            name: "app.bin".to_string(),
            path: PathBuf::from("/virtual/target/app.bin"),
            is_dir: false,
        })];
        target.visible_children = 1;
        root.children = vec![
            src,
            target,
            FileTreeNode::from_entry(FileEntry {
                name: "README.md".to_string(),
                path: PathBuf::from("/virtual/README.md"),
                is_dir: false,
            }),
        ];
        root.visible_children = 3;
        root.load_state = DirectoryLoadState::Loaded;
        root
    }

    #[test]
    fn filter_keeps_matches_and_ancestors_and_restores_on_clear() {
        let root = filter_fixture();
        // 空查询 = 恒等（调用方跳过过滤；直接调用也应全保留）。
        let filtered = root.filtered("").expect("empty query keeps everything");
        assert_eq!(filtered.children.len(), 3);

        // "rs"：main.rs/lib.rs 命中，祖先 src 保留且强制展开；target/README 剪掉。
        let filtered = root.filtered("rs").expect("matches exist");
        assert_eq!(filtered.children.len(), 1);
        let src = &filtered.children[0];
        assert_eq!(src.name, "src");
        assert!(src.expanded, "ancestor auto-expanded while filtering");
        assert_eq!(src.children.len(), 2);

        // 大小写不敏感："readme" 命中 README.md。
        let filtered = root.filtered("readme").expect("case-insensitive");
        assert_eq!(filtered.children.len(), 1);
        assert_eq!(filtered.children[0].name, "README.md");

        // 什么都不命中 → None；原树展开状态没有被改动（清空过滤即恢复）。
        assert!(root.filtered("zzz-no-match").is_none());
        assert!(!root.children[0].expanded, "原树的展开状态未被过滤改动");
        assert!(!root.children[1].expanded);
    }

    // ---- 批量操作 ----

    #[test]
    fn batch_delete_continues_past_errors_and_summarizes() {
        let dir = TestDir::new();
        let a = dir.0.join("a.txt");
        let b = dir.0.join("b.txt");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let missing = dir.0.join("missing.txt");
        let mut sidebar = Sidebar::with_scanner(dir.0.clone(), Arc::new(scan_dir) as Arc<ScanFn>);
        assert!(sidebar.refresh().is_none());
        poll_until_loaded(&mut sidebar);

        let batch = BatchIntent::Delete {
            endpoint: Box::new(remote_fs::FsEndpointSnapshot::new(
                FsLocation::Local,
                remote_fs::SshExecutionOverlay::default(),
            )),
            items: vec![a.clone(), missing.clone(), b.clone()],
        };
        assert!(sidebar.request_batch(batch, false).is_none());
        let messages = poll_ops_until(&mut sidebar, |sidebar| !sidebar.has_pending_op());
        // 三项中一项失败（missing）：失败不阻断其余项，汇总进状态栏。
        assert!(
            messages
                .iter()
                .any(|message| message.contains("3 项中 1 项失败")
                    && message.contains("missing.txt")),
            "messages: {messages:?}"
        );
        assert!(!a.exists() && !b.exists());
        // 父目录被重新扫描：树里不再出现 a/b。
        poll_until_loaded(&mut sidebar);
        let names: Vec<&str> = sidebar
            .root
            .as_ref()
            .unwrap()
            .children
            .iter()
            .map(|child| child.name.as_str())
            .collect();
        assert!(
            !names.contains(&"a.txt") && !names.contains(&"b.txt"),
            "{names:?}"
        );
    }

    #[test]
    fn batch_paste_skips_collisions_and_shrinks_the_cut_clipboard() {
        let dir = TestDir::new();
        let src_dir = dir.0.join("src");
        let dst_dir = dir.0.join("dst");
        std::fs::create_dir(&src_dir).unwrap();
        std::fs::create_dir(&dst_dir).unwrap();
        let a = src_dir.join("a.txt");
        let b = src_dir.join("b.txt");
        std::fs::write(&a, b"aa").unwrap();
        std::fs::write(&b, b"bb").unwrap();
        // b 的目标已存在 → AlreadyExists，粘贴继续、该项计入失败。
        std::fs::write(dst_dir.join("b.txt"), b"old").unwrap();

        let mut sidebar = Sidebar::with_scanner(dir.0.clone(), Arc::new(scan_dir) as Arc<ScanFn>);
        sidebar.set_clipboard(remote_fs::FsClipboard {
            loc: FsLocation::Local,
            overlay: remote_fs::SshExecutionOverlay::default(),
            items: vec![
                remote_fs::FsClipboardItem {
                    path: a.clone(),
                    is_dir: false,
                },
                remote_fs::FsClipboardItem {
                    path: b.clone(),
                    is_dir: false,
                },
            ],
            cut: true,
        });
        let batch = BatchIntent::Paste {
            src_endpoint: Box::new(remote_fs::FsEndpointSnapshot::new(
                FsLocation::Local,
                remote_fs::SshExecutionOverlay::default(),
            )),
            dst_endpoint: Box::new(remote_fs::FsEndpointSnapshot::new(
                FsLocation::Local,
                remote_fs::SshExecutionOverlay::default(),
            )),
            dst_dir: dst_dir.clone(),
            items: vec![(a.clone(), false), (b.clone(), false)],
            cut: true,
        };
        assert!(sidebar.request_batch(batch, true).is_none());
        let messages = poll_ops_until(&mut sidebar, |sidebar| !sidebar.has_pending_op());
        assert!(
            messages
                .iter()
                .any(|message| message.contains("2 项中 1 项失败") && message.contains("b.txt")),
            "messages: {messages:?}"
        );
        // cut = rename：a 移过去了；b 因目标已存在留在原地、目标内容未被覆盖。
        assert!(!a.exists() && dst_dir.join("a.txt").exists());
        assert_eq!(std::fs::read(dst_dir.join("b.txt")).unwrap(), b"old");
        assert!(b.exists());
        // 剪贴板收缩为失败项（便于换个目录重试）。
        let clipboard = sidebar
            .clipboard
            .as_ref()
            .expect("clipboard kept on partial");
        assert_eq!(clipboard.items.len(), 1);
        assert_eq!(clipboard.items[0].path, b);
    }
}
