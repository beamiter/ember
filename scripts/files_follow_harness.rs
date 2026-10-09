#![allow(dead_code)]
// GTK/egui rendering, transport, and process observation are deterministic
// facades. Production completion dispatch, authority gates, first-list worker,
// synchronous publication preparation, and manual scan admission are extracted.
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteHostConfig {
    host: String,
}
mod config {
    use super::*;
    pub fn remote_host_runtime_label(p: &RemoteHostConfig) -> String {
        p.host.clone()
    }
}
mod jterm_core {
    pub mod review_input {
        pub fn safe_inline_display(s: &str, _: usize) -> String {
            s.into()
        }
    }
}
mod remote_fs {
    use super::*;
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum FsLocation {
        Local,
        Remote(usize),
        Transient(RemoteHostConfig),
    }
    impl FsLocation {
        pub fn label(&self, _: &[RemoteHostConfig]) -> String {
            format!("{self:?}")
        }
    }
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct SshExecutionOverlay {
        pub control_path: Option<String>,
    }
    #[derive(Debug)]
    pub struct Entry {
        pub name: String,
        pub path: PathBuf,
        pub is_dir: bool,
    }
    pub fn validate_execution_endpoint(
        _: &FsLocation,
        _: &[RemoteHostConfig],
        _: &SshExecutionOverlay,
    ) -> std::io::Result<()> {
        Ok(())
    }
    pub fn cancelled_error() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled")
    }
    pub fn start_dir_with_overlay_cancellable(
        location: &FsLocation,
        hosts: &[RemoteHostConfig],
        overlay: &SshExecutionOverlay,
        cancellation: Arc<AtomicBool>,
    ) -> std::io::Result<PathBuf> {
        if cancellation.load(Ordering::SeqCst) {
            return Err(cancelled_error());
        }
        start_dir_with_overlay(location, hosts, overlay)
    }
    pub fn start_dir_with_overlay(
        _: &FsLocation,
        _: &[RemoteHostConfig],
        _: &SshExecutionOverlay,
    ) -> std::io::Result<PathBuf> {
        HOME_CALLS.with(|c| c.set(c.get() + 1));
        if CANCEL_PHASE.with(|c| c.get()) == 1 {
            ACTIVE_CANCEL.with(|c| c.borrow().as_ref().unwrap().store(true, Ordering::SeqCst));
        }
        Ok("/remote/home".into())
    }
    pub fn list_dir_with_overlay_and_hidden_control(
        _: &FsLocation,
        _: &[RemoteHostConfig],
        _: &SshExecutionOverlay,
        _: &Path,
        _: bool,
        _: Arc<AtomicBool>,
    ) -> std::io::Result<Vec<Entry>> {
        LIST_CALLS.with(|c| c.set(c.get() + 1));
        if CANCEL_PHASE.with(|c| c.get()) == 2 {
            ACTIVE_CANCEL.with(|c| c.borrow().as_ref().unwrap().store(true, Ordering::SeqCst));
        }
        Ok(vec![])
    }
}
use remote_fs::FsLocation;
mod sidebar {
    use super::*;
    pub const MAX_DIRECTORY_ENTRIES: usize = 4096;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SidebarView {
        Files,
        Commands,
    }
    #[derive(Clone, Debug)]
    pub struct FilesIntentContext {
        generation: u64,
    }
    pub struct FileEntry {
        name: String,
        path: PathBuf,
        is_dir: bool,
    }
    pub struct DirectoryListing {
        entries: Vec<FileEntry>,
        truncated: bool,
    }
    pub struct ScanFailure {
        message: String,
    }
    pub struct ScanResult {
        generation: u64,
        revision: u64,
        path: PathBuf,
        queue_delay: Duration,
        run_time: Duration,
        entries: Result<DirectoryListing, ScanFailure>,
    }
    pub struct ScanTiming {
        path: PathBuf,
        queue_delay: Duration,
        run_time: Duration,
    }
    #[derive(Clone)]
    pub enum NavigationCause {
        Ordinary,
    }
    #[derive(Clone)]
    pub struct PendingEndpoint {
        location: FsLocation,
        overlay: remote_fs::SshExecutionOverlay,
        home: PathBuf,
    }
    #[derive(Clone)]
    pub struct PendingNavigation {
        generation: u64,
        origin: PathBuf,
        target: PathBuf,
        cause: NavigationCause,
        endpoint_commit: Option<PendingEndpoint>,
    }
    pub fn validate_remote_navigation_text(s: &str) -> Result<PathBuf, String> {
        Ok(s.into())
    }
    pub struct Sidebar {
        pub visible: bool,
        pub view: SidebarView,
        pub selected_path: Option<PathBuf>,
        pub selection: BTreeMap<PathBuf, bool>,
        pub filter_open: bool,
        pub filter: String,
        pub current_dir: PathBuf,
        pub location: FsLocation,
        pub overlay: remote_fs::SshExecutionOverlay,
        pub scan_generation: u64,
        pub remote_hosts: Vec<RemoteHostConfig>,
        pub show_hidden: bool,
        pub pending_navigation: Option<PendingNavigation>,
        pub last_scan_timing: Option<ScanTiming>,
        pub latest_scan_revisions: BTreeMap<PathBuf, u64>,
        pub scan_cancel_tokens: BTreeMap<PathBuf, ()>,
        pub start_dir_pending: bool,
        pub pending_location_probe: Option<()>,
        pub failure_states: BTreeMap<PathBuf, ()>,
        pub commits: usize,
        pub busy: bool,
        pub last_truncated: bool,
    }
    impl Sidebar {
        pub fn new() -> Self {
            Self {
                visible: true,
                view: SidebarView::Files,
                selected_path: None,
                selection: BTreeMap::new(),
                filter_open: false,
                filter: String::new(),
                current_dir: "/local/kept".into(),
                location: FsLocation::Local,
                overlay: Default::default(),
                scan_generation: 9,
                remote_hosts: vec![],
                show_hidden: false,
                pending_navigation: None,
                last_scan_timing: None,
                latest_scan_revisions: BTreeMap::new(),
                scan_cancel_tokens: BTreeMap::new(),
                start_dir_pending: false,
                pending_location_probe: None,
                failure_states: BTreeMap::new(),
                commits: 0,
                busy: false,
                last_truncated: false,
            }
        }
        pub fn files_intent_context(&self) -> FilesIntentContext {
            FilesIntentContext {
                generation: self.scan_generation,
            }
        }
        pub fn files_intent_is_current(&self, c: &FilesIntentContext) -> bool {
            c.generation == self.scan_generation
        }
        pub fn files_user_intent_generation(&self) -> u64 {
            self.scan_generation
        }
        pub fn has_pending_op(&self) -> bool {
            self.busy
        }
        pub fn prepare_endpoint_switch(&mut self) {
            self.scan_generation += 1;
            self.pending_navigation = None;
        }
        pub fn commit_navigation(&mut self, p: PendingNavigation, l: DirectoryListing) {
            self.current_dir = p.target;
            if let Some(e) = p.endpoint_commit {
                self.location = e.location;
                self.overlay = e.overlay;
            }
            self.commits += 1;
            self.last_truncated = l.truncated;
        }
        pub fn clear_scan_failure(&mut self, _: &Path) {}
        pub fn record_scan_failure(&mut self, _: &Path, _: &ScanFailure) {}
        pub fn set_location_error(&mut self, _: String) {}
        pub fn finish_probed_execution_overlay(
            &mut self,
            overlay: remote_fs::SshExecutionOverlay,
            result: Result<PathBuf, String>,
        ) -> Result<(), String> {
            result?;
            self.overlay = overlay;
            Ok(())
        }
        // @SYNC_COMMIT@
        pub fn stage_manual(&mut self) {
            let root = PathBuf::from("/manual");
            self.latest_scan_revisions.insert(root.clone(), 1);
            self.pending_navigation = Some(PendingNavigation {
                generation: self.scan_generation,
                origin: self.current_dir.clone(),
                target: root,
                cause: NavigationCause::Ordinary,
                endpoint_commit: None,
            });
        }
        pub fn finish_manual(&mut self) {
            let result = ScanResult {
                generation: self.scan_generation,
                revision: 1,
                path: "/manual".into(),
                queue_delay: Duration::ZERO,
                run_time: Duration::ZERO,
                entries: Ok(DirectoryListing {
                    entries: vec![],
                    truncated: false,
                }),
            };
            let mut errors = Vec::new();
            for result in [result] {
                // @MANUAL_ADMISSION@
            }
        }
    }
}
use sidebar::{FilesIntentContext, Sidebar, SidebarView};
mod ssh_files_follow {
    use super::*;
    #[derive(Clone, Debug, PartialEq, Eq)]
    // @OBSERVATION_KEY@
    #[derive(Clone, Debug, PartialEq, Eq)]
    // @OBSERVATION@
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    // @FOLLOW_COMMIT@
    #[derive(Clone, Debug)]
    pub struct TargetAuthority(RemoteHostConfig);
    impl TargetAuthority {
        pub fn profile(&self) -> &RemoteHostConfig {
            &self.0
        }
        pub fn current_location(
            &self,
            profile: &RemoteHostConfig,
            _: &[RemoteHostConfig],
        ) -> Option<FsLocation> {
            (&self.0 == profile).then(|| FsLocation::Transient(profile.clone()))
        }
    }
    #[derive(Clone, Debug)]
    // @PENDING_PROBE@
    #[derive(Clone, Debug, PartialEq, Eq)]
    // @SIDEBAR_UI@
    // @SIDEBAR_UI_IMPL@
    #[derive(Debug)]
    // @PROBE_SNAPSHOT@
    #[derive(Debug)]
    // @PROBE_RESULT@
    // @RESULT_CURRENT@
    // @FILES_AUTHORITY@
    // @OBSERVATION_CURRENT@
    // @PROCESS_CHANGED@
    // @STALE_REARM@
    pub struct State {
        pub pending: Option<PendingProbe>,
        pub results: VecDeque<ProbeResult>,
    }
    impl State {
        pub fn try_result(&mut self) -> Option<ProbeResult> {
            self.results.pop_front()
        }
        pub fn sync_observation(&mut self, _: &Observation) -> u64 {
            3
        }
        pub fn sync_sidebar_ui(&mut self, _: &SidebarUiSnapshot) -> u64 {
            5
        }
        pub fn rearm_after_stale_probe(&mut self, _: &ObservationKey) {}
        pub fn clear_failure(&mut self) {}
        pub fn record_failure(&mut self, _: ObservationKey) {}
    }
    pub fn setup(sidebar: &Sidebar) -> (State, Observation) {
        let p = RemoteHostConfig {
            host: "observed".into(),
        };
        let key = ObservationKey {
            session_id: "pane-a".into(),
            shell_pid: 123,
            argv: vec!["ssh".into(), p.host.clone()],
            control_path: None,
        };
        let overlay = remote_fs::SshExecutionOverlay::default();
        let observation = Observation::Target {
            key: key.clone(),
            profile: Box::new(p.clone()),
            overlay: overlay.clone(),
        };
        let pending = PendingProbe {
            cancellation: Arc::new(AtomicBool::new(false)),
            show_hidden: false,
            token: 7,
            observation_epoch: 3,
            active_session_epoch: 11,
            files_user_intent_generation: sidebar.files_user_intent_generation(),
            sidebar_ui_epoch: 5,
            key,
            authority: TargetAuthority(p.clone()),
            profile: p,
            overlay,
            commit: FollowCommit::ReplaceLocation,
            files_context: sidebar.files_intent_context(),
            root: sidebar.current_dir.clone(),
            sidebar_ui: SidebarUiSnapshot::capture(sidebar, false),
        };
        (
            State {
                pending: Some(pending),
                results: VecDeque::new(),
            },
            observation,
        )
    }
    pub fn run_worker(
        list_root: bool,
        cancellation: Arc<AtomicBool>,
    ) -> std::io::Result<ProbeSnapshot> {
        let authority = TargetAuthority(RemoteHostConfig {
            host: "observed".into(),
        });
        let overlay = remote_fs::SshExecutionOverlay::default();
        let show_hidden = false;
        ACTIVE_CANCEL.with(|c| *c.borrow_mut() = Some(cancellation.clone()));
        // @WORKER@

    }
}
struct AppConfig {
    remote_hosts: Vec<RemoteHostConfig>,
}
struct TerminalApp {
    sidebar: Sidebar,
    ssh_files_follow: ssh_files_follow::State,
    active_session_epoch: u64,
    sidebar_name_dialog: Option<()>,
    sidebar_delete_dialog: Option<()>,
    config: AppConfig,
    observation: ssh_files_follow::Observation,
    status: Vec<String>,
}
impl TerminalApp {
    fn active_ssh_files_observation(&self) -> ssh_files_follow::Observation {
        self.observation.clone()
    }
    fn set_status_for(&mut self, message: impl Into<String>, _: Duration) {
        self.status.push(message.into());
    }
    // @BUMP_FOCUS@
    fn finish_results(&mut self) {
        // @COMPLETION_LOOP@
    }
}
fn setup() -> TerminalApp {
    let sidebar = Sidebar::new();
    let (state, observation) = ssh_files_follow::setup(&sidebar);
    TerminalApp {
        sidebar,
        ssh_files_follow: state,
        active_session_epoch: 11,
        sidebar_name_dialog: None,
        sidebar_delete_dialog: None,
        config: AppConfig {
            remote_hosts: vec![],
        },
        observation,
        status: vec![],
    }
}
fn deliver(app: &mut TerminalApp, outcome: Result<ssh_files_follow::ProbeSnapshot, String>) {
    app.ssh_files_follow
        .results
        .push_back(ssh_files_follow::ProbeResult { token: 7, outcome });
    app.finish_results();
}
fn listing() -> Result<ssh_files_follow::ProbeSnapshot, String> {
    Ok(ssh_files_follow::ProbeSnapshot {
        home: "/remote/home".into(),
        listing: Some(vec![]),
    })
}
#[test]
fn combined_listing_commits_once_without_a_second_scan() {
    let mut app = setup();
    deliver(&mut app, listing());
    assert_eq!(app.sidebar.current_dir, PathBuf::from("/remote/home"));
    assert_eq!(app.sidebar.commits, 1);
    assert!(app.sidebar.pending_navigation.is_none());
}
#[test]
fn focus_change_during_listing_preserves_old_tree() {
    let mut app = setup();
    app.bump_active_session_epoch();
    deliver(&mut app, listing());
    assert_eq!(app.sidebar.commits, 0);
    assert!(app.status.is_empty());
}
#[test]
fn source_exit_discards_success_and_error() {
    for result in [listing(), Err("old failure".into())] {
        let mut app = setup();
        app.observation = ssh_files_follow::Observation::None;
        deliver(&mut app, result);
        assert_eq!(app.sidebar.commits, 0);
        assert!(app.status.is_empty());
    }
}
#[test]
fn argv_change_during_listing_preserves_old_tree() {
    let mut app = setup();
    if let ssh_files_follow::Observation::Target { key, .. } = &mut app.observation {
        key.argv.push("-v".into());
    }
    deliver(&mut app, listing());
    assert_eq!(app.sidebar.commits, 0);
}
#[test]
fn cancellation_cannot_revive_at_the_same_focus() {
    let mut app = setup();
    app.ssh_files_follow
        .pending
        .as_ref()
        .unwrap()
        .cancellation
        .store(true, Ordering::SeqCst);
    deliver(&mut app, listing());
    assert_eq!(app.sidebar.commits, 0);
}
#[test]
fn file_operation_during_listing_preserves_old_tree() {
    let mut app = setup();
    app.sidebar.busy = true;
    deliver(&mut app, listing());
    assert_eq!(app.sidebar.commits, 0);
}
#[test]
fn manual_navigation_survives_source_focus_change() {
    let mut app = setup();
    app.sidebar.stage_manual();
    app.bump_active_session_epoch();
    app.sidebar.finish_manual();
    assert_eq!(app.sidebar.current_dir, PathBuf::from("/manual"));
    assert_eq!(app.sidebar.commits, 1);
}
#[test]
fn same_namespace_rebind_preserves_root_after_gate() {
    let mut app = setup();
    let p = app.ssh_files_follow.pending.as_mut().unwrap();
    p.commit = ssh_files_follow::FollowCommit::RebindCurrentOverlay;
    p.overlay.control_path = Some("/tmp/current".into());
    if let ssh_files_follow::Observation::Target { overlay, .. } = &mut app.observation {
        *overlay = p.overlay.clone();
    }
    deliver(
        &mut app,
        Ok(ssh_files_follow::ProbeSnapshot {
            home: "/remote/home".into(),
            listing: None,
        }),
    );
    assert_eq!(app.sidebar.current_dir, PathBuf::from("/local/kept"));
    assert_eq!(
        app.sidebar.overlay.control_path.as_deref(),
        Some("/tmp/current")
    );
    assert_eq!(app.sidebar.commits, 0);
}
#[test]
fn stale_same_namespace_rebind_does_not_change_overlay() {
    let mut app = setup();
    app.ssh_files_follow.pending.as_mut().unwrap().commit =
        ssh_files_follow::FollowCommit::RebindCurrentOverlay;
    app.bump_active_session_epoch();
    deliver(
        &mut app,
        Ok(ssh_files_follow::ProbeSnapshot {
            home: "/remote/home".into(),
            listing: None,
        }),
    );
    assert_eq!(app.sidebar.overlay.control_path, None);
    assert!(app.status.is_empty());
}
thread_local! {static HOME_CALLS:Cell<usize>=const{Cell::new(0)};static LIST_CALLS:Cell<usize>=const{Cell::new(0)};static CANCEL_PHASE:Cell<usize>=const{Cell::new(0)};static ACTIVE_CANCEL:RefCell<Option<Arc<AtomicBool>>>=const{RefCell::new(None)};}
fn reset_worker(phase: usize) {
    HOME_CALLS.with(|c| c.set(0));
    LIST_CALLS.with(|c| c.set(0));
    CANCEL_PHASE.with(|c| c.set(phase));
}
#[test]
fn original_probe_contains_first_listing() {
    reset_worker(0);
    let r = ssh_files_follow::run_worker(true, Arc::new(AtomicBool::new(false))).unwrap();
    assert!(r.listing.is_some());
    LIST_CALLS.with(|c| assert_eq!(c.get(), 1));
}
#[test]
fn same_namespace_probe_skips_listing() {
    reset_worker(0);
    let r = ssh_files_follow::run_worker(false, Arc::new(AtomicBool::new(false))).unwrap();
    assert!(r.listing.is_none());
    LIST_CALLS.with(|c| assert_eq!(c.get(), 0));
}
#[test]
fn cancelled_probe_never_starts_transport() {
    reset_worker(0);
    assert!(ssh_files_follow::run_worker(true, Arc::new(AtomicBool::new(true))).is_err());
    HOME_CALLS.with(|c| assert_eq!(c.get(), 0));
}
#[test]
fn cancellation_during_home_skips_listing() {
    reset_worker(1);
    assert!(ssh_files_follow::run_worker(true, Arc::new(AtomicBool::new(false))).is_err());
    LIST_CALLS.with(|c| assert_eq!(c.get(), 0));
}
#[test]
fn cancellation_during_listing_discards_result() {
    reset_worker(2);
    assert!(ssh_files_follow::run_worker(true, Arc::new(AtomicBool::new(false))).is_err());
    LIST_CALLS.with(|c| assert_eq!(c.get(), 1));
}
