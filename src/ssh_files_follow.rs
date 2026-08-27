//! Follow a hand-launched interactive SSH process into its remote Files tree.
//!
//! The trust boundary is intentionally narrow: observation starts at the
//! active session's real `/proc` process argv. Terminal text, OSC command
//! markers, titles, and cwd reports are never accepted as evidence of SSH.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender};
use jterm_core::jsh_remote::{ObservedSshTarget, RemoteHostConfig};
use jterm_core::process::ObservedSshCommand;

use crate::remote_fs::{self, FsLocation};
use crate::session::{Session, SessionPurpose};
use crate::sidebar::{FilesIntentContext, Sidebar, SidebarView};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservationKey {
    pub session_id: String,
    pub shell_pid: i32,
    pub argv: Vec<String>,
    pub control_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Observation {
    None,
    Unsupported {
        key: ObservationKey,
        reason: &'static str,
    },
    Target {
        key: ObservationKey,
        profile: Box<RemoteHostConfig>,
        overlay: remote_fs::SshExecutionOverlay,
    },
}

/// Classify only argv that the caller has already obtained from `/proc`.
/// Kept private outside this module so UI/OSC strings cannot accidentally be
/// wired into the authority path later.
fn classify_observed_command(
    session_id: String,
    shell_pid: i32,
    command: ObservedSshCommand,
) -> Observation {
    let ObservedSshCommand {
        argv,
        target,
        reusable_control_path,
    } = command;
    match target {
        ObservedSshTarget::NotSsh => Observation::None,
        ObservedSshTarget::Unsupported(reason) => Observation::Unsupported {
            key: ObservationKey {
                session_id,
                shell_pid,
                argv,
                control_path: reusable_control_path,
            },
            reason,
        },
        ObservedSshTarget::Target(mut profile) => {
            let (base_args, explicit_control_path) =
                remote_fs::split_ssh_control_path_args(&profile.ssh_args);
            if explicit_control_path.is_some()
                && reusable_control_path.is_some()
                && explicit_control_path != reusable_control_path
            {
                return Observation::Unsupported {
                    key: ObservationKey {
                        session_id,
                        shell_pid,
                        argv,
                        control_path: reusable_control_path,
                    },
                    reason: "the observed SSH command has conflicting ControlPath values",
                };
            }
            profile.ssh_args = base_args;
            let control_path = explicit_control_path.or(reusable_control_path);
            Observation::Target {
                key: ObservationKey {
                    session_id,
                    shell_pid,
                    argv,
                    control_path: control_path.clone(),
                },
                profile: Box::new(profile),
                overlay: remote_fs::SshExecutionOverlay::from_control_path(control_path),
            }
        }
    }
}

/// Observe the currently focused terminal session. Ember-created remote tabs
/// are exact-argv `EphemeralCommand` sessions, so they are explicitly outside
/// this feature; ordinary interactive sessions are covered regardless of tab,
/// split-pane, block-mode, or sidebar presentation.
pub(crate) fn observe_session(session: &Session) -> Observation {
    if !purpose_is_observable(session.purpose) {
        return Observation::None;
    }
    let shell_pid = session.get_shell_pid();
    let Some(command) = jterm_core::process::observed_ssh_command_via_stat(shell_pid) else {
        return Observation::None;
    };
    classify_observed_command(session.metadata.session_id.clone(), shell_pid, command)
}

fn purpose_is_observable(purpose: SessionPurpose) -> bool {
    purpose == SessionPurpose::Interactive
}

fn same_ssh_transport(candidate: &RemoteHostConfig, observed: &RemoteHostConfig) -> bool {
    let candidate_args = remote_fs::split_ssh_control_path_args(&candidate.ssh_args).0;
    !candidate.docker
        && !observed.docker
        && candidate.host == observed.host
        && candidate.user == observed.user
        && candidate_args == observed.ssh_args
}

fn unique_saved_transport_index(
    observed: &RemoteHostConfig,
    hosts: &[RemoteHostConfig],
) -> Option<usize> {
    let mut matches = hosts
        .iter()
        .take(crate::config::MAX_REMOTE_HOSTS)
        .enumerate()
        .filter_map(|(index, candidate)| {
            (crate::config::validate_remote_host_at(hosts, index).is_ok()
                && same_ssh_transport(candidate, observed))
            .then_some(index)
        });
    let index = matches.next()?;
    matches.next().is_none().then_some(index)
}

fn unique_exact_saved_index(
    expected: &RemoteHostConfig,
    hosts: &[RemoteHostConfig],
) -> Option<usize> {
    let mut matches = hosts
        .iter()
        .take(crate::config::MAX_REMOTE_HOSTS)
        .enumerate()
        .filter_map(|(index, candidate)| {
            (candidate == expected && crate::config::validate_remote_host_at(hosts, index).is_ok())
                .then_some(index)
        });
    let index = matches.next()?;
    matches.next().is_none().then_some(index)
}

/// Immutable identity selected before the probe. A managed choice carries the
/// complete validated profile, not its reorderable index; the final callback
/// must still find that exact profile and prove it remains the sole saved
/// transport match. A transient choice never starts following a later config
/// edit merely because it happened while the worker was running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetAuthority {
    Saved(RemoteHostConfig),
    Transient(RemoteHostConfig),
}

impl TargetAuthority {
    pub fn profile(&self) -> &RemoteHostConfig {
        match self {
            Self::Saved(profile) | Self::Transient(profile) => profile,
        }
    }

    pub fn current_location(
        &self,
        observed: &RemoteHostConfig,
        hosts: &[RemoteHostConfig],
    ) -> Option<FsLocation> {
        match self {
            Self::Saved(expected) => {
                let exact = unique_exact_saved_index(expected, hosts)?;
                (unique_saved_transport_index(observed, hosts) == Some(exact))
                    .then_some(FsLocation::Remote(exact))
            }
            Self::Transient(profile) => (profile == observed
                && crate::config::validate_remote_host(profile).is_ok())
            .then(|| FsLocation::Transient(profile.clone())),
        }
    }
}

pub(crate) fn target_authority(
    observed: &RemoteHostConfig,
    hosts: &[RemoteHostConfig],
) -> TargetAuthority {
    match unique_saved_transport_index(observed, hosts) {
        Some(index) => TargetAuthority::Saved(hosts[index].clone()),
        None => TargetAuthority::Transient(observed.clone()),
    }
}

pub(crate) fn location_matches_observed(
    location: &FsLocation,
    hosts: &[RemoteHostConfig],
    observed: &RemoteHostConfig,
) -> bool {
    match location {
        FsLocation::Local => false,
        FsLocation::Remote(index) => crate::config::validate_remote_host_at(hosts, *index)
            .is_ok_and(|candidate| same_ssh_transport(candidate, observed)),
        FsLocation::Transient(candidate) => same_ssh_transport(candidate, observed),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FollowCommit {
    /// A first entry into this namespace installs the probed home as the root.
    ReplaceLocation,
    /// The visible tree already belongs to the same stable namespace. Only
    /// its execution overlay is rebound after the new socket probe succeeds.
    RebindCurrentOverlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SameTargetAction {
    DifferentLocation,
    RevealExisting,
    ProbeOverlayUpgrade,
}

pub(crate) fn same_target_action(
    same_target_with_valid_tree: bool,
    current_overlay: &remote_fs::SshExecutionOverlay,
    observed_overlay: &remote_fs::SshExecutionOverlay,
) -> SameTargetAction {
    if !same_target_with_valid_tree {
        SameTargetAction::DifferentLocation
    } else if current_overlay == observed_overlay {
        SameTargetAction::RevealExisting
    } else {
        SameTargetAction::ProbeOverlayUpgrade
    }
}

pub(crate) fn same_frame_files_intent_suppresses_new_observation(
    frame_start_generation: u64,
    current_generation: u64,
) -> bool {
    current_generation != frame_start_generation
}

pub(crate) fn ongoing_files_surface_is_user_intent(
    popup_open: bool,
    os_file_drag_hover: bool,
) -> bool {
    popup_open || os_file_drag_hover
}

pub(crate) fn poll_allowed_after_shell_exit(
    shell_exited: bool,
    exiting_session_id: &str,
    active_session_id_after_close: Option<&str>,
) -> bool {
    !shell_exited
        || active_session_id_after_close.is_some_and(|active| active != exiting_session_id)
}

#[derive(Clone, Debug)]
pub(crate) struct PendingProbe {
    pub token: u64,
    pub observation_epoch: u64,
    pub active_session_epoch: u64,
    pub files_user_intent_generation: u64,
    pub sidebar_ui_epoch: u64,
    pub key: ObservationKey,
    pub authority: TargetAuthority,
    pub profile: RemoteHostConfig,
    pub overlay: remote_fs::SshExecutionOverlay,
    pub commit: FollowCommit,
    pub files_context: FilesIntentContext,
    pub root: PathBuf,
    pub sidebar_ui: SidebarUiSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SidebarUiSnapshot {
    visible: bool,
    view: SidebarView,
    selected_path: Option<PathBuf>,
    selection: BTreeMap<PathBuf, bool>,
    filter_open: bool,
    filter: String,
    files_dialog_open: bool,
}

impl SidebarUiSnapshot {
    pub fn capture(sidebar: &Sidebar, files_dialog_open: bool) -> Self {
        Self {
            visible: sidebar.visible,
            view: sidebar.view,
            selected_path: sidebar.selected_path.clone(),
            selection: sidebar.selection.clone(),
            filter_open: sidebar.filter_open,
            filter: sidebar.filter.clone(),
            files_dialog_open,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProbeResult {
    pub token: u64,
    pub outcome: Result<PathBuf, String>,
}

#[derive(Debug)]
pub(crate) struct State {
    pub pending: Option<PendingProbe>,
    pub handled_observation: Option<ObservationKey>,
    failed_observation: Option<ObservationKey>,
    retry_requested: bool,
    last_observation: Option<ObservationKey>,
    observation_epoch: u64,
    last_sidebar_ui: Option<SidebarUiSnapshot>,
    sidebar_ui_epoch: u64,
    next_token: u64,
    result_tx: Sender<ProbeResult>,
    result_rx: Receiver<ProbeResult>,
}

impl Default for State {
    fn default() -> Self {
        let (result_tx, result_rx) = crossbeam_channel::unbounded();
        Self {
            pending: None,
            handled_observation: None,
            failed_observation: None,
            retry_requested: false,
            last_observation: None,
            observation_epoch: 0,
            last_sidebar_ui: None,
            sidebar_ui_epoch: 0,
            next_token: 0,
            result_tx,
            result_rx,
        }
    }
}

impl State {
    pub fn sync_observation(&mut self, observation: &Observation) -> u64 {
        let current = match observation {
            Observation::None => None,
            Observation::Unsupported { key, .. } | Observation::Target { key, .. } => {
                Some(key.clone())
            }
        };
        if current != self.last_observation {
            self.last_observation = current;
            self.failed_observation = None;
            self.retry_requested = false;
            self.observation_epoch = self.observation_epoch.wrapping_add(1);
            if self.observation_epoch == 0 {
                self.observation_epoch = 1;
            }
        }
        self.observation_epoch
    }

    pub fn mark_observation_absent(&mut self) {
        self.handled_observation = None;
        self.failed_observation = None;
        self.retry_requested = false;
    }

    pub fn sync_sidebar_ui(&mut self, current: &SidebarUiSnapshot) -> u64 {
        if self.last_sidebar_ui.as_ref() != Some(current) {
            self.last_sidebar_ui = Some(current.clone());
            self.sidebar_ui_epoch = self.sidebar_ui_epoch.wrapping_add(1);
            if self.sidebar_ui_epoch == 0 {
                self.sidebar_ui_epoch = 1;
            }
        }
        self.sidebar_ui_epoch
    }

    pub fn was_handled(&self, key: &ObservationKey) -> bool {
        self.handled_observation.as_ref() == Some(key)
    }

    pub fn mark_handled(&mut self, key: ObservationKey) {
        self.handled_observation = Some(key);
    }

    pub fn record_failure(&mut self, key: ObservationKey) {
        self.failed_observation = Some(key);
        self.retry_requested = false;
    }

    /// Persistent UI retry is available only while the exact failed process
    /// observation is still live. A stale failure must not become a button
    /// that can authorize a different session or argv.
    pub fn retry_available_for_observation(&self, observation: &Observation) -> bool {
        matches!(
            observation,
            Observation::Target { key, .. }
                if self.failed_observation.as_ref() == Some(key)
        )
    }

    pub fn request_retry(&mut self) {
        if self.failed_observation.is_some() {
            self.retry_requested = true;
        }
    }

    pub fn retry_requested_for(&self, key: &ObservationKey) -> bool {
        self.retry_requested && self.failed_observation.as_ref() == Some(key)
    }

    pub fn cancel_retry_for(&mut self, key: &ObservationKey) {
        if self.retry_requested_for(key) {
            self.retry_requested = false;
        }
    }

    /// A Files operation/navigation/dialog is an explicit user decision that
    /// consumes this process observation. Cancelling an armed Retry preserves
    /// the failed record for the persistent control, but never clears dedupe
    /// and accidentally turns the same argv into a new automatic follow.
    pub fn suppress_for_files_intent(&mut self, key: &ObservationKey) {
        self.mark_handled(key.clone());
        self.cancel_retry_for(key);
    }

    pub fn consume_retry_for(&mut self, key: &ObservationKey) {
        if self.retry_requested_for(key) {
            self.retry_requested = false;
            self.failed_observation = None;
        }
    }

    pub fn clear_failure(&mut self) {
        self.failed_observation = None;
        self.retry_requested = false;
    }

    pub fn rearm_after_stale_probe(&mut self, key: &ObservationKey) {
        if self.handled_observation.as_ref() == Some(key) {
            self.handled_observation = None;
        }
    }

    pub fn begin_probe(
        &mut self,
        mut pending: PendingProbe,
        repaint: egui::Context,
    ) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("an SSH Files probe is already running".to_string());
        }
        self.next_token = self.next_token.wrapping_add(1);
        if self.next_token == 0 {
            self.next_token = 1;
        }
        let token = self.next_token;
        pending.token = token;
        let authority = pending.authority.clone();
        let overlay = pending.overlay.clone();
        let results = self.result_tx.clone();
        std::thread::Builder::new()
            .name("ember-ssh-files-probe".to_string())
            .spawn(move || {
                let outcome = remote_fs::start_dir_with_overlay(
                    &FsLocation::Transient(authority.profile().clone()),
                    &[],
                    &overlay,
                )
                .map_err(|error| error.to_string());
                let _ = results.send(ProbeResult { token, outcome });
                repaint.request_repaint();
            })
            .map_err(|error| format!("could not start SSH Files probe: {error}"))?;
        self.pending = Some(pending);
        Ok(())
    }

    pub fn try_result(&self) -> Option<ProbeResult> {
        self.result_rx.try_recv().ok()
    }
}

/// Full commit gate, split out so all negative dimensions can be unit tested
/// without a PTY or network connection.
#[allow(clippy::too_many_arguments)] // Keep each independent authority dimension explicit.
pub(crate) fn result_is_current(
    pending: &PendingProbe,
    observation: &Observation,
    observation_epoch: u64,
    active_session_epoch: u64,
    files_user_intent_generation: u64,
    sidebar_ui_epoch: u64,
    files_context_current: bool,
    current_root: &std::path::Path,
    sidebar_ui: &SidebarUiSnapshot,
) -> bool {
    let same_observation = matches!(
        observation,
        Observation::Target {
            key,
            profile,
            overlay,
        } if key == &pending.key
            && profile.as_ref() == &pending.profile
            && overlay == &pending.overlay
    );
    same_observation
        && observation_epoch == pending.observation_epoch
        && active_session_epoch == pending.active_session_epoch
        && files_authority_is_current(
            pending,
            files_user_intent_generation,
            sidebar_ui_epoch,
            files_context_current,
            current_root,
            sidebar_ui,
        )
}

pub(crate) fn files_authority_is_current(
    pending: &PendingProbe,
    files_user_intent_generation: u64,
    sidebar_ui_epoch: u64,
    files_context_current: bool,
    current_root: &std::path::Path,
    sidebar_ui: &SidebarUiSnapshot,
) -> bool {
    files_user_intent_generation == pending.files_user_intent_generation
        && sidebar_ui_epoch == pending.sidebar_ui_epoch
        && files_context_current
        && current_root == pending.root
        && sidebar_ui == &pending.sidebar_ui
}

pub(crate) fn pending_observation_is_current(
    pending: &PendingProbe,
    observation: &Observation,
    observation_epoch: u64,
    active_session_epoch: u64,
) -> bool {
    matches!(
        observation,
        Observation::Target {
            key,
            profile,
            overlay,
        } if key == &pending.key
            && profile.as_ref() == &pending.profile
            && overlay == &pending.overlay
    ) && observation_epoch == pending.observation_epoch
        && active_session_epoch == pending.active_session_epoch
}

pub(crate) fn process_authority_changed_since_probe(
    pending: &PendingProbe,
    observation: &Observation,
    observation_epoch: u64,
    active_session_epoch: u64,
) -> bool {
    !pending_observation_is_current(
        pending,
        observation,
        observation_epoch,
        active_session_epoch,
    )
}

#[allow(clippy::too_many_arguments)] // Mirrors the full gate; Files authority must win races.
pub(crate) fn stale_probe_should_rearm(
    pending: &PendingProbe,
    observation: &Observation,
    observation_epoch: u64,
    active_session_epoch: u64,
    files_user_intent_generation: u64,
    sidebar_ui_epoch: u64,
    files_context_current: bool,
    current_root: &std::path::Path,
    sidebar_ui: &SidebarUiSnapshot,
) -> bool {
    process_authority_changed_since_probe(
        pending,
        observation,
        observation_epoch,
        active_session_epoch,
    ) && files_authority_is_current(
        pending,
        files_user_intent_generation,
        sidebar_ui_epoch,
        files_context_current,
        current_root,
        sidebar_ui,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::jsh_remote::observed_ssh_target;

    fn target() -> RemoteHostConfig {
        match observed_ssh_target(&[
            "ssh".to_string(),
            "example.test".to_string(),
            "-p".to_string(),
            "22".to_string(),
        ]) {
            ObservedSshTarget::Target(target) => target,
            other => panic!("expected target, got {other:?}"),
        }
    }

    fn key() -> ObservationKey {
        ObservationKey {
            session_id: "session-1".to_string(),
            shell_pid: 42,
            argv: vec![
                "ssh".to_string(),
                "example.test".to_string(),
                "-p".to_string(),
                "22".to_string(),
            ],
            control_path: None,
        }
    }

    fn direct_command() -> ObservedSshCommand {
        let key = key();
        ObservedSshCommand {
            target: observed_ssh_target(&key.argv),
            argv: key.argv,
            reusable_control_path: None,
        }
    }

    fn command(argv: &[&str], reusable_control_path: Option<&str>) -> ObservedSshCommand {
        let argv = argv
            .iter()
            .map(|argument| argument.to_string())
            .collect::<Vec<_>>();
        ObservedSshCommand {
            target: observed_ssh_target(&argv),
            argv,
            reusable_control_path: reusable_control_path.map(str::to_string),
        }
    }

    fn sidebar_ui() -> SidebarUiSnapshot {
        SidebarUiSnapshot {
            visible: false,
            view: SidebarView::Commands,
            selected_path: None,
            selection: BTreeMap::new(),
            filter_open: false,
            filter: String::new(),
            files_dialog_open: false,
        }
    }

    fn pending() -> PendingProbe {
        PendingProbe {
            token: 7,
            observation_epoch: 3,
            active_session_epoch: 11,
            files_user_intent_generation: 13,
            sidebar_ui_epoch: 5,
            key: key(),
            authority: TargetAuthority::Transient(target()),
            profile: target(),
            overlay: remote_fs::SshExecutionOverlay::default(),
            commit: FollowCommit::ReplaceLocation,
            files_context: crate::sidebar::Sidebar::test_files_intent_context(9),
            root: PathBuf::from("/old/root"),
            sidebar_ui: sidebar_ui(),
        }
    }

    #[test]
    fn destination_before_port_is_an_observed_target() {
        assert!(matches!(
            classify_observed_command("session-1".to_string(), 42, direct_command()),
            Observation::Target { profile, .. }
                if profile.host == "example.test"
                    && profile.ssh_args == ["-p", "22"]
        ));
    }

    #[test]
    fn exact_dsw_argv_preserves_target_and_uses_bounded_distinctive_label() {
        const HOST: &str = "dsw-notebook-dsw-l8rnh0wm7vs81o7z6j-22.vpc-0jlbz3pri2042fd5xw2ov.instance-forward.dsw.cn-wulanchabu.aliyuncs.com";
        let observed = command(&["ssh", &format!("root@{HOST}"), "-p", "22"], None);
        let Observation::Target { profile, .. } =
            classify_observed_command("dsw-session".to_string(), 4242, observed)
        else {
            panic!("expected exact DSW SSH target");
        };
        assert_eq!(profile.user.as_deref(), Some("root"));
        assert_eq!(profile.host, HOST);
        assert_eq!(profile.ssh_args, ["-p", "22"]);

        let compact = crate::config::remote_host_runtime_location_label(&profile);
        assert!(compact.starts_with("root@dsw"), "{compact}");
        assert!(compact.ends_with("aliyuncs.com"), "{compact}");
        assert!(compact.contains('…'), "{compact}");
        let location = FsLocation::Transient((*profile).clone());
        let detail = location.detail(&[]);
        assert!(detail.contains(&format!("root@{HOST}")), "{detail}");
    }

    #[test]
    fn direct_control_path_is_execution_overlay_not_stable_profile_identity() {
        for command in [
            command(
                &["ssh", "-S", "/tmp/direct-cm-%C", "example.test", "-p", "22"],
                None,
            ),
            command(
                &[
                    "ssh",
                    "-o",
                    "ControlPath=/tmp/option-cm-%C",
                    "example.test",
                    "-p",
                    "22",
                ],
                None,
            ),
        ] {
            let expected_path = if command.argv.iter().any(|arg| arg == "-S") {
                "/tmp/direct-cm-%C"
            } else {
                "/tmp/option-cm-%C"
            };
            let Observation::Target {
                key,
                profile,
                overlay,
            } = classify_observed_command("session-1".to_string(), 42, command)
            else {
                panic!("expected direct SSH target");
            };
            assert_eq!(profile.ssh_args, ["-p", "22"]);
            assert_eq!(overlay.control_path.as_deref(), Some(expected_path));
            assert_eq!(key.control_path.as_deref(), Some(expected_path));
        }
    }

    #[test]
    fn trusted_jsh_launcher_fixture_keeps_socket_out_of_base_profile() {
        let launcher = command(
            &[
                "/bin/sh",
                "/home/alice/.cache/jsh/jsh-remote.sh",
                "--persist",
                "--local-jsh",
                "/home/alice/bin/jsh",
                "root@box.example",
                "--",
                "-p",
                "22",
            ],
            Some("/run/user/1000/anvil/cm-%C"),
        );
        let Observation::Target {
            key,
            profile,
            overlay,
        } = classify_observed_command("session-1".to_string(), 42, launcher)
        else {
            panic!("expected launcher SSH target");
        };
        assert_eq!(profile.host, "box.example");
        assert_eq!(profile.user.as_deref(), Some("root"));
        assert_eq!(profile.ssh_args, ["-p", "22"]);
        assert_eq!(
            overlay.control_path.as_deref(),
            Some("/run/user/1000/anvil/cm-%C")
        );
        assert_eq!(key.control_path, overlay.control_path);
    }

    #[test]
    fn saved_transport_matching_ignores_control_path_but_final_identity_is_exact() {
        let observed = target();
        let mut saved = observed.clone();
        saved.name = "Saved display name".to_string();
        saved
            .ssh_args
            .extend(["-S".to_string(), "/tmp/saved-control-%C".to_string()]);
        let authority = target_authority(&observed, &[saved.clone()]);
        assert_eq!(authority, TargetAuthority::Saved(saved.clone()));

        let mut unrelated = target();
        unrelated.host = "other.example.test".to_string();
        let reordered = vec![unrelated, saved.clone()];
        assert_eq!(
            authority.current_location(&observed, &reordered),
            Some(FsLocation::Remote(1))
        );

        let mut replacement = saved.clone();
        replacement.name = "Edited while probing".to_string();
        assert_eq!(authority.current_location(&observed, &[replacement]), None);
        assert_eq!(
            authority.current_location(&observed, &[saved.clone(), saved]),
            None,
            "duplicate exact profiles are ambiguous at final commit"
        );
    }

    #[test]
    fn ambiguous_saved_transport_stays_transient() {
        let observed = target();
        let mut first = observed.clone();
        first.name = "first".to_string();
        let mut second = observed.clone();
        second.name = "second".to_string();
        assert_eq!(
            target_authority(&observed, &[first, second]),
            TargetAuthority::Transient(observed)
        );
    }

    #[test]
    fn managed_exact_argv_remote_sessions_are_not_observed() {
        assert!(purpose_is_observable(SessionPurpose::Interactive));
        assert!(!purpose_is_observable(SessionPurpose::EphemeralCommand));
        assert!(!purpose_is_observable(SessionPurpose::RetainedCommand));
    }

    #[test]
    fn different_same_target_overlay_is_staged_without_preprobe_mutation() {
        let old_overlay = remote_fs::SshExecutionOverlay::from_control_path(Some(
            "/run/user/1000/ember/old-%C".to_string(),
        ));
        let observed_overlay = remote_fs::SshExecutionOverlay::from_control_path(Some(
            "/run/user/1000/ember/new-%C".to_string(),
        ));
        let retained = old_overlay.clone();

        assert_eq!(
            same_target_action(true, &old_overlay, &observed_overlay),
            SameTargetAction::ProbeOverlayUpgrade
        );
        assert_eq!(
            old_overlay, retained,
            "staging is a pure authority decision"
        );
        assert_eq!(
            same_target_action(true, &old_overlay, &old_overlay),
            SameTargetAction::RevealExisting
        );
        assert_eq!(
            same_target_action(false, &old_overlay, &observed_overlay),
            SameTargetAction::DifferentLocation
        );
    }

    #[test]
    fn same_frame_files_chrome_intent_suppresses_first_observation_including_armed_retry() {
        let frame_start = 40;
        let after_shortcut_or_menu_toggle = 41;
        // Retry itself deliberately does not advance this generation. Thus a
        // changed value always proves some *other* Files intent happened and
        // wins even if Retry was armed in the same frame.
        assert!(same_frame_files_intent_suppresses_new_observation(
            frame_start,
            after_shortcut_or_menu_toggle,
        ));
        assert!(!same_frame_files_intent_suppresses_new_observation(
            frame_start,
            frame_start,
        ));
    }

    #[test]
    fn open_files_popups_and_drag_hover_are_ongoing_user_intents() {
        assert!(ongoing_files_surface_is_user_intent(true, false));
        assert!(ongoing_files_surface_is_user_intent(false, true));
        assert!(ongoing_files_surface_is_user_intent(true, true));
        assert!(!ongoing_files_surface_is_user_intent(false, false));
    }

    #[test]
    fn observation_epoch_rejects_exit_and_same_argv_reentry_aba() {
        let observation = Observation::Target {
            key: key(),
            profile: Box::new(target()),
            overlay: remote_fs::SshExecutionOverlay::default(),
        };
        let mut state = State::default();
        let first = state.sync_observation(&observation);
        assert_eq!(state.sync_observation(&observation), first);
        let absent = state.sync_observation(&Observation::None);
        assert_ne!(absent, first);
        let reentered = state.sync_observation(&observation);
        assert_ne!(reentered, first);
        assert_ne!(reentered, absent);
    }

    #[test]
    fn focus_aba_rearms_old_key_but_unchanged_process_ui_cancel_does_not() {
        let observation_a = Observation::Target {
            key: key(),
            profile: Box::new(target()),
            overlay: remote_fs::SshExecutionOverlay::default(),
        };
        let mut key_b = key();
        key_b.session_id = "session-b".to_string();
        key_b.shell_pid = 84;
        let observation_b = Observation::Target {
            key: key_b,
            profile: Box::new(target()),
            overlay: remote_fs::SshExecutionOverlay::default(),
        };
        let mut state = State::default();
        let epoch_a = state.sync_observation(&observation_a);
        let mut pending = pending();
        pending.observation_epoch = epoch_a;
        state.mark_handled(pending.key.clone());

        assert!(!process_authority_changed_since_probe(
            &pending,
            &observation_a,
            epoch_a,
            11,
        ));
        assert!(state.was_handled(&pending.key));

        let _ = state.sync_observation(&observation_b);
        let returned_epoch = state.sync_observation(&observation_a);
        assert!(process_authority_changed_since_probe(
            &pending,
            &observation_a,
            returned_epoch,
            11,
        ));
        state.rearm_after_stale_probe(&pending.key);
        assert!(!state.was_handled(&pending.key));
    }

    #[test]
    fn synchronous_active_session_epoch_rejects_between_poll_focus_aba() {
        let pending = pending();
        let observation = Observation::Target {
            key: pending.key.clone(),
            profile: Box::new(pending.profile.clone()),
            overlay: pending.overlay.clone(),
        };
        let ui = sidebar_ui();

        assert!(result_is_current(
            &pending,
            &observation,
            pending.observation_epoch,
            pending.active_session_epoch,
            pending.files_user_intent_generation,
            pending.sidebar_ui_epoch,
            true,
            &pending.root,
            &ui,
        ));
        let after_a_to_b_to_a = pending.active_session_epoch + 2;
        assert!(!result_is_current(
            &pending,
            &observation,
            pending.observation_epoch,
            after_a_to_b_to_a,
            pending.files_user_intent_generation,
            pending.sidebar_ui_epoch,
            true,
            &pending.root,
            &ui,
        ));
        assert!(process_authority_changed_since_probe(
            &pending,
            &observation,
            pending.observation_epoch,
            after_a_to_b_to_a,
        ));
    }

    #[test]
    fn scheduled_active_close_cannot_commit_pending_session_a() {
        let pending = pending();
        let observation_a = Observation::Target {
            key: pending.key.clone(),
            profile: Box::new(pending.profile.clone()),
            overlay: pending.overlay.clone(),
        };
        let ui = sidebar_ui();
        assert!(result_is_current(
            &pending,
            &observation_a,
            pending.observation_epoch,
            pending.active_session_epoch,
            pending.files_user_intent_generation,
            pending.sidebar_ui_epoch,
            true,
            &pending.root,
            &ui,
        ));
        assert!(!poll_allowed_after_shell_exit(
            true,
            "session-a",
            Some("session-a")
        ));
        assert!(!poll_allowed_after_shell_exit(true, "session-a", None));
        assert!(poll_allowed_after_shell_exit(
            true,
            "session-a",
            Some("session-b")
        ));
        assert!(poll_allowed_after_shell_exit(
            false,
            "session-a",
            Some("session-a")
        ));
        assert!(
            !result_is_current(
                &pending,
                &Observation::None,
                pending.observation_epoch + 1,
                pending.active_session_epoch + 1,
                pending.files_user_intent_generation,
                pending.sidebar_ui_epoch,
                true,
                &pending.root,
                &ui,
            ),
            "after a multi-session close, B's activation/process authority rejects pending A"
        );
    }

    #[test]
    fn prior_files_intent_wins_over_combined_focus_aba_and_prevents_rearm() {
        let pending = pending();
        let observation = Observation::Target {
            key: pending.key.clone(),
            profile: Box::new(pending.profile.clone()),
            overlay: pending.overlay.clone(),
        };
        let after_focus_aba = pending.active_session_epoch + 2;
        let after_files_intent = pending.files_user_intent_generation + 1;
        let ui = sidebar_ui();
        let mut state = State::default();
        state.mark_handled(pending.key.clone());

        assert!(process_authority_changed_since_probe(
            &pending,
            &observation,
            pending.observation_epoch,
            after_focus_aba,
        ));
        assert!(!stale_probe_should_rearm(
            &pending,
            &observation,
            pending.observation_epoch,
            after_focus_aba,
            after_files_intent,
            pending.sidebar_ui_epoch,
            true,
            &pending.root,
            &ui,
        ));
        assert!(state.was_handled(&pending.key));
        assert!(stale_probe_should_rearm(
            &pending,
            &observation,
            pending.observation_epoch,
            after_focus_aba,
            pending.files_user_intent_generation,
            pending.sidebar_ui_epoch,
            true,
            &pending.root,
            &ui,
        ));
    }

    #[test]
    fn failed_probe_retries_only_after_explicit_request_for_exact_observation() {
        let mut state = State::default();
        let key = key();
        let observation = Observation::Target {
            key: key.clone(),
            profile: Box::new(target()),
            overlay: remote_fs::SshExecutionOverlay::default(),
        };
        state.mark_handled(key.clone());
        state.record_failure(key.clone());
        assert!(state.retry_available_for_observation(&observation));
        assert!(!state.retry_requested_for(&key));

        state.request_retry();
        let mut other = key.clone();
        other.shell_pid += 1;
        assert!(!state.retry_requested_for(&other));
        assert!(state.retry_requested_for(&key));

        // A user action can cancel the armed retry without changing the live
        // process key. Dedupe remains handled, so the same argv cannot be
        // mistaken for a brand-new automatic observation on the next frame.
        state.cancel_retry_for(&key);
        assert!(!state.retry_requested_for(&key));
        assert!(state.was_handled(&key));
        assert!(state.retry_available_for_observation(&observation));

        state.request_retry();
        state.consume_retry_for(&key);
        assert!(state.was_handled(&key));
        assert!(!state.retry_available_for_observation(&observation));
    }

    #[test]
    fn persistent_retry_is_available_only_for_the_exact_live_observation() {
        let observation = Observation::Target {
            key: key(),
            profile: Box::new(target()),
            overlay: remote_fs::SshExecutionOverlay::default(),
        };
        let mut state = State::default();
        state.sync_observation(&observation);
        state.record_failure(key());
        assert!(state.retry_available_for_observation(&observation));

        let mut other_key = key();
        other_key.argv.push("different".to_string());
        let other = Observation::Target {
            key: other_key,
            profile: Box::new(target()),
            overlay: remote_fs::SshExecutionOverlay::default(),
        };
        assert!(!state.retry_available_for_observation(&other));
        assert!(!state.retry_available_for_observation(&Observation::None));
        assert!(
            !state.retry_available_for_observation(&Observation::Unsupported {
                key: key(),
                reason: "unsupported",
            })
        );
    }

    #[test]
    fn files_intent_suppresses_same_argv_after_operation_settles() {
        let observation = Observation::Target {
            key: key(),
            profile: Box::new(target()),
            overlay: remote_fs::SshExecutionOverlay::default(),
        };
        let mut state = State::default();
        let epoch = state.sync_observation(&observation);
        state.mark_handled(key());
        state.record_failure(key());
        state.request_retry();

        // A pending operation/dialog consumes even an armed explicit retry.
        state.suppress_for_files_intent(&key());
        assert!(state.was_handled(&key()));
        assert!(!state.retry_requested_for(&key()));
        assert!(state.retry_available_for_observation(&observation));

        // Settling the operation without a process/focus change must not
        // re-arm automatic following of the unchanged argv.
        assert_eq!(state.sync_observation(&observation), epoch);
        assert!(state.was_handled(&key()));
    }

    #[test]
    fn sidebar_ui_epoch_rejects_hide_or_view_aba() {
        let mut state = State::default();
        let initial_ui = sidebar_ui();
        let initial = state.sync_sidebar_ui(&initial_ui);
        assert_eq!(state.sync_sidebar_ui(&initial_ui), initial);
        let mut changed_ui = initial_ui.clone();
        changed_ui.visible = true;
        changed_ui.view = SidebarView::Files;
        let changed = state.sync_sidebar_ui(&changed_ui);
        let returned = state.sync_sidebar_ui(&initial_ui);
        assert_ne!(changed, initial);
        assert_ne!(returned, initial);
        assert_ne!(returned, changed);
    }

    #[test]
    fn commit_gate_requires_exact_session_argv_profile_and_untouched_files_ui() {
        let pending = pending();
        let observation = Observation::Target {
            key: pending.key.clone(),
            profile: Box::new(pending.profile.clone()),
            overlay: pending.overlay.clone(),
        };
        let ui = sidebar_ui();
        assert!(result_is_current(
            &pending,
            &observation,
            3,
            11,
            13,
            5,
            true,
            std::path::Path::new("/old/root"),
            &ui,
        ));

        let mut different_key = pending.key.clone();
        different_key.session_id = "session-2".to_string();
        let different_session = Observation::Target {
            key: different_key,
            profile: Box::new(pending.profile.clone()),
            overlay: pending.overlay.clone(),
        };
        assert!(!result_is_current(
            &pending,
            &different_session,
            3,
            11,
            13,
            5,
            true,
            std::path::Path::new("/old/root"),
            &ui,
        ));
        let different_overlay = Observation::Target {
            key: pending.key.clone(),
            profile: Box::new(pending.profile.clone()),
            overlay: remote_fs::SshExecutionOverlay::from_control_path(Some(
                "/tmp/different-cm-%C".to_string(),
            )),
        };
        assert!(!result_is_current(
            &pending,
            &different_overlay,
            3,
            11,
            13,
            5,
            true,
            std::path::Path::new("/old/root"),
            &ui,
        ));
        assert!(!result_is_current(
            &pending,
            &observation,
            3,
            11,
            13,
            5,
            false,
            std::path::Path::new("/old/root"),
            &ui,
        ));
        assert!(!result_is_current(
            &pending,
            &observation,
            3,
            11,
            13,
            5,
            true,
            std::path::Path::new("/user/changed/root"),
            &ui,
        ));
        let mut visible_ui = ui.clone();
        visible_ui.visible = true;
        assert!(!result_is_current(
            &pending,
            &observation,
            3,
            11,
            13,
            5,
            true,
            std::path::Path::new("/old/root"),
            &visible_ui,
        ));
        let mut files_ui = ui.clone();
        files_ui.view = SidebarView::Files;
        assert!(!result_is_current(
            &pending,
            &observation,
            3,
            11,
            13,
            5,
            true,
            std::path::Path::new("/old/root"),
            &files_ui,
        ));
        assert!(!result_is_current(
            &pending,
            &observation,
            4,
            11,
            13,
            5,
            true,
            std::path::Path::new("/old/root"),
            &ui,
        ));
        assert!(!result_is_current(
            &pending,
            &observation,
            3,
            11,
            14,
            5,
            true,
            std::path::Path::new("/old/root"),
            &ui,
        ));
        assert!(!result_is_current(
            &pending,
            &observation,
            3,
            11,
            13,
            6,
            true,
            std::path::Path::new("/old/root"),
            &ui,
        ));
        let mut selection_ui = ui.clone();
        selection_ui
            .selection
            .insert(PathBuf::from("/old/root/new-selection"), false);
        assert!(!result_is_current(
            &pending,
            &observation,
            3,
            11,
            13,
            5,
            true,
            std::path::Path::new("/old/root"),
            &selection_ui,
        ));
    }
}
