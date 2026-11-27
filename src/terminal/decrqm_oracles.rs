//! Test-only DECRQM baseline. No runtime golden is bundled.
use super::*;
use std::collections::BTreeMap;
// Verbatim baseline method bodies; not a replacement implementation.
impl TerminalState {
    fn frozen_decrqm_private_mode_state(&self, mode: u16) -> u8 {
        let set = match mode {
            6 => self.vt.screen.origin_mode,
            1
            | 7
            | 12
            | 25
            | 47
            | 66
            | 1000..=1007
            | 1015
            | 1034
            | 1036
            | 1047..=1049
            | 2004
            | 2026
            | 2031
            | 5522 => self.vt.protocol.modes().contains(&mode),
            _ => return 0,
        };
        if set {
            1
        } else {
            2
        }
    }

    fn frozen_decrqm_ansi_mode_state(&self, mode: u16) -> u8 {
        match mode {
            4 if self.vt.screen.insert_mode => 1,
            4 => 2,
            20 => 4,
            _ => 0,
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
    out.insert(
        "sync_output_active".into(),
        format!("{:?}", t.vt.sync_output.active),
    );
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
    t.vt.screen
        .current_flags
        .set_underline(UnderlineStyle::Curly);
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
    t.titles.window_title = "DECRQM window".into();
    t.titles.icon_title = "DECRQM icon".into();
    t.titles.title_stack = vec![(Some("saved icon".into()), Some("saved window".into()))];
    t.current_working_dir = Some("/baseline/decrqm".into());
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
        token: "decrqm-grant".into(),
        offered_mimes: ["text/plain".to_owned()].into_iter().collect(),
        expires_at: now + std::time::Duration::from_secs(600),
    });
    // Make the query's observer call visible even without other VT mutations.
    t.chat_native_takeover_sequence = None;
    t
}

#[derive(serde::Deserialize)]
struct Corpus {
    cases: Vec<Input>,
}
#[derive(serde::Deserialize)]
struct Input {
    id: String,
    bytes_hex: String,
}
fn inputs() -> Vec<Input> {
    serde_json::from_str::<Corpus>(include_str!("../../tests/fixtures/decrqm_corpus.json"))
        .unwrap()
        .cases
}
fn bytes(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0);
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
fn partitions(n: usize) -> Vec<Vec<usize>> {
    if n <= 8 {
        (0..(1usize << n.saturating_sub(1)))
            .map(|mask| {
                (1..=n)
                    .filter(|&end| end == n || mask & (1 << (end - 1)) != 0)
                    .collect()
            })
            .collect()
    } else {
        let mut all = vec![vec![n], (1..=n).collect()];
        all.extend((1..n).map(|at| vec![at, n]));
        all
    }
}
fn for_each_case(mut visit: impl FnMut(String, Snapshot)) {
    for seed in 0..4 {
        visit(format!("seed_{seed}"), snapshot(&seeded(seed)));
        for input in inputs() {
            let data = bytes(&input.bytes_hex);
            for (partition, ends) in partitions(data.len()).into_iter().enumerate() {
                let mut t = seeded(seed);
                let start_time = t.vt.sync_output.started;
                let clock_reading = t.sync_output_clock.now();
                let grant_expiry = t.osc5522.pending_paste_grant.as_ref().map(|g| g.expires_at);
                let mut start = 0;
                for (feed, end) in ends.into_iter().enumerate() {
                    t.process_input(&data[start..end]);
                    start = end;
                    assert_eq!(
                        t.vt.sync_output.started, start_time,
                        "query must not restart sync clock"
                    );
                    assert_eq!(
                        t.sync_output_clock.now(),
                        clock_reading,
                        "clock behavior retained"
                    );
                    assert_eq!(
                        t.osc5522.pending_paste_grant.as_ref().map(|g| g.expires_at),
                        grant_expiry,
                        "paste expiry retained"
                    );
                    visit(
                        format!("seed_{seed}_{}_partition_{partition}_feed_{feed}", input.id),
                        snapshot(&t),
                    );
                }
            }
        }
    }
}
#[test]
#[ignore = "explicit original-production capture, stdout only"]
fn print_decrqm_baseline() {
    for_each_case(|key, lanes| {
        println!(
            "\nDECRQM_BASELINE\t{}",
            serde_json::to_string(&(key, lanes)).unwrap()
        );
    });
}
#[test]
fn decrqm_original_production_golden() {
    // Missing capture is a hard failure, never an implicit capture or skipped pass.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/decrqm_before_engine.json");
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
fn decrqm_frozen_lookup_exhaustive() {
    for state in 0..4 {
        let mut t = seeded(state);
        for mode in 0..=u16::MAX {
            for stored in [false, true] {
                if stored {
                    t.vt.protocol.modes_mut().insert(mode);
                } else {
                    t.vt.protocol.modes_mut().remove(&mode);
                }
                // X10, UK NRCS, and reverse-wrap were unsupported in the
                // frozen baseline. Check their current Boolean state and keep
                // every other private mode under the original oracle.
                let private_expected = match mode {
                    9 | 42 | 45 => {
                        if stored {
                            1
                        } else {
                            2
                        }
                    }
                    _ => t.frozen_decrqm_private_mode_state(mode),
                };
                assert_eq!(
                    t.decrqm_private_mode_state(mode),
                    private_expected,
                    "private {mode}/{state}/{stored}"
                );
                assert_eq!(
                    t.decrqm_ansi_mode_state(mode),
                    t.frozen_decrqm_ansi_mode_state(mode),
                    "ansi {mode}/{state}/{stored}"
                );
            }
        }
    }
}

#[test]
fn supported_x10_and_reverse_wrap_queries_follow_actual_wire_order() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b[?9;45h\x1b[?45$p\x1b[?9$p\x1b[?45$p");
    assert_eq!(
        terminal.get_output(),
        b"\x1b[?45;1$y\x1b[?9;1$y\x1b[?45;1$y"
    );
    terminal.process_input(b"\x1b[?9;45l\x1b[?9$p\x1b[?45$p\x1b[?9$p");
    assert_eq!(terminal.get_output(), b"\x1b[?9;2$y\x1b[?45;2$y\x1b[?9;2$y");
    // Ember's inherited DECRQM surface answers only the first parameter.
    terminal.process_input(b"\x1b[?9;45;9$p");
    assert_eq!(terminal.get_output(), b"\x1b[?9;2$y");
}

#[test]
fn uk_nrcs_queries_follow_set_reset_and_saved_mode_on_the_wire() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(
        b"\x1b[?42$p\x1b[?42h\x1b[?42$p\x1b[?42s\x1b[?42l\x1b[?42$p\x1b[?42r\x1b[?42$p",
    );
    assert_eq!(
        terminal.get_output(),
        b"\x1b[?42;2$y\x1b[?42;1$y\x1b[?42;2$y\x1b[?42;1$y"
    );
}
