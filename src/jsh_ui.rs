//! Install and update surfaces for the companion shell, jsh.
//!
//! Two entry points, both explicit: the palette command, and a notice row that
//! appears only after a background check found something actionable. Nothing
//! installs itself, and nothing blocks a frame — the check runs on a worker
//! thread and the row stays hidden until it has an answer.
//!
//! The decisions live in `jterm_core::jsh_install`, shared with the other
//! terminals; this file is only ember's surface for them.

use crate::app::state::TerminalApp;
use eframe::egui;
use jterm_core::jsh_install::{self, Prompt, Status};
use jterm_core::jsh_remote::RemoteHostConfig;
use std::sync::mpsc::{Receiver, TryRecvError};

fn ssh_files_login_argv(
    host: &RemoteHostConfig,
    overlay: &crate::remote_fs::SshExecutionOverlay,
) -> Result<Vec<String>, String> {
    crate::config::validate_remote_host(host)?;
    if host.docker {
        return Err("a Files execution overlay requires SSH".to_string());
    }
    crate::remote_fs::validate_execution_endpoint(
        &crate::remote_fs::FsLocation::Transient(host.clone()),
        &[],
        overlay,
    )
    .map_err(|error| error.to_string())?;
    let mut argv = vec!["ssh".to_string(), "-t".to_string()];
    if overlay.is_empty() {
        argv.extend(host.ssh_args.iter().cloned());
    } else {
        argv.extend(crate::remote_fs::split_ssh_control_path_args(&host.ssh_args).0);
    }
    if let Some(path) = &overlay.control_path {
        argv.push("-S".to_string());
        argv.push(path.clone());
    }
    argv.push("--".to_string());
    argv.push(match &host.user {
        Some(user) => format!("{user}@{}", host.host),
        None => host.host.clone(),
    });
    Ok(argv)
}

/// Background update check plus whatever it decided to offer.
#[derive(Default)]
pub struct JshNotice {
    started: bool,
    pending: Option<Receiver<Status>>,
    prompt: Option<Prompt>,
    dismissed: bool,
}

impl JshNotice {
    /// Start the check on the first frame, unless the policy says not to.
    /// Deferring to the first frame keeps startup itself untouched.
    fn ensure_started(&mut self, policy: &str) {
        if self.started {
            return;
        }
        self.started = true;
        self.start(policy);
    }

    /// Start the check unless the configured policy says not to.
    fn start(&mut self, policy: &str) {
        // "startup" asks the network every launch; "daily" reuses the
        // installer's cache, which every jterm on this machine shares.
        let Some(max_age) = jsh_install::UpdateCheck::parse(policy).max_age() else {
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(jsh_install::check_blocking(max_age));
        });
        self.pending = Some(receiver);
    }

    /// Collect the worker's answer, if it has one. Never blocks a frame.
    fn poll(&mut self) {
        let Some(receiver) = self.pending.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(status) => {
                self.pending = None;
                if let Some(error) = &status.error {
                    log::info!("jsh update check unavailable: {error}");
                }
                if let Some(other) = &status.shadowed_by {
                    // Some other binary named jsh, earlier on PATH. Installing
                    // does not fix PATH order, so the installer explains it in
                    // the session; here it is only worth a log line.
                    log::warn!("PATH resolves jsh to {other}, which ember does not manage");
                }
                // A check that failed, or found nothing to do, stays silent: an
                // offline laptop must not grow a row it cannot act on.
                self.prompt = jsh_install::prompt_for(&status);
                if let Some(prompt) = &self.prompt {
                    log::info!("jsh notice: {}", prompt.banner_title());
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                log::warn!("jsh update check ended without a result");
            }
        }
    }

    fn visible_prompt(&self) -> Option<&Prompt> {
        if self.dismissed {
            return None;
        }
        self.prompt.as_ref()
    }
}

impl TerminalApp {
    fn connect_plain_ssh_files_host(
        &mut self,
        display_name: String,
        host: RemoteHostConfig,
        overlay: crate::remote_fs::SshExecutionOverlay,
    ) {
        let argv = match ssh_files_login_argv(&host, &overlay) {
            Ok(argv) => argv,
            Err(problem) => {
                self.set_status(format!("Remote host {display_name}: {problem}"));
                return;
            }
        };
        self.set_status(format!("Connecting to {display_name}"));

        let (cols, rows) = crate::terminal::clamp_terminal_dimensions(self.cols, self.rows);
        let old_len = self.session_manager.len();
        let index = self.session_manager.new_command_session(
            display_name,
            argv,
            cols,
            rows,
            self.config.scrollback_lines,
        );
        if self.session_manager.len() > old_len {
            self.tabs.on_session_inserted(index);
            self.tabs.insert_tab_after_active(index);
        }
        self.activate_session(index);
    }

    /// Draw the notice row, if there is anything to say. Returns true when the
    /// user asked to install, so the caller can act outside the closure.
    pub fn render_jsh_notice(&mut self, root_ui: &mut egui::Ui) -> bool {
        self.jsh_notice
            .ensure_started(&self.config.jsh_update_check);
        self.jsh_notice.poll();
        let Some(prompt) = self.jsh_notice.visible_prompt() else {
            return false;
        };
        let title = prompt.banner_title();
        let button = prompt.button_label();

        let mut install = false;
        let mut dismiss = false;
        egui::Panel::top("jsh_notice")
            .resizable(false)
            .show(root_ui, |ui| {
                // Buttons are laid out first so they always fit; the message
                // takes whatever space is left and truncates in a narrow
                // window instead of pushing them off the edge.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("✕")
                        .on_hover_text("Dismiss until the next launch")
                        .clicked()
                    {
                        dismiss = true;
                    }
                    if ui.button(button).clicked() {
                        install = true;
                    }
                    ui.add(egui::Label::new(title).truncate());
                });
            });

        if install || dismiss {
            self.jsh_notice.dismissed = true;
        }
        install
    }

    /// Run the installer in its own session. The script narrates what it does,
    /// so the session is the progress UI — the user can read a failure or
    /// interrupt it with Ctrl+C like any other command.
    pub fn install_or_update_jsh(&mut self) {
        let argv = match jsh_install::install_argv() {
            Ok(argv) => argv,
            Err(error) => {
                log::warn!("cannot stage the jsh installer: {error}");
                self.set_status(format!("Could not write the installer script: {error}"));
                return;
            }
        };

        let (cols, rows) = crate::terminal::clamp_terminal_dimensions(self.cols, self.rows);
        let old_len = self.session_manager.len();
        let index = self.session_manager.new_command_session(
            "Install jsh".to_string(),
            argv,
            cols,
            rows,
            self.config.scrollback_lines,
        );
        if self.session_manager.len() > old_len {
            self.tabs.on_session_inserted(index);
            // 安装脚本自己就是进度界面，给它一个独立 tab，而不是塞进当前
            // tab 的某个窗格里。
            self.tabs.insert_tab_after_active(index);
        }
        self.activate_session(index);
        self.set_status("Installing jsh in a new session");
    }

    /// Open a `[[remote_hosts]]` destination in its own session. The argv
    /// comes from the shared family builder: the deploy launcher when the
    /// entry asks for it — lending the local jsh when that one is static —
    /// and a plain ssh / `docker exec` otherwise.
    pub fn connect_remote_host(&mut self, index: usize) {
        let host = match crate::config::validate_remote_host_at(&self.config.remote_hosts, index) {
            Ok(host) => host.clone(),
            Err(problem) => {
                let name = self
                    .config
                    .remote_hosts
                    .get(index)
                    .map(|host| crate::config::remote_host_display_name(host, index))
                    .unwrap_or_else(|| format!("remote host #{}", index + 1));
                self.set_status(format!("Remote host {name}: {problem}"));
                return;
            }
        };
        let display_name = crate::config::remote_host_display_name(&host, index);
        let (argv, degraded) = host.tab_argv();
        if let Some(error) = degraded {
            // The tab still opens — a plain connection beats no connection —
            // but quietly pretending jsh was deployed would be worse than
            // either.
            log::warn!("cannot publish jsh-remote.sh: {error}; connecting without deployment");
            self.set_status(format!(
                "Deploy unavailable ({error}); connecting to {} plainly",
                display_name
            ));
        } else {
            self.set_status(format!("Connecting to {display_name}"));
        }

        let (cols, rows) = crate::terminal::clamp_terminal_dimensions(self.cols, self.rows);
        let old_len = self.session_manager.len();
        let index = self.session_manager.new_command_session(
            display_name,
            argv,
            cols,
            rows,
            self.config.scrollback_lines,
        );
        if self.session_manager.len() > old_len {
            self.tabs.on_session_inserted(index);
            // 远程会话拿独立 tab，而不是塞进当前 tab 的分屏里。
            self.tabs.insert_tab_after_active(index);
        }
        self.activate_session(index);
    }

    /// Open the terminal action for a saved Files profile. Ordinarily this
    /// preserves the profile's existing deploy/jsh behavior. When the Files
    /// tree is bound to a live execution overlay, however, the exact socket is
    /// the connection authority: use a plain interactive SSH login and never
    /// inject the saved profile's remote command.
    pub fn connect_files_remote_host(
        &mut self,
        index: usize,
        overlay: crate::remote_fs::SshExecutionOverlay,
    ) {
        if overlay.is_empty() {
            self.connect_remote_host(index);
            return;
        }
        let host = match crate::config::validate_remote_host_at(&self.config.remote_hosts, index) {
            Ok(host) => host.clone(),
            Err(problem) => {
                let name = self
                    .config
                    .remote_hosts
                    .get(index)
                    .map(|host| crate::config::remote_host_display_name(host, index))
                    .unwrap_or_else(|| format!("remote host #{}", index + 1));
                self.set_status(format!("Remote host {name}: {problem}"));
                return;
            }
        };
        let display_name = crate::config::remote_host_display_name(&host, index);
        self.connect_plain_ssh_files_host(display_name, host, overlay);
    }

    /// Re-open a transient Files target as an interactive SSH login. Unlike a
    /// configured jsh profile this intentionally has no remote command: the
    /// target came from a hand-written plain SSH login and may not have jsh.
    /// `new_command_session` marks it `EphemeralCommand`, which also prevents
    /// the SSH Files observer from following Ember's own managed tab again.
    pub fn connect_transient_remote_host(
        &mut self,
        host: RemoteHostConfig,
        overlay: crate::remote_fs::SshExecutionOverlay,
    ) {
        let display_name = crate::config::remote_host_runtime_label(&host);
        self.connect_plain_ssh_files_host(display_name, host, overlay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::jsh_remote::{observed_ssh_target, ObservedSshTarget};

    #[test]
    fn transient_terminal_reuses_connection_options_but_no_remote_command() {
        let host = match observed_ssh_target(&[
            "ssh".to_string(),
            "alice@example.test".to_string(),
            "-p".to_string(),
            "2222".to_string(),
        ]) {
            ObservedSshTarget::Target(host) => host,
            other => panic!("expected SSH target, got {other:?}"),
        };
        assert_eq!(
            ssh_files_login_argv(&host, &crate::remote_fs::SshExecutionOverlay::default(),)
                .unwrap(),
            ["ssh", "-t", "-p", "2222", "--", "alice@example.test"]
        );
    }

    #[test]
    fn transient_terminal_is_plain_ssh_with_execution_only_control_path() {
        let host = match observed_ssh_target(&["ssh".to_string(), "example.test".to_string()]) {
            ObservedSshTarget::Target(host) => host,
            other => panic!("expected SSH target, got {other:?}"),
        };
        let overlay = crate::remote_fs::SshExecutionOverlay::from_control_path(Some(
            "/run/user/1000/anvil/cm-%C".to_string(),
        ));
        assert_eq!(
            ssh_files_login_argv(&host, &overlay).unwrap(),
            [
                "ssh",
                "-t",
                "-S",
                "/run/user/1000/anvil/cm-%C",
                "--",
                "example.test",
            ]
        );
        assert!(host.ssh_args.is_empty(), "overlay must not mutate identity");
    }

    #[test]
    fn saved_live_overlay_terminal_is_plain_ssh_and_omits_deploy_command() {
        let mut host = match observed_ssh_target(&[
            "ssh".to_string(),
            "alice@example.test".to_string(),
            "-p".to_string(),
            "2222".to_string(),
        ]) {
            ObservedSshTarget::Target(host) => host,
            other => panic!("expected SSH target, got {other:?}"),
        };
        host.deploy = "persist".to_string();
        host.ssh_args
            .extend(["-S".to_string(), "/tmp/saved-stale-%C".to_string()]);
        let overlay = crate::remote_fs::SshExecutionOverlay::from_control_path(Some(
            "/run/user/1000/ember/live-%C".to_string(),
        ));

        let argv = ssh_files_login_argv(&host, &overlay).unwrap();
        assert_eq!(
            argv,
            [
                "ssh",
                "-t",
                "-p",
                "2222",
                "-S",
                "/run/user/1000/ember/live-%C",
                "--",
                "alice@example.test",
            ]
        );
        assert!(
            argv.iter().all(|arg| !arg.contains("jsh-remote")),
            "a live Files socket must never inherit a saved deploy command"
        );
    }
}
