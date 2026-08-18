use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
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
const SCAN_QUEUE_CAPACITY: usize = 8;
const SCAN_RESULT_CAPACITY: usize = 8;
const OP_QUEUE_CAPACITY: usize = 8;
const OP_RESULT_CAPACITY: usize = 8;

type ScanFn = dyn Fn(&Path) -> io::Result<DirectoryListing> + Send + Sync + 'static;

/// 一次目录扫描的后端。本机走注入的 [`ScanFn`]（生产是 scan_dir，测试是虚拟
/// 扫描器）；远程走 remote_fs 探针。主机列表随请求携带快照：排队期间配置
/// 被改动，也不影响这次扫描落到哪台主机。
#[derive(Clone, Debug)]
enum ScanBackend {
    Local,
    Remote(usize, Arc<Vec<RemoteHostConfig>>),
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

#[derive(Debug)]
struct DirectoryScanService {
    request_tx: Sender<ScanRequest>,
    /// Kept by the UI solely so a root-generation change can discard queued
    /// work. Workers receive from clones of this same bounded queue.
    request_rx: Receiver<ScanRequest>,
    result_rx: Receiver<ScanResult>,
}

impl DirectoryScanService {
    fn new(scanner: Arc<ScanFn>) -> io::Result<Self> {
        let (request_tx, request_rx) = crossbeam_channel::bounded(SCAN_QUEUE_CAPACITY);
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

        self.request_tx
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    "directory scan queue is busy; collapse and reopen the directory to retry"
                        .to_string()
                }
                TrySendError::Disconnected(_) => "directory scan workers stopped".to_string(),
            })
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

/// 操作 worker 的内部请求种类：除公开的文件操作外，还有切换位置时的
/// 起始目录解析（远程 home 目录不能阻塞 UI 线程）。
#[derive(Clone, Debug)]
enum OpRequestKind {
    StartDir,
    Fs(FsOpKind),
}

#[derive(Clone, Debug)]
struct FsOpRequest {
    generation: u64,
    location: FsLocation,
    hosts: Arc<Vec<RemoteHostConfig>>,
    kind: OpRequestKind,
    /// cut-paste 成功后才清空剪贴板；普通重命名不带这个标记。
    clear_clipboard_on_success: bool,
}

#[derive(Debug)]
struct FsOpResult {
    generation: u64,
    kind: OpRequestKind,
    clear_clipboard_on_success: bool,
    /// StartDir 成功时携带解析出的路径；文件操作成功为 Ok(None)。
    outcome: Result<Option<PathBuf>, String>,
}

/// 文件操作 worker：单线程串行执行（操作之间本就有先后语义，比如
/// cut-paste 不能被后续操作抢跑），有界队列与扫描服务同规格。
#[derive(Debug)]
struct FsOpService {
    request_tx: Sender<FsOpRequest>,
    result_rx: Receiver<FsOpResult>,
}

impl FsOpService {
    fn new() -> io::Result<Self> {
        let (request_tx, request_rx) = crossbeam_channel::bounded(OP_QUEUE_CAPACITY);
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
        self.request_tx
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    "file operation queue is busy; try again in a moment".to_string()
                }
                TrySendError::Disconnected(_) => "file operation worker stopped".to_string(),
            })
    }
}

fn op_worker(requests: Receiver<FsOpRequest>, results: Sender<FsOpResult>) {
    while let Ok(request) = requests.recv() {
        let outcome = execute_op(&request).map_err(|error| error.to_string());
        if results
            .send(FsOpResult {
                generation: request.generation,
                kind: request.kind,
                clear_clipboard_on_success: request.clear_clipboard_on_success,
                outcome,
            })
            .is_err()
        {
            break;
        }
    }
}

fn execute_op(request: &FsOpRequest) -> io::Result<Option<PathBuf>> {
    let location = &request.location;
    let hosts = request.hosts.as_slice();
    match &request.kind {
        OpRequestKind::StartDir => remote_fs::start_dir(location, hosts).map(Some),
        OpRequestKind::Fs(FsOpKind::CreateDir(path)) => {
            remote_fs::create_dir(location, hosts, path).map(|_| None)
        }
        OpRequestKind::Fs(FsOpKind::CreateFile(path)) => {
            remote_fs::create_file(location, hosts, path).map(|_| None)
        }
        OpRequestKind::Fs(FsOpKind::Delete(path)) => {
            remote_fs::delete(location, hosts, path).map(|_| None)
        }
        OpRequestKind::Fs(FsOpKind::Rename { src, dst }) => {
            remote_fs::rename(location, hosts, src, dst).map(|_| None)
        }
        OpRequestKind::Fs(FsOpKind::Copy { src, dst }) => {
            remote_fs::copy(location, hosts, src, dst).map(|_| None)
        }
    }
}

fn scan_worker(requests: Receiver<ScanRequest>, results: Sender<ScanResult>, scanner: Arc<ScanFn>) {
    while let Ok(request) = requests.recv() {
        let listing = match &request.backend {
            ScanBackend::Local => scanner(&request.path),
            ScanBackend::Remote(index, hosts) => {
                remote_fs::list_dir(&FsLocation::Remote(*index), hosts, &request.path).map(
                    |entries| DirectoryListing {
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
                    },
                )
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

/// 侧边栏状态
#[derive(Debug)]
pub struct Sidebar {
    pub visible: bool,
    pub width: f32,
    pub current_dir: PathBuf,
    pub root: Option<FileTreeNode>,
    pub selected_path: Option<PathBuf>,
    /// 当前侧边栏视图。
    pub view: SidebarView,
    /// 文件操作剪贴板（Copy/Cut → Paste），只允许同一位置内粘贴。
    pub clipboard: Option<remote_fs::FsClipboard>,
    scan_generation: u64,
    scan_service: Option<DirectoryScanService>,
    worker_error: Option<String>,
    worker_error_reported: bool,
    worker_disconnect_reported: bool,
    /// 文件树当前浏览的位置（本机或某台远程主机）。
    location: FsLocation,
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
            view: SidebarView::default(),
            clipboard: None,
            scan_generation: 0,
            scan_service,
            worker_error,
            worker_error_reported: false,
            worker_disconnect_reported: false,
            location: FsLocation::Local,
            remote_hosts: Arc::new(Vec::new()),
            op_service,
            op_worker_error,
            op_worker_error_reported: false,
            op_disconnect_reported: false,
            pending_ops: 0,
            start_dir_pending: false,
            location_error: None,
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
        self.start_root_scan()
    }

    /// 文件树当前浏览的位置（本机或某台远程主机）。
    pub fn location(&self) -> &FsLocation {
        &self.location
    }

    /// 起始目录解析失败（连不上主机等）时留在面板上的错误。
    pub fn location_error(&self) -> Option<&str> {
        self.location_error.as_deref()
    }

    /// 远程位置的起始目录还在 worker 上解析。
    pub fn is_starting(&self) -> bool {
        self.start_dir_pending
    }

    /// 是否有还在 worker 上执行的文件操作（含起始目录解析）。
    pub fn has_pending_op(&self) -> bool {
        self.pending_ops > 0
    }

    /// 同步远程主机配置快照。每帧调用、只在内容变化时替换，成本可忽略。
    pub fn set_remote_hosts(&mut self, hosts: &[RemoteHostConfig]) {
        if self.remote_hosts.as_slice() != hosts {
            self.remote_hosts = Arc::new(hosts.to_vec());
        }
    }

    /// 切换浏览位置：作废旧 generation（排队与在途的扫描/操作结果随之
    /// 全部丢弃）、清空树，再解析新位置的起始目录。本机当场解析；远程经
    /// 操作 worker 异步解析，期间面板显示"正在连接"。
    pub fn set_location(&mut self, location: FsLocation) -> Option<String> {
        if self.location == location {
            return None;
        }
        self.location = location;
        self.selected_path = None;
        self.location_error = None;
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.root = None;
        self.current_dir = PathBuf::new();
        self.start_dir_pending = false;
        if matches!(self.location, FsLocation::Local) {
            self.current_dir = remote_fs::start_dir(&self.location, &self.remote_hosts)
                .unwrap_or_else(|_| PathBuf::from("/"));
            self.start_root_scan()
        } else {
            self.start_dir_pending = true;
            self.enqueue_op(OpRequestKind::StartDir, false)
        }
    }

    /// UI 入口：请求一个文件变更操作（CreateDir/CreateFile/Delete/Rename/
    /// Copy）。cut-paste 传 clear_clipboard_on_success = true，成功后清空
    /// 剪贴板；失败则保留，方便用户换个目录重试。
    pub fn request_fs_op(
        &mut self,
        kind: FsOpKind,
        clear_clipboard_on_success: bool,
    ) -> Option<String> {
        self.enqueue_op(OpRequestKind::Fs(kind), clear_clipboard_on_success)
    }

    fn enqueue_op(
        &mut self,
        kind: OpRequestKind,
        clear_clipboard_on_success: bool,
    ) -> Option<String> {
        let Some(service) = &self.op_service else {
            self.start_dir_pending = false;
            return Some(
                self.op_worker_error
                    .clone()
                    .unwrap_or_else(|| "file operation worker is unavailable".to_string()),
            );
        };
        let request = FsOpRequest {
            generation: self.scan_generation,
            location: self.location.clone(),
            hosts: self.remote_hosts.clone(),
            kind,
            clear_clipboard_on_success,
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

    /// 收割操作 worker 的结果，不阻塞 UI 线程。StartDir 成功会落地
    /// current_dir 并发起根扫描；文件操作成功会安排受影响目录的重新扫描。
    /// 返回的字符串可直接进状态栏。
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
                Ok(result) => {
                    self.pending_ops = self.pending_ops.saturating_sub(1);
                    // set_location 会 bump generation：迟到结果一律丢弃。
                    if result.generation != self.scan_generation {
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
                                    self.location_error = Some(error.clone());
                                    messages.push(format!("无法进入该位置：{error}"));
                                }
                            }
                        }
                        OpRequestKind::Fs(kind) => match result.outcome {
                            Ok(_) => {
                                if result.clear_clipboard_on_success {
                                    self.clipboard = None;
                                }
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
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.op_disconnect_reported && self.pending_ops > 0 {
                        self.op_disconnect_reported = true;
                        self.pending_ops = 0;
                        self.start_dir_pending = false;
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

    /// 切换节点展开状态，并只在第一次展开（或错误后重试）时请求扫描。
    pub fn toggle_node(&mut self, path: &Path) -> Option<String> {
        let mut should_scan = false;
        if let Some(node) = self.find_node_mut(path) {
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

        if should_scan {
            self.enqueue_scan(path.to_path_buf(), false)
        } else {
            None
        }
    }

    pub fn show_more(&mut self, path: &Path) {
        if let Some(node) = self.find_node_mut(path) {
            node.visible_children = node
                .visible_children
                .saturating_add(DIRECTORY_PAGE_SIZE)
                .min(node.children.len());
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
            FsLocation::Remote(index) => ScanBackend::Remote(*index, self.remote_hosts.clone()),
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
    fn switching_to_an_unknown_remote_host_surfaces_an_error_without_network() {
        let scanner = Arc::new(|_: &Path| Ok(DirectoryListing::complete(vec![]))) as Arc<ScanFn>;
        let mut sidebar = Sidebar::with_scanner(PathBuf::from("/virtual/local"), scanner);
        // 没配置任何主机：Remote(0) 的起始目录解析会在 worker 上立即失败，
        // 错误要落在 location_error 上，而不是 panic、卡死或触网。
        assert!(sidebar.set_location(FsLocation::Remote(0)).is_none());
        assert!(sidebar.is_starting());
        assert!(sidebar.has_pending_op());

        let messages = poll_ops_until(&mut sidebar, |sidebar| sidebar.location_error().is_some());
        assert!(
            sidebar.location_error().is_some(),
            "messages so far: {messages:?}"
        );
        assert!(!sidebar.is_starting());
        assert!(!sidebar.has_pending_op());

        // 切回本机立即恢复，且以进程 cwd 为根。
        assert!(sidebar.set_location(FsLocation::Local).is_none());
        assert_eq!(sidebar.location(), &FsLocation::Local);
        assert_eq!(sidebar.current_dir, std::env::current_dir().unwrap());
        assert!(sidebar.location_error().is_none());
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
        sidebar.clipboard = Some(remote_fs::FsClipboard {
            loc: FsLocation::Local,
            path: renamed.clone(),
            is_dir: false,
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
}
