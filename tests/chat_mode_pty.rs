//! Opt-in acceptance test using one real, long-lived interactive Bash PTY.
#![cfg(target_os = "linux")]

use ember::session::Session;
use ember::shell::{ShellEvent, ShellSession};
use ember::terminal::{
    ChatInputAction, HistoryProjection, ProjectionPolicy, ProjectionViewState, TerminalState,
};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn button(session: &mut Session, action: ChatInputAction, bytes: &[u8]) -> bool {
    let foreground = session.shell.foreground_pgid().unwrap_or(0);
    session.try_chat_input(
        action,
        bytes,
        foreground,
        None,
        &ember::session_manager::SessionInputBarriers::default(),
        &ember::session_manager::ProtocolResponseSender::new(egui::Context::default()),
    )
}

fn flush(session: &mut Session) {
    session
        .shell
        .write(&session.pending_input)
        .expect("existing PTY writer accepts queued input");
    session.pending_input.clear();
}

fn poll_until(session: &mut Session, predicate: impl Fn(&TerminalState, &[u8]) -> bool) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut output = Vec::new();
    let mut policy = ProjectionPolicy::new();
    policy.set_chat_mode(true);
    policy.set_block_spacing(true);
    policy.set_block_chrome(true, 0);
    let mut state = ProjectionViewState::new();
    loop {
        {
            let mut terminal = session.terminal.lock();
            let raw_cursor = terminal.get_cursor_pos();
            let view = terminal.projected_viewport_with_state(
                HistoryProjection::identity(),
                true,
                &policy,
                &mut state,
            );
            assert_eq!(view.application_cell(view.cursor()), Some(raw_cursor));
            if predicate(&terminal, &output) {
                return output;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for real shell; captured {} bytes",
            output.len()
        );
        match session
            .shell
            .events()
            .recv_timeout(Duration::from_millis(50))
        {
            Ok(ShellEvent::Output(bytes)) => {
                for fragment in bytes.fragments() {
                    output.extend_from_slice(fragment.bytes);
                    session.terminal.lock().process_input(fragment.bytes);
                }
            }
            Ok(ShellEvent::Error(error)) => panic!("PTY failed: {error}"),
            Ok(ShellEvent::Exit(code)) => panic!("interactive shell exited unexpectedly: {code}"),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("PTY event channel ended: {error}"),
        }
    }
}

fn submit(session: &mut Session, command: &[u8]) -> u64 {
    assert!(session.queue_input(command));
    flush(session);
    poll_until(session, |terminal, bytes| {
        terminal.shell_is_prompt_ready() && !bytes.is_empty()
    });
    let sequence = session
        .terminal
        .lock()
        .command_records()
        .back()
        .unwrap()
        .sequence;
    let action = ChatInputAction::Submit { sequence };
    assert!(button(session, action, b"\r"));
    flush(session);
    assert!(
        !button(session, action, b"\r"),
        "one button click never sends a second Enter before shell acknowledgment"
    );
    sequence
}

fn ready_after(session: &mut Session, sequence: u64) -> Vec<u8> {
    poll_until(session, |terminal, _| {
        terminal.shell_is_prompt_ready()
            && terminal
                .command_records()
                .back()
                .is_some_and(|record| record.sequence > sequence)
    })
}

#[test]
#[ignore = "requires a real local Bash PTY; run explicitly for Chat acceptance"]
fn chat_mode_keeps_cwd_environment_completion_multiline_hidden_input_and_interrupt_in_one_pty() {
    let shell = ShellSession::new_with_cwd(
        100,
        30,
        None,
        None,
        Some("/bin/bash"),
        Some(&[
            "/bin/bash".into(),
            "--noprofile".into(),
            "--norc".into(),
            "-i".into(),
        ]),
        egui::Context::default(),
    )
    .expect("real Bash PTY");
    let terminal = Arc::new(Mutex::new(TerminalState::new(100, 30)));
    let mut session = Session::new_with_session_id(
        "chat-acceptance".into(),
        Vec::new(),
        terminal,
        shell,
        "chat-acceptance".into(),
    );
    let pid = session.get_shell_pid();
    // Bash's native prompt hooks emit standard OSC 133. They do not create a
    // second executor and all later commands go through Session's input FIFO.
    assert!(session.queue_input(b"PS1=$'\\[\\e]133;A\\a\\]$ \\[\\e]133;B\\a\\]'; PS0=$'\\e]133;C\\a'; PROMPT_COMMAND='printf \"\\033]133;D;%s\\007\" \"$?\"'\r"));
    flush(&mut session);
    poll_until(&mut session, |terminal, _| terminal.shell_is_prompt_ready());

    // The dock is display-only: the real PTY retains the full configured size.
    let sequence = submit(&mut session, b"printf 'SIZE:'; stty size");
    let output = ready_after(&mut session, sequence);
    assert!(String::from_utf8_lossy(&output).contains("SIZE:30 100"));

    let sequence = submit(&mut session, b"cd /tmp; export EMBER_CHAT_FIXTURE=samepty");
    ready_after(&mut session, sequence);
    let sequence = submit(
        &mut session,
        b"printf 'PERSIST:%s:%s\\n' \"$PWD\" \"$EMBER_CHAT_FIXTURE\"",
    );
    let output = ready_after(&mut session, sequence);
    assert!(String::from_utf8_lossy(&output).contains("PERSIST:/tmp:samepty"));
    assert_eq!(session.get_shell_pid(), pid);

    let sequence = submit(&mut session, b"ember_chat_fixture_complete() { :; }");
    ready_after(&mut session, sequence);
    assert!(session.queue_input(b"ember_chat_fixture_comp\t"));
    flush(&mut session);
    poll_until(&mut session, |terminal, bytes| {
        terminal.shell_is_prompt_ready() && String::from_utf8_lossy(bytes).contains("lete")
    });
    let sequence = session
        .terminal
        .lock()
        .command_records()
        .back()
        .unwrap()
        .sequence;
    assert!(button(
        &mut session,
        ChatInputAction::Submit { sequence },
        b"\r"
    ));
    flush(&mut session);
    ready_after(&mut session, sequence);

    let sequence = submit(&mut session, b"for part in one two; do");
    poll_until(&mut session, |_, bytes| {
        bytes.windows(2).any(|pair| pair == b"> ")
    });
    assert!(session.queue_input(b"printf 'MULTI:%s\\n' \"$part\"; done\r"));
    flush(&mut session);
    let output = ready_after(&mut session, sequence);
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("MULTI:one") && text.contains("MULTI:two"));

    let sequence = submit(
        &mut session,
        b"read -s -p 'Secret:' token; printf '\\nLENGTH:%s\\n' \"${#token}\"",
    );
    poll_until(&mut session, |terminal, bytes| {
        terminal.chat_surface() == jterm_core::chat_mode::ChatSurface::Running
            && String::from_utf8_lossy(bytes).contains("Secret:")
    });
    assert!(!button(
        &mut session,
        ChatInputAction::Submit { sequence },
        b"\r"
    ));
    assert!(session.queue_input(b"synthetic\r"));
    flush(&mut session);
    let output = ready_after(&mut session, sequence);
    assert!(String::from_utf8_lossy(&output).contains("LENGTH:9"));
    assert!(
        !String::from_utf8_lossy(&output).contains("synthetic"),
        "hidden input must not be recorded as terminal output"
    );

    let sequence = submit(&mut session, b"sleep 30");
    poll_until(&mut session, |terminal, _| {
        terminal.chat_surface() == jterm_core::chat_mode::ChatSurface::Running
    });
    assert!(button(
        &mut session,
        ChatInputAction::Interrupt { sequence },
        b"\x03"
    ));
    assert_eq!(
        session.terminal.lock().chat_surface(),
        jterm_core::chat_mode::ChatSurface::Running
    );
    flush(&mut session);
    ready_after(&mut session, sequence);
    assert!(
        !button(
            &mut session,
            ChatInputAction::Interrupt { sequence },
            b"\x03"
        ),
        "a completed command's interrupt cannot leak into the next prompt"
    );
    let terminal = session.terminal.lock();
    let interrupted = terminal
        .command_records()
        .iter()
        .find(|record| record.sequence == sequence)
        .unwrap();
    assert!(interrupted.complete);
    assert_eq!(interrupted.exit_code, Some(130));
    assert_eq!(session.get_shell_pid(), pid);
}

#[test]
#[ignore = "requires real Bash/Python PTYs; run explicitly for Chat safety acceptance"]
fn chat_review_foreground_spoof_and_barrier_never_deliver_a_late_control() {
    use ember::session_manager::{SessionInputBarriers, SessionManager};
    use ember::terminal::CommandState;
    let shell = ShellSession::new_with_cwd(
        100,
        30,
        None,
        None,
        Some("/bin/bash"),
        Some(&[
            "/bin/bash".into(),
            "--noprofile".into(),
            "--norc".into(),
            "-i".into(),
        ]),
        egui::Context::default(),
    )
    .unwrap();
    let terminal = Arc::new(Mutex::new(TerminalState::new(100, 30)));
    let session =
        Session::new_with_session_id("review".into(), vec![], terminal, shell, "review".into());
    let mut manager =
        SessionManager::new(session, egui::Context::default(), Some("/bin/bash".into()));
    let responses = manager.protocol_response_sender(0).unwrap();
    let barriers = SessionInputBarriers::default();
    {
        let s = manager.get_session_mut(0).unwrap();
        s.shell.write(b"PS1=$'\\[\\e]133;A\\a\\]$ \\[\\e]133;B\\a\\]'; PS0=$'\\e]133;C\\a'; PROMPT_COMMAND='printf \"\\033]133;D;%s\\007\" \"$?\"'\r").unwrap();
        poll_until(s, |t, _| t.shell_is_prompt_ready());
        s.shell.write(b"python3 -c 'import sys; print(\"\\x1b]133;A\\x07APP> \\x1b]133;B\\x07\", end=\"\", flush=True); input(); print(\"NATIVE_KEYBOARD_REACHED_APP\")'\r").unwrap();
        poll_until(s, |t, b| {
            t.shell_is_prompt_ready() && String::from_utf8_lossy(b).contains("APP> ")
        });
        let seq = s.terminal.lock().command_records().back().unwrap().sequence;
        let action = ChatInputAction::Submit { sequence: seq };
        let foreground = s.shell.foreground_pgid().unwrap();
        assert_ne!(foreground, s.get_shell_pid());
        assert!(action.is_current(&s.terminal.lock()));
        let renderer = ember::ui::TerminalRenderer::new(
            16.0,
            12.0,
            1.3,
            ember::config::ScrollbarVisibility::Always,
            ember::theme::Theme::default(),
        );
        let bytes =
            renderer.chat_action_bytes(&egui::Context::default(), &s.terminal.lock(), action);
        assert!(!s.try_chat_input(action, &bytes, foreground, None, &barriers, &responses));
        assert!(s.pending_input.is_empty());
        assert!(
            action.is_current(&s.terminal.lock()),
            "rejection must not arm the Run latch"
        );
        // Normal keyboard input retains native ownership and reaches input().
        assert!(s.queue_input(b"\r"));
        flush(s);
        let output = poll_until(s, |t, b| {
            t.shell_is_prompt_ready()
                && t.command_records().back().unwrap().sequence > seq
                && String::from_utf8_lossy(b).contains("NATIVE_KEYBOARD_REACHED_APP")
        });
        assert!(String::from_utf8_lossy(&output).contains("NATIVE_KEYBOARD_REACHED_APP"));
        s.shell.write(b"sleep 0.25\rsleep 0.35\r").unwrap();
        poll_until(s, |t, _| {
            t.command_records()
                .back()
                .is_some_and(|r| r.state == CommandState::Running)
        });
    }
    let old_seq = manager.sessions()[0]
        .terminal
        .lock()
        .command_records()
        .back()
        .unwrap()
        .sequence;
    let old_foreground = manager.sessions()[0].shell.foreground_pgid().unwrap();
    let action = ChatInputAction::Interrupt { sequence: old_seq };
    let barrier = barriers.acquire("review".into());
    assert!(!manager.get_session_mut(0).unwrap().try_chat_input(
        action,
        b"\x03",
        old_foreground,
        None,
        &barriers,
        &responses
    ));
    assert!(manager.sessions()[0].pending_input.is_empty());
    let second = manager.new_session(Some("pump-control".into()), None, 80, 24, 1000);
    assert!(manager.switch_session(second));
    let deadline = Instant::now() + Duration::from_secs(5);
    let new_seq = loop {
        manager.pump_inactive_sessions(1 << 20, &[0], None, &barriers, true);
        let t = manager.sessions()[0].terminal.lock();
        let r = t.command_records().back().unwrap();
        if r.sequence > old_seq && r.state == CommandState::Running {
            break r.sequence;
        }
        drop(t);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(!action.is_current(&manager.sessions()[0].terminal.lock()));
    drop(barrier);
    assert!(!manager.get_session_mut(0).unwrap().try_chat_input(
        action,
        b"\x03",
        old_foreground,
        None,
        &barriers,
        &responses
    ));
    let new_exit = loop {
        manager.pump_inactive_sessions(1 << 20, &[0], None, &barriers, true);
        let t = manager.sessions()[0].terminal.lock();
        let r = t
            .command_records()
            .iter()
            .find(|r| r.sequence == new_seq)
            .unwrap();
        if r.complete {
            break r.exit_code;
        }
        drop(t);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(
        new_exit,
        Some(0),
        "the next real command must finish normally, never receive stale Ctrl+C"
    );
    let s = manager.get_session_mut(0).unwrap();
    poll_until(s, |t, _| t.shell_is_prompt_ready());
    let sequence = s.terminal.lock().command_records().back().unwrap().sequence;
    let submit = ChatInputAction::Submit { sequence };
    let foreground = s.shell.foreground_pgid().unwrap();
    assert!(!s.try_chat_input(submit, b"\x1b[13u", foreground, None, &barriers, &responses));
    assert!(!s.try_chat_input(submit, b"\r", foreground + 1, None, &barriers, &responses));
    assert!(!s.try_chat_input(
        submit,
        b"\r",
        foreground,
        Some("review"),
        &barriers,
        &responses
    ));
    responses.try_enqueue(b"response".to_vec()).unwrap();
    assert!(!s.try_chat_input(submit, b"\r", foreground, None, &barriers, &responses));
    assert!(s.pending_input.is_empty());
    assert!(
        submit.is_current(&s.terminal.lock()),
        "all rejected admissions leave the latch available"
    );
}
