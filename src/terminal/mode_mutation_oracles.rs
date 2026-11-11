//! Frozen pre-extraction Ember mode mutations at 9ca7891. Do not delegate these copies to the engine.
use super::*;
use std::collections::BTreeMap;
impl TerminalState {
    pub(super) fn frozen_set_mode(&mut self, mode: u16) {
        match mode {
            25 => {
                // Show cursor (mode 25)
                self.protocol.modes_mut().insert(25);
            }
            1004 => {
                // Focus event reporting (CSI I / CSI O). If the window is
                // already focused, report that immediately so a client that
                // probes then waits for the first I is not stuck until Alt-Tab.
                let newly = !self.protocol.modes().contains(&1004);
                self.protocol.modes_mut().insert(1004);
                if newly && self.host_window_focused {
                    self.output_buffer.extend_from_slice(b"\x1b[I");
                }
            }
            2004 => {
                // Bracketed paste mode
                self.protocol.modes_mut().insert(2004);
            }
            1000..=1003 => {
                // Mouse reporting modes
                self.protocol.modes_mut().insert(mode);
            }
            1006 => {
                // SGR mouse reporting format
                self.protocol.modes_mut().insert(mode);
            }
            47 => {
                // 备用屏(无保存光标、无清屏)
                self.enter_alt_screen(false, false);
                self.protocol.modes_mut().insert(47);
            }
            1047 => {
                // 备用屏(切入时清屏)
                self.enter_alt_screen(false, true);
                self.protocol.modes_mut().insert(1047);
            }
            1048 => {
                // 仅保存光标(等价 DECSC). It shares DECSC's slot, as in xterm,
                // not the one 1049 keeps for the primary screen, so a 1048
                // inside a 1049 session cannot move the cursor 1049l restores.
                self.save_cursor_state();
                self.protocol.modes_mut().insert(1048);
            }
            1049 => {
                // 备用屏:保存光标 + 切入 + 清屏
                self.enter_alt_screen(true, true);
                self.protocol.modes_mut().insert(1049);
            }
            2026 => {
                // Synchronized output: suppress rendering until cleared
                self.protocol.modes_mut().insert(2026);
                self.sync_output_active = true;
                self.sync_output_start = Some(self.sync_output_clock.now());
            }
            7 => {
                // Autowrap mode
                self.protocol.modes_mut().insert(7);
            }
            6 => {
                // DECOM - 原点模式:寻址相对滚动区域,光标移到区域原点
                self.screen.origin_mode = true;
                self.screen.cursor_row = self.screen.scroll_region_top;
                self.screen.cursor_col = 0;
            }
            66 => {
                self.protocol.modes_mut().insert(66);
            }
            1007 => {
                self.protocol.modes_mut().insert(1007);
            }
            _ => {
                // Unknown mode, just store it
                self.protocol.modes_mut().insert(mode);
            }
        }
    }

    /// ANSI 标准模式 (CSI Pn h,无 ? 前缀)。目前仅 IRM(4) 有实际效果。
    pub(super) fn frozen_set_ansi_mode(&mut self, mode: u16) {
        if mode == 4 {
            // IRM - 插入替换模式
            self.screen.insert_mode = true;
        }
    }

    pub(super) fn frozen_reset_ansi_mode(&mut self, mode: u16) {
        if mode == 4 {
            self.screen.insert_mode = false;
        }
    }

    pub(super) fn frozen_reset_mode(&mut self, mode: u16) {
        match mode {
            25 => {
                // Hide cursor
                self.protocol.modes_mut().remove(&25);
            }
            1004 => {
                // Disable focus event reporting
                self.protocol.modes_mut().remove(&1004);
            }
            2004 => {
                // Disable bracketed paste mode
                self.protocol.modes_mut().remove(&2004);
            }
            5522 => {
                // A paste capability is valid only while the application keeps
                // paste-event mode enabled. Revocation must be immediate.
                self.protocol.modes_mut().remove(&5522);
                self.pending_paste_grant = None;
            }
            1000..=1003 => {
                // Disable mouse reporting
                self.protocol.modes_mut().remove(&mode);
            }
            1006 => {
                // Disable SGR mouse reporting format
                self.protocol.modes_mut().remove(&mode);
            }
            47 => {
                // 退出备用屏(不恢复光标)
                self.exit_alt_screen(false);
                self.protocol.modes_mut().remove(&47);
            }
            1047 => {
                // 退出备用屏(不恢复光标)
                self.exit_alt_screen(false);
                self.protocol.modes_mut().remove(&1047);
            }
            1048 => {
                // 仅恢复光标(等价 DECRC)
                self.restore_cursor_state();
                self.protocol.modes_mut().remove(&1048);
            }
            1049 => {
                // 退出备用屏并恢复进入前保存的光标
                self.exit_alt_screen(true);
                self.protocol.modes_mut().remove(&1049);
            }
            2026 => {
                // End synchronized output: force full render
                if self.screen.use_alt_buffer {
                    self.archive_visible_screen_to_scrollback_with_options(true, true);
                } else {
                    self.last_synced_primary_screen_snapshot =
                        self.visible_screen_snapshot().unwrap_or_default();
                }
                self.protocol.modes_mut().remove(&2026);
                self.sync_output_active = false;
                self.sync_output_start = None;
                self.dirty_region.mark_all(self.screen.grid.rows());
                self.mark_rows_dirty(0, self.screen.grid.rows().saturating_sub(1));
            }
            7 => {
                // Disable autowrap
                self.protocol.modes_mut().remove(&7);
            }
            6 => {
                // DECOM 关闭:恢复绝对寻址,光标移到屏幕原点
                self.screen.origin_mode = false;
                self.screen.cursor_row = 0;
                self.screen.cursor_col = 0;
            }
            66 => {
                self.protocol.modes_mut().remove(&66);
            }
            1007 => {
                self.protocol.modes_mut().remove(&1007);
            }
            _ => {
                // Unknown mode, just remove it
                self.protocol.modes_mut().remove(&mode);
            }
        }
    }
}
type Snapshot = BTreeMap<String, String>;
fn snapshot(t: &TerminalState) -> Snapshot {
    let mut out = super::super::alternate_parity_tests::snapshot(t);
    out.insert("dynamic_fg".into(), format!("{:?}", t.colors.dynamic_fg));
    out.insert("dynamic_bg".into(), format!("{:?}", t.colors.dynamic_bg));
    out.insert(
        "dynamic_cursor_color".into(),
        format!("{:?}", t.colors.dynamic_cursor_color),
    );
    macro_rules! host {
        ($($f:ident),* $(,)?) => {$(out.insert(stringify!($f).into(), format!("{:?}", t.$f));)*};
    }
    host!(
        sync_output_active,
        window_title,
        icon_title,
        title_stack,
        current_working_dir,
        ime_enabled,
        preedit_text,
        preedit_cursor,
        host_window_focused,
        pending_osc52_clipboard_set,
        pending_osc52_clipboard_queries,
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
        t.sync_output_start.is_some().to_string(),
    );
    out.insert(
        "sync_age_ms".into(),
        format!(
            "{:?}",
            t.sync_output_start.map(|start| t
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
            t.pending_paste_grant.as_ref().map(|grant| grant
                .expires_at
                .saturating_duration_since(t.sync_output_clock.now())
                .as_millis())
        ),
    );
    out.insert("parser".into(), format!("{:?}", t.parser.snapshot()));
    out.insert(
        "kitty_image_count".into(),
        t.kitty_graphics.image_count().to_string(),
    );
    out.insert(
        "paste_grant".into(),
        format!(
            "{:?}",
            t.pending_paste_grant.as_ref().map(|g| (
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
    t.screen.cursor_shape = match seed % 3 {
        0 => CursorShape::Block,
        1 => CursorShape::Beam,
        _ => CursorShape::Underline,
    };
    t.screen.current_flags.set_protected(seed & 1 != 0);
    t.screen.g1_charset = Charset::DecSpecialGraphics;
    t.screen.active_charset = Charset::DecSpecialGraphics;
    t.screen.current_fg = Color::Rgb(1, 2, 3);
    t.screen.current_bg = Color::Indexed(201);
    t.global_bg = Color::Blue;
    t.screen.current_flags.set_underline(UnderlineStyle::Curly);
    t.screen.pending_wrap = true;
    t.output_buffer = b"existing-reply".to_vec();
    // Modes 4/6/1016 have no TerminalModes storage slots. Use the real
    // authoritative booleans; true values detect mistaken protocol reads.
    t.screen.origin_mode = seed & 1 == 0;
    t.screen.insert_mode = seed & 2 == 0;
    super::super::structural_parity_tests::prime(&mut t);
    assert!(t.command_records.back().is_some_and(
        |record| !record.complete && record.state == super::super::CommandState::Running
    ));
    assert!(t.chat_native_takeover());
    t.window_title = "Mode mutation window".into();
    t.icon_title = "Mode mutation icon".into();
    t.title_stack = vec![(Some("saved icon".into()), Some("saved window".into()))];
    t.current_working_dir = Some("/baseline/mode-mutation".into());
    t.preedit_text = "输入中".into();
    t.preedit_cursor = 1;
    t.pending_osc52_clipboard_set = Some("clipboard sentinel".into());
    t.pending_osc52_clipboard_queries.push_back(b"\x07");
    t.pending_notifications = vec![("title sentinel".into(), "body sentinel".into())];
    t.colors.dynamic_fg = Some((1, 2, 3));
    t.colors.dynamic_bg = Some((4, 5, 6));
    t.colors.dynamic_cursor_color = Some((7, 8, 9));
    t.pending_bell = true;
    t.pending_paste_grant = Some(super::super::PendingPasteGrant {
        token: "mode-mutation-grant".into(),
        offered_mimes: ["text/plain".to_owned()].into_iter().collect(),
        expires_at: now + std::time::Duration::from_secs(600),
    });
    // Make the query's observer call visible even without other VT mutations.
    t.chat_native_takeover_sequence = None;
    t.host_window_focused = seed & 2 == 0;
    t.protocol.modes_mut().remove(&1004);
    t.sync_output_clock =
        super::super::SyncOutputClock::new(move || now + std::time::Duration::from_millis(5));
    t
}

fn corpus() -> Vec<(String, Vec<u8>)> {
    serde_json::from_str::<Vec<(String, Vec<u8>)>>(include_str!(
        "../../tests/fixtures/mode_mutation_corpus.json"
    ))
    .unwrap()
}
fn for_each_case(mut visit: impl FnMut(String, Snapshot)) {
    for seed in 0..4 {
        visit(format!("seed_{seed}"), snapshot(&seeded(seed)));
        for (id, data) in corpus() {
            // Whole feed, every single split, and byte-at-a-time. Snapshot every feed.
            let mut partitions = vec![vec![data.len()], (1..=data.len()).collect()];
            partitions.extend((1..data.len()).map(|at| vec![at, data.len()]));
            partitions.sort();
            partitions.dedup();
            for (partition, ends) in partitions.into_iter().enumerate() {
                let mut t = seeded(seed);
                let mut start = 0;
                for (feed, end) in ends.into_iter().enumerate() {
                    t.process_input(&data[start..end]);
                    start = end;
                    visit(
                        format!("seed_{seed}_{id}_partition_{partition}_feed_{feed}"),
                        snapshot(&t),
                    );
                }
            }
        }
    }
}
#[test]
#[ignore = "explicit original production capture, stdout only"]
fn print_mode_mutation_baseline() {
    for_each_case(|key, lanes| {
        println!(
            "\nMODE_BASELINE\t{}",
            serde_json::to_string(&(key, lanes)).unwrap()
        )
    });
}
#[test]
fn mode_mutation_original_production_golden() {
    // Missing capture is a hard failure, never an implicit capture or skipped pass.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mode_mutation_golden.json");
    #[derive(serde::Deserialize)]
    struct Captured {
        values: Vec<String>,
        cases: BTreeMap<String, BTreeMap<String, usize>>,
    }
    let encoded: serde_json::Value = serde_json::from_reader(std::io::BufReader::new(
        std::fs::File::open(path).expect("capture and freeze original baseline first"),
    ))
    .unwrap();
    let decoded = if encoded["format"] == "jterm-oracle-intern-v1" {
        super::super::oracle_fixture::decode(encoded).expect("valid lossless oracle")
    } else {
        encoded
    };
    let expected: Captured = serde_json::from_value(decoded).unwrap();
    let mut seen = 0;
    for_each_case(|key, got| {
        let lanes = expected.cases.get(&key).expect("missing captured case");
        assert_eq!(lanes.len(), got.len(), "{key}: lane count");
        for (lane, index) in lanes {
            assert_eq!(
                got.get(lane).expect("missing actual lane"),
                expected.values.get(*index).expect("invalid captured index"),
                "{key}: {lane}"
            );
        }
        seen += 1;
    });
    assert_eq!(seen, expected.cases.len(), "unexpected golden records");
}
#[test]
fn mode_mutation_frozen_host_differential() {
    let modes = [
        0, 1, 4, 6, 7, 12, 25, 47, 66, 1000, 1001, 1002, 1003, 1004, 1005, 1006, 1007, 1015, 1034,
        1036, 1047, 1048, 1049, 2031, 2004, 2026, 5522, 65535,
    ];
    for seed in 0..4 {
        for mode in modes {
            for private in [false, true] {
                for set in [false, true] {
                    let mut live = seeded(seed);
                    let mut reference = seeded(seed);
                    match (private, set) {
                        (true, true) => {
                            live.set_mode(mode);
                            reference.frozen_set_mode(mode);
                        }
                        (true, false) => {
                            live.reset_mode(mode);
                            reference.frozen_reset_mode(mode);
                        }
                        (false, true) => {
                            live.set_ansi_mode(mode);
                            reference.frozen_set_ansi_mode(mode);
                        }
                        (false, false) => {
                            live.reset_ansi_mode(mode);
                            reference.frozen_reset_ansi_mode(mode);
                        }
                    }
                    assert_eq!(
                        snapshot(&live),
                        snapshot(&reference),
                        "seed={seed} mode={mode} private={private} set={set}"
                    );
                }
            }
        }
    }
}
