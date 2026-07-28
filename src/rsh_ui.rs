//! Install and update surfaces for the companion shell, rsh.
//!
//! Two entry points, both explicit: the palette command, and a notice row that
//! appears only after a background check found something actionable. Nothing
//! installs itself, and nothing blocks a frame — the check runs on a worker
//! thread and the row stays hidden until it has an answer.
//!
//! The decisions live in `jterm_core::rsh_install`, shared with the other
//! terminals; this file is only jterm2's surface for them.

use crate::app::state::TerminalApp;
use eframe::egui;
use jterm_core::rsh_install::{self, Prompt, Status};
use std::sync::mpsc::{Receiver, TryRecvError};

/// Background update check plus whatever it decided to offer.
#[derive(Default)]
pub struct RshNotice {
    started: bool,
    pending: Option<Receiver<Status>>,
    prompt: Option<Prompt>,
    dismissed: bool,
}

impl RshNotice {
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
        let Some(max_age) = rsh_install::UpdateCheck::parse(policy).max_age() else {
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(rsh_install::check_blocking(max_age));
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
                    log::info!("rsh update check unavailable: {error}");
                }
                if let Some(other) = &status.shadowed_by {
                    // Usually /usr/bin/rsh, the BSD remote shell. Installing
                    // does not fix PATH order, so the installer explains it in
                    // the session; here it is only worth a log line.
                    log::warn!("PATH resolves rsh to {other}, which jterm2 does not manage");
                }
                // A check that failed, or found nothing to do, stays silent: an
                // offline laptop must not grow a row it cannot act on.
                self.prompt = rsh_install::prompt_for(&status);
                if let Some(prompt) = &self.prompt {
                    log::info!("rsh notice: {}", prompt.banner_title());
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                log::warn!("rsh update check ended without a result");
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
    /// Draw the notice row, if there is anything to say. Returns true when the
    /// user asked to install, so the caller can act outside the closure.
    pub fn render_rsh_notice(&mut self, root_ui: &mut egui::Ui) -> bool {
        self.rsh_notice
            .ensure_started(&self.config.rsh_update_check);
        self.rsh_notice.poll();
        let Some(prompt) = self.rsh_notice.visible_prompt() else {
            return false;
        };
        let title = prompt.banner_title();
        let button = prompt.button_label();

        let mut install = false;
        let mut dismiss = false;
        egui::Panel::top("rsh_notice")
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
            self.rsh_notice.dismissed = true;
        }
        install
    }

    /// Run the installer in its own session. The script narrates what it does,
    /// so the session is the progress UI — the user can read a failure or
    /// interrupt it with Ctrl+C like any other command.
    pub fn install_or_update_rsh(&mut self) {
        let argv = match rsh_install::install_argv() {
            Ok(argv) => argv,
            Err(error) => {
                log::warn!("cannot stage the rsh installer: {error}");
                self.set_status(format!("Could not write the installer script: {error}"));
                return;
            }
        };

        let (cols, rows) = crate::terminal::clamp_terminal_dimensions(self.cols, self.rows);
        let old_len = self.session_manager.len();
        let index = self.session_manager.new_command_session(
            "Install rsh".to_string(),
            argv,
            cols,
            rows,
            self.config.scrollback_lines,
        );
        if self.session_manager.len() > old_len {
            self.layout_manager.on_session_inserted(index);
        }
        self.activate_session(index);
        self.set_status("Installing rsh in a new session");
    }
}
