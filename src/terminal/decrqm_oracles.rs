//! Test-only DECRQM baseline. No runtime golden is bundled.
use super::*;
use std::collections::BTreeMap;
// Verbatim baseline method bodies; not a replacement implementation.
impl TerminalState {
    fn frozen_decrqm_private_mode_state(&self, mode: u16) -> u8 {
        let set = match mode {
            6 => self.screen.origin_mode,
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
            | 5522 => self.protocol.modes().contains(&mode),
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
            4 if self.screen.insert_mode => 1,
            4 => 2,
            20 => 4,
            _ => 0,
        }
    }
}

type Snapshot = BTreeMap<String, String>;
fn snapshot(t: &TerminalState) -> Snapshot {
    let mut out = super::super::alternate_parity_tests::snapshot(t);
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
        dynamic_fg,
        dynamic_bg,
        dynamic_cursor_color,
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
    t.window_title = "DECRQM window".into();
    t.icon_title = "DECRQM icon".into();
    t.title_stack = vec![(Some("saved icon".into()), Some("saved window".into()))];
    t.current_working_dir = Some("/baseline/decrqm".into());
    t.preedit_text = "输入中".into();
    t.preedit_cursor = 1;
    t.pending_osc52_clipboard_set = Some("clipboard sentinel".into());
    t.pending_osc52_clipboard_queries.push_back(b"\x07");
    t.pending_notifications = vec![("title sentinel".into(), "body sentinel".into())];
    t.dynamic_fg = Some((1, 2, 3));
    t.dynamic_bg = Some((4, 5, 6));
    t.dynamic_cursor_color = Some((7, 8, 9));
    t.pending_bell = true;
    t.pending_paste_grant = Some(super::super::PendingPasteGrant {
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
                let start_time = t.sync_output_start;
                let clock_reading = t.sync_output_clock.now();
                let grant_expiry = t.pending_paste_grant.as_ref().map(|g| g.expires_at);
                let mut start = 0;
                for (feed, end) in ends.into_iter().enumerate() {
                    t.process_input(&data[start..end]);
                    start = end;
                    assert_eq!(
                        t.sync_output_start, start_time,
                        "query must not restart sync clock"
                    );
                    assert_eq!(
                        t.sync_output_clock.now(),
                        clock_reading,
                        "clock behavior retained"
                    );
                    assert_eq!(
                        t.pending_paste_grant.as_ref().map(|g| g.expires_at),
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
                    t.protocol.modes_mut().insert(mode);
                } else {
                    t.protocol.modes_mut().remove(&mode);
                }
                assert_eq!(
                    t.decrqm_private_mode_state(mode),
                    t.frozen_decrqm_private_mode_state(mode),
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
