//! Frozen pre-extraction native h/l dispatch and application mode callbacks.
//! Neither reference path calls NativeModeCommand or its parameter mutation.
use super::*;
use jterm_engine::mode_mutation::{apply_ember_mode, ModeNamespace};
use std::cell::RefCell;
use std::collections::BTreeMap;
type Snapshot = BTreeMap<String, String>;
thread_local! { static TRACE: RefCell<Option<Vec<Snapshot>>> = const { RefCell::new(None) }; }
pub(super) fn observe(t: &TerminalState) {
    TRACE.with(|slot| {
        if let Some(trace) = slot.borrow_mut().as_mut() {
            trace.push(snapshot(t));
        }
    });
}
fn snapshot(t: &TerminalState) -> Snapshot {
    let mut out = super::super::alternate_parity_tests::snapshot(t);
    out.insert("dynamic_fg".into(), format!("{:?}", t.colors.dynamic_fg));
    out.insert("dynamic_bg".into(), format!("{:?}", t.colors.dynamic_bg));
    out.insert(
        "dynamic_cursor_color".into(),
        format!("{:?}", t.colors.dynamic_cursor_color),
    );
    out.insert(
        "window_title".into(),
        format!("{:?}", t.titles.window_title),
    );
    out.insert("icon_title".into(), format!("{:?}", t.titles.icon_title));
    out.insert("title_stack".into(), format!("{:?}", t.titles.title_stack));
    macro_rules! host {
        ($($f:ident),* $(,)?) => {$(out.insert(stringify!($f).into(), format!("{:?}", t.$f));)*};
    }
    out.insert(
        "pending_osc52_clipboard_set".into(),
        format!("{:?}", t.osc52.pending_set),
    );
    out.insert(
        "pending_osc52_clipboard_queries".into(),
        format!("{:?}", t.osc52.pending_queries),
    );
    out.insert("sync_output_active".into(), format!("{:?}", t.vt.sync_output.active));
    host!(
        current_working_dir,
        ime_enabled,
        preedit_text,
        preedit_cursor,
        host_window_focused,
        pending_notifications,
        pending_bell,
        chat_pending_submit,
        chat_native_takeover_sequence,
        inline_cli_launch_floor,
        inline_cli_command_scope,
        next_projection_plan_revision
    );
    out.insert(
        "sync_start_present".into(),
        t.vt.sync_output.started.is_some().to_string(),
    );
    out.insert(
        "sync_age_ms".into(),
        format!(
            "{:?}",
            t.vt.sync_output.started.map(|start| t
                .sync_output_clock
                .now()
                .saturating_duration_since(start)
                .as_millis())
        ),
    );
    out.insert(
        "paste_expiry_ms".into(),
        format!(
            "{:?}",
            t.osc5522.pending_paste_grant.as_ref().map(|grant| grant
                .expires_at
                .saturating_duration_since(t.sync_output_clock.now())
                .as_millis())
        ),
    );
    out.insert("parser".into(), format!("{:?}", t.vt.parser.snapshot()));
    out.insert(
        "kitty_image_count".into(),
        t.kitty_graphics.image_count().to_string(),
    );
    out.insert(
        "paste_grant".into(),
        format!(
            "{:?}",
            t.osc5522.pending_paste_grant.as_ref().map(|g| (
                &g.token,
                g.offered_mimes
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
            ))
        ),
    );
    out
}
fn seeded(seed: usize) -> TerminalState {
    let mut t = super::super::alternate_parity_tests::seeded(seed);
    let now = std::time::Instant::now();
    t.sync_output_clock = super::super::SyncOutputClock::new(move || now);
    if seed & 1 != 0 {
        t.process_input(b"\x1b[?1049h");
    }
    if seed & 2 != 0 {
        t.process_input(b"\x1b[?2026h");
    }
    t.process_input(
        b"\x1b[?1;66;1000;1004;2004h\x1b[?7;25;1007l\x1b[4h\x1b[?6h\x1b[2;4r\x1b[2;3H\x1b7",
    );
    t.vt.screen.cursor_shape = match seed % 3 {
        0 => CursorShape::Block,
        1 => CursorShape::Beam,
        _ => CursorShape::Underline,
    };
    t.vt.screen.current_flags.set_protected(seed & 1 != 0);
    t.vt.screen.g1_charset = Charset::DecSpecialGraphics;
    t.vt.screen.active_charset = Charset::DecSpecialGraphics;
    t.vt.screen.current_fg = Color::Rgb(1, 2, 3);
    t.vt.screen.current_bg = Color::Indexed(201);
    t.global_bg = Color::Blue;
    t.vt.screen.current_flags.set_underline(UnderlineStyle::Curly);
    t.vt.screen.pending_wrap = true;
    t.output_buffer = b"existing-reply".to_vec();
    // Modes 4/6/1016 have no TerminalModes storage slots. Use the real
    // authoritative booleans; true values detect mistaken protocol reads.
    t.vt.screen.origin_mode = seed & 1 == 0;
    t.vt.screen.insert_mode = seed & 2 == 0;
    super::super::structural_parity_tests::prime(&mut t);
    assert!(t.command_records.back().is_some_and(
        |record| !record.complete && record.state == super::super::CommandState::Running
    ));
    assert!(t.chat_native_takeover());
    t.titles.window_title = "Mode mutation window".into();
    t.titles.icon_title = "Mode mutation icon".into();
    t.titles.title_stack = vec![(Some("saved icon".into()), Some("saved window".into()))];
    t.current_working_dir = Some("/baseline/mode-mutation".into());
    t.preedit_text = "输入中".into();
    t.preedit_cursor = 1;
    t.osc52.pending_set = Some("clipboard sentinel".into());
    t.osc52.pending_queries.push_back(b"\x07");
    t.pending_notifications = vec![("title sentinel".into(), "body sentinel".into())];
    t.colors.dynamic_fg = Some((1, 2, 3));
    t.colors.dynamic_bg = Some((4, 5, 6));
    t.colors.dynamic_cursor_color = Some((7, 8, 9));
    t.pending_bell = true;
    t.osc5522.pending_paste_grant = Some(super::super::PendingPasteGrant {
        token: "mode-mutation-grant".into(),
        offered_mimes: ["text/plain".to_owned()].into_iter().collect(),
        expires_at: now + std::time::Duration::from_secs(600),
    });
    // Make the query's observer call visible even without other VT mutations.
    t.chat_native_takeover_sequence = None;
    t.host_window_focused = seed & 2 == 0;
    t.vt.protocol.modes_mut().remove(&1004);
    t.sync_output_clock =
        super::super::SyncOutputClock::new(move || now + std::time::Duration::from_millis(5));
    t
}

impl TerminalState {
    pub(super) fn frozen_command_set_mode(&mut self, mode: u16) {
        if apply_ember_mode(
            &mut self.vt.screen,
            &mut self.vt.protocol,
            ModeNamespace::DecPrivate,
            mode,
            true,
        ) {
            return;
        }
        match mode {
            1004 => {
                // Focus event reporting (CSI I / CSI O). If the window is
                // already focused, report that immediately so a client that
                // probes then waits for the first I is not stuck until Alt-Tab.
                let newly = !self.vt.protocol.modes().contains(&1004);
                self.vt.protocol.modes_mut().insert(1004);
                if newly && self.host_window_focused {
                    self.output_buffer.extend_from_slice(b"\x1b[I");
                }
            }
            47 => {
                // 备用屏(无保存光标、无清屏)
                self.enter_alt_screen(false, false);
                self.vt.protocol.modes_mut().insert(47);
            }
            1047 => {
                // 备用屏(切入时清屏)
                self.enter_alt_screen(false, true);
                self.vt.protocol.modes_mut().insert(1047);
            }
            1048 => {
                // 仅保存光标(等价 DECSC). It shares DECSC's slot, as in xterm,
                // not the one 1049 keeps for the primary screen, so a 1048
                // inside a 1049 session cannot move the cursor 1049l restores.
                self.save_cursor_state();
                self.vt.protocol.modes_mut().insert(1048);
            }
            1049 => {
                // 备用屏:保存光标 + 切入 + 清屏
                self.enter_alt_screen(true, true);
                self.vt.protocol.modes_mut().insert(1049);
            }
            2026 => {
                // Synchronized output: suppress rendering until cleared
                self.vt.protocol.modes_mut().insert(2026);
                self.vt.sync_output.active = true;
                self.vt.sync_output.started = Some(self.sync_output_clock.now());
            }
            5522 => {
                self.vt.protocol.modes_mut().insert(5522);
            }
            _ => unreachable!("engine returned an unknown host mode"),
        }
    }
    /// ANSI modes have no host lifecycle hooks.
    pub(super) fn frozen_command_set_ansi_mode(&mut self, mode: u16) {
        apply_ember_mode(
            &mut self.vt.screen,
            &mut self.vt.protocol,
            ModeNamespace::Ansi,
            mode,
            true,
        );
    }

    pub(super) fn frozen_command_reset_ansi_mode(&mut self, mode: u16) {
        apply_ember_mode(
            &mut self.vt.screen,
            &mut self.vt.protocol,
            ModeNamespace::Ansi,
            mode,
            false,
        );
    }

    pub(super) fn frozen_command_reset_mode(&mut self, mode: u16) {
        if apply_ember_mode(
            &mut self.vt.screen,
            &mut self.vt.protocol,
            ModeNamespace::DecPrivate,
            mode,
            false,
        ) {
            return;
        }
        match mode {
            1004 => {
                // Disable focus event reporting
                self.vt.protocol.modes_mut().remove(&1004);
            }
            5522 => {
                // A paste capability is valid only while the application keeps
                // paste-event mode enabled. Revocation must be immediate.
                self.vt.protocol.modes_mut().remove(&5522);
                self.osc5522.pending_paste_grant = None;
            }
            47 => {
                // 退出备用屏(不恢复光标)
                self.exit_alt_screen(false);
                self.vt.protocol.modes_mut().remove(&47);
            }
            1047 => {
                // 退出备用屏(不恢复光标)
                self.exit_alt_screen(false);
                self.vt.protocol.modes_mut().remove(&1047);
            }
            1048 => {
                // 仅恢复光标(等价 DECRC)
                self.restore_cursor_state();
                self.vt.protocol.modes_mut().remove(&1048);
            }
            1049 => {
                // 退出备用屏并恢复进入前保存的光标
                self.exit_alt_screen(true);
                self.vt.protocol.modes_mut().remove(&1049);
            }
            2026 => {
                // End synchronized output: force full render
                if self.vt.screen.use_alt_buffer {
                    self.archive_visible_screen_to_scrollback_with_options(true, true);
                } else {
                    self.last_synced_primary_screen_snapshot =
                        self.visible_screen_snapshot().unwrap_or_default();
                }
                self.vt.protocol.modes_mut().remove(&2026);
                self.vt.sync_output.active = false;
                self.vt.sync_output.started = None;
                self.dirty_region.mark_all(self.vt.screen.grid.rows());
                self.mark_rows_dirty(0, self.vt.screen.grid.rows().saturating_sub(1));
            }
            _ => unreachable!("engine returned an unknown host mode"),
        }
    }
    fn frozen_mode_command(&mut self, params: &[u16], cmd: char, private_prefix: Option<u8>) {
        match cmd {
            'h' => {
                // 区分 DEC 私有模式(CSI ? Pn h)与 ANSI 模式(CSI Pn h)。
                // 否则 CSI 7h 会被误当作 DECAWM(?7),CSI 4h(IRM)也会落空。
                match private_prefix {
                    Some(b'?') => {
                        for &mode in params {
                            self.frozen_command_set_mode(mode);
                            observe(self);
                        }
                    }
                    None => {
                        for &mode in params {
                            self.frozen_command_set_ansi_mode(mode);
                            observe(self);
                        }
                    }
                    _ => {}
                }
            }
            'l' => match private_prefix {
                Some(b'?') => {
                    for &mode in params {
                        self.frozen_command_reset_mode(mode);
                        observe(self);
                    }
                }
                None => {
                    for &mode in params {
                        self.frozen_command_reset_ansi_mode(mode);
                        observe(self);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        self.observe_chat_native_takeover();
    }
}
fn run(
    mut t: TerminalState,
    params: &[u16],
    cmd: char,
    prefix: Option<u8>,
    intermediate: &[u8],
    original: bool,
) -> (Snapshot, Vec<Snapshot>) {
    TRACE.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
    if original {
        t.frozen_mode_command(params, cmd, prefix);
    } else {
        t.handle_escape_sequence(params, cmd, prefix, intermediate);
    }
    let trace = TRACE.with(|slot| slot.borrow_mut().take().unwrap());
    (snapshot(&t), trace)
}

#[test]
fn mode_command_matches_frozen_parameter_effect_order() {
    for seed in 0..4 {
        for cmd in ['h', 'l'] {
            for prefix in [None, Some(b'?'), Some(b'>'), Some(b'=')] {
                for intermediate in [b"".as_slice(), b"!", b" "] {
                    for params in [
                        vec![],
                        vec![4, 6, 7, 25],
                        vec![1004, 1004, 5522, 1048],
                        vec![6, 1049],
                        vec![1049, 6],
                        vec![47, 1047, 1049, 1048, 6, 7],
                        vec![2026, 2026, 1049, 2026],
                        vec![5522, 1004, 2026, 0, 65535],
                        vec![1000, 1002, 1000, 1006, 1015],
                    ] {
                        assert_eq!(run(seeded(seed), &params, cmd, prefix, intermediate, false), run(seeded(seed), &params, cmd, prefix, intermediate, true), "seed={seed} cmd={cmd} prefix={prefix:?} intermediate={intermediate:?} params={params:?}");
                    }
                }
            }
        }
    }
}
