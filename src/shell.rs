//! Product and platform launch policy for Ember's shared session actor.
use crate::pty::Pty;
use crate::terminal::clamp_terminal_dimensions;
use jterm_core::native_pty_launch::PtyLaunchError;
use jterm_core::output_batch::OutputBuffer;
use jterm_core::runtime_wake::WakeSink;
use jterm_core::session_actor::SessionHandle;
use std::time::{Duration, Instant};

pub use jterm_core::session_actor::{
    OutputNotice, OutputProgress, WriteError as ShellWriteError, WriteSender as ShellWriteSender,
};

pub struct ShellSession {
    actor: SessionHandle<Pty>,
    child_pid: i32,
    started_at: Instant,
    shell_program: Option<String>,
    shell_identity: companion_host::shell_identity::ShellIdentity,
}

impl ShellSession {
    /// 启动新的 shell session
    #[allow(dead_code)]
    pub fn new(
        cols: usize,
        rows: usize,
        configured_shell: Option<&str>,
        repaint_ctx: WakeSink,
    ) -> std::result::Result<Self, String> {
        Self::new_with_cwd(cols, rows, None, None, configured_shell, None, repaint_ctx)
    }

    /// 启动新的 shell session，指定初始工作目录和 session ID
    /// `command_argv` replaces the interactive shell with an explicit argv, for
    /// one-shot helper sessions such as the jsh installer.
    pub fn new_with_cwd(
        cols: usize,
        rows: usize,
        cwd: Option<&str>,
        session_id: Option<&str>,
        configured_shell: Option<&str>,
        command_argv: Option<&[String]>,
        repaint_ctx: WakeSink,
    ) -> std::result::Result<Self, String> {
        Self::new_with_pinned_cwd(
            cols,
            rows,
            cwd,
            session_id,
            configured_shell,
            command_argv,
            None,
            repaint_ctx,
        )
        .map_err(|error| error.to_string())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_pinned_cwd(
        cols: usize,
        rows: usize,
        cwd: Option<&str>,
        session_id: Option<&str>,
        configured_shell: Option<&str>,
        command_argv: Option<&[String]>,
        pinned_cwd: Option<crate::pty::PinnedDirectory>,
        repaint_ctx: WakeSink,
    ) -> anyhow::Result<Self> {
        let (cols, rows) = clamp_terminal_dimensions(cols, rows);
        let shell_program = if command_argv.is_none() {
            Some(crate::pty::resolved_shell(configured_shell)?)
        } else {
            None
        };
        let spawn_shell = shell_program.as_deref().or(configured_shell);
        let pty = Pty::new_with_pinned_cwd(
            cols,
            rows,
            cwd,
            session_id,
            spawn_shell,
            command_argv,
            pinned_cwd,
        )
        .map_err(|error| {
            let message = format!("Failed to create shell session: {error}");
            error.context(message)
        })?;
        let child_pid = pty.get_child_pid();
        let shell_identity = companion_host::shell_identity::ShellIdentity::capture(
            child_pid,
            shell_program.as_deref().map(std::path::Path::new),
        );
        let started_at = Instant::now();
        let actor = SessionHandle::start_ordered(pty, repaint_ctx)
            .map_err(|error| PtyLaunchError::ExecConfirmed(std::io::Error::other(error)))?;
        Ok(Self {
            actor,
            child_pid,
            started_at,
            shell_program,
            shell_identity,
        })
    }

    pub(crate) fn failed_working_directory(error: &anyhow::Error) -> bool {
        error
            .downcast_ref::<PtyLaunchError>()
            .is_some_and(PtyLaunchError::failed_working_directory)
    }

    pub fn output_source(&self) -> Option<u64> {
        self.actor.output_source()
    }

    pub fn poll_output(
        &mut self,
        budget: usize,
        admit: impl FnOnce() -> bool,
        apply: impl FnOnce(&OutputBuffer, bool),
    ) -> OutputProgress {
        self.actor.poll_output(budget, admit, apply)
    }

    pub fn has_pending_output(&self) -> bool {
        self.actor.has_pending_output()
    }

    pub fn buffered_output_bytes(&self) -> usize {
        self.actor.buffered_output_bytes()
    }

    pub fn output_finished(&self) -> bool {
        self.actor.output_finished()
    }

    /// Time when the exit was first collected, even if its tail took additional polls.
    pub fn uptime_at(&self, observed_at: Instant) -> Duration {
        observed_at.saturating_duration_since(self.started_at)
    }

    pub fn shell_program(&self) -> Option<&str> {
        self.shell_program.as_deref()
    }

    pub fn matches_interactive_shell_program(&self) -> bool {
        self.shell_identity.matches_current_program()
    }

    pub fn session_activity(&self) -> companion_host::pty_session::Activity {
        self.actor.inspect_transport(Pty::session_activity)
    }

    pub fn write(&self, data: &[u8]) -> Result<(), ShellWriteError> {
        self.actor.write(data)
    }

    pub fn input_drained(&self) -> Result<bool, ShellWriteError> {
        self.actor.input_drained()
    }

    pub fn foreground_pgid(&self) -> Option<i32> {
        self.actor.foreground_pgid()
    }

    pub(crate) fn try_write_control_byte(
        &self,
        byte: u8,
        validate: impl FnOnce(i32) -> bool,
    ) -> bool {
        self.actor.try_write_control_byte(byte, validate)
    }

    pub fn write_sender(&self) -> ShellWriteSender {
        self.actor.write_sender()
    }

    pub fn resize(&self, cols: usize, rows: usize) -> Result<(), String> {
        self.actor.resize(cols, rows)
    }

    pub fn get_child_pid(&self) -> i32 {
        self.child_pid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    fn wait_for_output(
        shell: &mut ShellSession,
        deadline: Instant,
        mut apply: impl FnMut(&OutputBuffer),
    ) -> Option<OutputProgress> {
        while Instant::now() < deadline {
            let progress = shell.poll_output(4096, || true, |batch, _| apply(batch));
            if progress.bytes_applied != 0 || progress.notice.is_some() {
                return Some(progress);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    fn wait_for_ready(shell: &mut ShellSession, deadline: Instant) {
        let mut output = Vec::new();
        while !output.windows(5).any(|word| word == b"READY") {
            let progress = wait_for_output(shell, deadline, |batch| {
                output.extend_from_slice(batch.as_bytes());
            })
            .expect("timed out waiting for PTY readiness");
            assert!(progress.notice.is_none(), "{:?}", progress.notice);
        }
    }

    fn read_until_exit(shell: &mut ShellSession, deadline: Instant) -> (Vec<u8>, i32) {
        let mut output = Vec::new();
        loop {
            let progress = wait_for_output(shell, deadline, |batch| {
                output.extend_from_slice(batch.as_bytes());
            })
            .expect("timed out waiting for PTY exit");
            match progress.notice {
                Some(OutputNotice::Exited {
                    code: Some(code), ..
                }) => return (output, code),
                Some(notice) => panic!("unexpected PTY notice: {notice:?}"),
                None => {}
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn chat_control_rejects_real_queued_inflight_and_closed_writer_without_retry() {
        let argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            "stty raw -echo; printf READY; sleep 30".into(),
        ];
        let mut shell = ShellSession::new_with_cwd(
            80,
            24,
            None,
            None,
            None,
            Some(&argv),
            crate::runtime_wake::egui_wake(eframe::egui::Context::default()),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        wait_for_ready(&mut shell, deadline);
        // A real FIFO write becomes in-flight while its PTY lock is held.
        let pty = shell.actor.test_transport();
        shell.write(b"older").unwrap();
        assert!(!shell.try_write_control_byte(3, |_| panic!("must reject before validation")));
        while shell.actor.test_input_counts().0 != 0 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(shell.actor.test_input_counts().1, 1);
        assert!(
            !shell.try_write_control_byte(3, |_| panic!("in-flight bytes still own the writer"))
        );
        drop(pty);
        while shell.actor.test_input_counts().1 != 0 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        shell.actor.test_close_input();
        let terminal = Arc::new(Mutex::new(crate::terminal::TerminalState::new(80, 24)));
        terminal
            .lock()
            .process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let action = crate::terminal::ChatInputAction::Submit {
            sequence: terminal.lock().command_records().back().unwrap().sequence,
        };
        let mut session = crate::session::Session::new_with_session_id(
            "closed".into(),
            vec![],
            terminal,
            shell,
            "closed".into(),
        );
        let foreground = session.shell.foreground_pgid().unwrap();
        assert!(!session.try_chat_input(
            action,
            b"\r",
            foreground,
            None,
            &crate::session_manager::SessionInputBarriers::default(),
            &crate::session_manager::ProtocolResponseSender::new(eframe::egui::Context::default())
        ));
        assert!(session.pending_input.is_empty());
        assert!(
            action.is_current(&session.terminal.lock()),
            "closed writer never arms the Run latch"
        );
    }

    #[cfg(unix)]
    #[test]
    fn chat_control_requires_nonblocking_before_validation() {
        let argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            "stty raw -echo; printf READY; read line; printf DONE".into(),
        ];
        let mut shell = ShellSession::new_with_cwd(
            80,
            24,
            None,
            None,
            None,
            Some(&argv),
            crate::runtime_wake::egui_wake(eframe::egui::Context::default()),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        wait_for_ready(&mut shell, deadline);
        let terminal = Arc::new(Mutex::new(crate::terminal::TerminalState::new(80, 24)));
        terminal
            .lock()
            .process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let action = crate::terminal::ChatInputAction::Submit {
            sequence: terminal.lock().command_records().back().unwrap().sequence,
        };
        let mut session = crate::session::Session::new_with_session_id(
            "nonblocking".into(),
            vec![],
            terminal,
            shell,
            "nonblocking".into(),
        );
        let foreground = session.shell.foreground_pgid().unwrap();
        let barriers = crate::session_manager::SessionInputBarriers::default();
        let responses =
            crate::session_manager::ProtocolResponseSender::new(eframe::egui::Context::default());
        let set_flags = |flags: libc::c_int| {
            let pty = session.shell.actor.test_transport();
            // SAFETY: the live guard pins this idle fixture's master fd.
            assert_eq!(
                unsafe { libc::fcntl(pty.master_fd(), libc::F_SETFL, flags) },
                0
            );
        };
        let flags = {
            let pty = session.shell.actor.test_transport();
            // SAFETY: the live guard pins the descriptor queried here.
            unsafe { libc::fcntl(pty.master_fd(), libc::F_GETFL) }
        };
        assert!(flags >= 0 && flags & libc::O_NONBLOCK != 0);
        // No queued input and no output after READY: never fill this PTY or
        // intentionally attempt a blocking write. A red-test callback also
        // returns false, so a missing admission guard cannot issue a syscall.
        set_flags(flags & !libc::O_NONBLOCK);
        let called = std::cell::Cell::new(false);
        let accepted = session.shell.try_write_control_byte(b'\r', |_| {
            called.set(true);
            false
        });
        set_flags(flags);
        assert!(!accepted);
        assert!(
            !called.get(),
            "reject before lifecycle/foreground validation"
        );

        set_flags(flags & !libc::O_NONBLOCK);
        let accepted =
            session.try_chat_input(action, b"\r", foreground, None, &barriers, &responses);
        {
            let pty = session.shell.actor.test_transport();
            // SAFETY: restore before assertions; the live guard pins the fd.
            assert_eq!(
                unsafe { libc::fcntl(pty.master_fd(), libc::F_SETFL, flags) },
                0
            );
        }
        assert!(!accepted);
        assert!(session.pending_input.is_empty());
        assert!(action.is_current(&session.terminal.lock()));
        assert_eq!(session.shell.actor.test_input_counts().1, 0);
        assert_eq!(session.shell.actor.test_input_counts().0, 0);
        assert!(session.try_chat_input(action, b"\r", foreground, None, &barriers, &responses));
        assert!(!action.is_current(&session.terminal.lock()));
    }

    #[cfg(unix)]
    #[test]
    fn chat_control_kernel_backpressure_has_no_delayed_suffix_or_retry() {
        let argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            "stty raw -echo; printf READY; sleep 30".into(),
        ];
        let mut shell = ShellSession::new_with_cwd(
            80,
            24,
            None,
            None,
            None,
            Some(&argv),
            crate::runtime_wake::egui_wake(eframe::egui::Context::default()),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        wait_for_ready(&mut shell, deadline);
        // Saturate the real nonblocking kernel PTY while its child is not
        // reading. This fixture holds the same descriptor lock as production.
        for _ in 0..3 {
            let mut pty = shell.actor.test_transport();
            let mut written = 0;
            loop {
                match pty.write(b"x") {
                    Ok(crate::pty::WriteOutcome::Written(1)) => written += 1,
                    Ok(crate::pty::WriteOutcome::WouldBlock) => break,
                    result => panic!("unexpected fill outcome: {result:?}"),
                }
                assert!(written < 1_000_000);
            }
            drop(pty);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!shell.try_write_control_byte(b'\r', |_| true));
        assert_eq!(shell.actor.test_input_counts().1, 0);
        assert_eq!(shell.actor.test_input_counts().0, 0);
    }

    #[cfg(unix)]
    #[test]
    fn reader_closes_every_writer_before_the_ui_observes_exit() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'FINAL_TAIL'; exit 0".to_string(),
        ];
        let mut session = ShellSession::new_with_cwd(
            80,
            24,
            Some("/tmp"),
            None,
            None,
            Some(&argv),
            crate::runtime_wake::egui_wake(eframe::egui::Context::default()),
        )
        .unwrap();

        let cloned_writer = session.write_sender();
        let deadline = Instant::now() + Duration::from_secs(2);
        while session.input_drained() != Err(ShellWriteError::Closed) {
            assert!(Instant::now() < deadline, "writer survived child exit");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            cloned_writer.try_send(b"ignored".to_vec()),
            Err(ShellWriteError::Closed)
        );
        loop {
            let paused = session.poll_output(64, || false, |_, _| panic!("PTY reply gate held"));
            assert!(paused.notice.is_none());
            assert_eq!(paused.bytes_applied, 0);
            assert!(!session.output_finished());
            if paused.parked {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the parked tail"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let (received, code) = read_until_exit(&mut session, deadline);
        assert_eq!(received, b"FINAL_TAIL");
        assert_eq!(code, 0);
        assert!(session.output_finished());
        assert!(matches!(
            session.write(b"ignored"),
            Err(ShellWriteError::Closed)
        ));
        assert!(matches!(
            cloned_writer.try_send(b"ignored".to_vec()),
            Err(ShellWriteError::Closed)
        ));
        assert!(!session.has_pending_output());
    }

    #[cfg(unix)]
    #[test]
    fn exited_leader_cannot_leave_same_group_descendants_running() {
        let root = std::env::temp_dir().join(format!(
            "ember-pty-descendant-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let escaped_marker = root.join("descendant-escaped");
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "(trap '' HUP TERM; sleep 0.6; printf escaped > \"$1\") & exit 0".to_string(),
            "ember-descendant-test".to_string(),
            escaped_marker.to_string_lossy().into_owned(),
        ];
        let mut session = ShellSession::new_with_cwd(
            80,
            24,
            Some("/tmp"),
            None,
            None,
            Some(&argv),
            crate::runtime_wake::egui_wake(eframe::egui::Context::default()),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        assert_eq!(read_until_exit(&mut session, deadline).1, 0);

        // The old reap-first path published Exit while the background child
        // remained alive. Give that child enough time to prove it escaped;
        // the cleaned private process group can never create this marker.
        std::thread::sleep(Duration::from_millis(850));
        assert!(!escaped_marker.exists());
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn transient_pty_hangup_waits_for_real_exit_and_recovers_output() {
        use std::os::unix::fs::PermissionsExt;

        let unique = format!(
            "ember-pty-hangup-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let script_path = std::env::temp_dir().join(unique);
        std::fs::write(
            &script_path,
            b"#!/bin/sh\nexec 0>&- 1>&- 2>&-\nsleep 0.25\nexec 0</dev/tty 1>/dev/tty 2>/dev/tty\nprintf recovered\nsleep 0.05\nexit 7\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script_path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script_path, permissions).unwrap();

        let script = script_path.to_string_lossy().into_owned();
        // Cargo runs the suite as threads of one process: a sibling test that
        // forks while this script is still open for writing hands its child a
        // writable descriptor to it, and our exec then fails with ETXTBSY
        // until that child execs. The window is a neighbour's timing, not this
        // test's subject, so retry briefly instead of failing the run.
        let spawn_deadline = Instant::now() + Duration::from_secs(5);
        let mut session = loop {
            match ShellSession::new(
                80,
                24,
                Some(&script),
                crate::runtime_wake::egui_wake(eframe::egui::Context::default()),
            ) {
                Ok(session) => break session,
                Err(error)
                    if error.contains("Text file busy") && Instant::now() < spawn_deadline =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("could not start the hangup script: {error}"),
            }
        };

        // The old implementation emitted Exit(-1) after a 30 ms reap window,
        // causing the live child to be dropped and killed before it could reopen.
        let premature = wait_for_output(
            &mut session,
            Instant::now() + Duration::from_millis(100),
            |_| panic!("output while the slave was closed"),
        );
        assert!(premature.is_none());
        let deadline = Instant::now() + Duration::from_secs(2);
        let (output, exit_code) = read_until_exit(&mut session, deadline);
        assert_eq!(output, b"recovered");
        assert_eq!(exit_code, 7);

        drop(session);
        let _ = std::fs::remove_file(script_path);
    }
}
