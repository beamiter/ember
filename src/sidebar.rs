use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

type ScanFn = dyn Fn(&Path) -> io::Result<DirectoryListing> + Send + Sync + 'static;

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

fn scan_worker(requests: Receiver<ScanRequest>, results: Sender<ScanResult>, scanner: Arc<ScanFn>) {
    while let Ok(request) = requests.recv() {
        let entries = scanner(&request.path)
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
    scan_generation: u64,
    scan_service: Option<DirectoryScanService>,
    worker_error: Option<String>,
    worker_error_reported: bool,
    worker_disconnect_reported: bool,
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
        let root = Some(Self::root_node(&current_dir));

        Self {
            visible: true,
            width: 200.0,
            current_dir,
            root,
            selected_path: None,
            view: SidebarView::default(),
            scan_generation: 0,
            scan_service,
            worker_error,
            worker_error_reported: false,
            worker_disconnect_reported: false,
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
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.root = Some(Self::root_node(&self.current_dir));
        if let Some(root) = &mut self.root {
            root.load_state = DirectoryLoadState::Loading;
        }
        self.enqueue_scan(self.current_dir.clone(), true)
    }

    fn enqueue_scan(&mut self, path: PathBuf, supersede_queued: bool) -> Option<String> {
        let result = match &self.scan_service {
            Some(service) => service.request(
                ScanRequest {
                    generation: self.scan_generation,
                    path: path.clone(),
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
}
