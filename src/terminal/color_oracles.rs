//! Test-only original production color observations; never synthesize expectations.
use super::*;
use std::collections::BTreeMap;
type Snapshot = BTreeMap<String, String>;
fn snapshot(t: &TerminalState, previous_epoch: Option<u64>) -> Snapshot {
    let mut out = super::super::alternate_parity_tests::snapshot(t);
    out.insert(
        "search_scrollback_epoch_changed".into(),
        previous_epoch
            .is_some_and(|old| old != t.search_scrollback_epoch)
            .to_string(),
    );
    out.insert("dynamic_fg".into(), format!("{:?}", t.colors.dynamic_fg));
    out.insert("dynamic_bg".into(), format!("{:?}", t.colors.dynamic_bg));
    out.insert(
        "dynamic_cursor_color".into(),
        format!("{:?}", t.colors.dynamic_cursor_color),
    );
    out.insert(
        "dynamic_palette".into(),
        format!("{:?}", t.colors.dynamic_palette),
    );
    out.insert(
        "default_colors".into(),
        format!("{:?}", t.colors.default_colors),
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
    host!(
        sync_output_active,
        current_working_dir,
        ime_enabled,
        preedit_text,
        preedit_cursor,
        host_window_focused,
        pending_osc52_clipboard_set,
        pending_osc52_clipboard_queries,
        pending_notifications,
        pending_bell,
        osc_reply_terminator,
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
    out.insert(
        "effective_default_background".into(),
        format!("{:?}", t.effective_default_background()),
    );
    out.insert(
        "color_scheme_report".into(),
        t.color_scheme_report().to_string(),
    );
    let records = t
        .command_records
        .iter()
        .cloned()
        .map(|mut r| {
            r.started_at = None;
            r.finished_at = None;
            r.started_instant = None;
            r.duration_ms = None;
            r
        })
        .collect::<Vec<_>>();
    out.insert("commands".into(), format!("{records:?}"));
    let completed = t
        .pending_completed_command_outputs
        .iter()
        .cloned()
        .map(|mut e| {
            e.completed.duration_ms = None;
            e
        })
        .collect::<Vec<_>>();
    out.insert("completed".into(), format!("{completed:?}"));
    out
}
fn theme(light: bool) -> super::super::TerminalDefaultColors {
    super::super::TerminalDefaultColors {
        foreground: if light { (11, 22, 33) } else { (211, 222, 233) },
        background: if light { (240, 230, 220) } else { (10, 20, 30) },
        cursor: if light { (171, 182, 193) } else { (71, 82, 93) },
        ansi: std::array::from_fn(|i| {
            if light {
                (255 - i as u8 * 13, i as u8 * 11, 255 - i as u8 * 7)
            } else {
                (i as u8 * 13, 255 - i as u8 * 11, i as u8 * 7)
            }
        }),
    }
}
fn seeded(seed: usize) -> TerminalState {
    let mut t = super::super::alternate_parity_tests::seeded(seed);
    let now = std::time::Instant::now();
    t.sync_output_clock = super::super::SyncOutputClock::new(move || now);
    t.set_default_colors(theme(seed & 1 != 0));
    if seed & 1 != 0 {
        t.process_input(b"\x1b[?1049h");
    }
    if seed & 2 != 0 {
        t.process_input(b"\x1b[?2026h");
    }
    t.colors.dynamic_fg = Some((13, 24, 35));
    t.colors.dynamic_bg = Some((36, 47, 58));
    t.colors.dynamic_cursor_color = Some((59, 60, 71));
    t.colors.dynamic_palette = std::array::from_fn(|i| {
        if seed & 1 != 0 || i % 17 == 0 {
            Some((
                i as u8,
                (i as u8).wrapping_add(47),
                (i as u8).wrapping_add(93),
            ))
        } else {
            None
        }
    });
    t.preedit_text = "输入中".into();
    t.preedit_cursor = 1;
    t.pending_osc52_clipboard_set = Some("clipboard sentinel".into());
    t.pending_osc52_clipboard_queries.push_back(b"\x07");
    t.pending_notifications = vec![("title sentinel".into(), "body sentinel".into())];
    t.output_buffer = b"existing-reply".to_vec();
    t
}
#[derive(serde::Deserialize)]
struct Corpus {
    cases: Vec<Input>,
    expected_record_count: usize,
}
#[derive(serde::Deserialize)]
struct Input {
    id: String,
    partition_policy: String,
    steps: Vec<Step>,
    expected_snapshots_per_seed: usize,
}
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Step {
    Feed { bytes_hex: String },
    Theme { theme: String },
}
fn corpus() -> Corpus {
    serde_json::from_str(include_str!("../../tests/fixtures/color_corpus.json")).unwrap()
}
fn bytes(hex: &str) -> Vec<u8> {
    assert!(hex.is_ascii() && hex.len() & 1 == 0);
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
fn ends(n: usize, schedule: usize, bulk: bool) -> Vec<usize> {
    match schedule {
        0 => vec![n],
        1 if bulk => (1..=n.div_ceil(17)).map(|i| (i * 17).min(n)).collect(),
        1 => (1..=n).collect(),
        2..=5 => {
            let mut state = (schedule - 1) as u64;
            let mut at = 0;
            let mut out = Vec::new();
            while at < n {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                at = (at + 1 + (state % if bulk { 64 } else { 23 }) as usize).min(n);
                out.push(at);
            }
            out
        }
        _ => vec![schedule - 5, n],
    }
}
fn for_each_case(mut visit: impl FnMut(String, Snapshot)) {
    let corpus = corpus();
    let mut seen = 0;
    for seed in 0..4 {
        visit(format!("seed_{seed}"), snapshot(&seeded(seed), None));
        seen += 1;
        for input in &corpus.cases {
            let schedules = if input.partition_policy == "all_two_way_and_bytewise" {
                assert_eq!(input.steps.len(), 1);
                let Step::Feed { bytes_hex } = &input.steps[0] else {
                    panic!("expected feed")
                };
                6 + bytes(bytes_hex).len().saturating_sub(1)
            } else {
                6
            };
            let mut input_seen = 0;
            for schedule in 0..schedules {
                let mut t = seeded(seed);
                for (step, operation) in input.steps.iter().enumerate() {
                    match operation {
                        Step::Theme { theme: name } => {
                            assert!(matches!(name.as_str(), "light" | "dark"));
                            let epoch = t.search_scrollback_epoch;
                            t.set_default_colors(theme(name == "light"));
                            visit(
                                format!(
                                    "seed_{seed}_{}_partition_{schedule}_step_{step}_feed_0",
                                    input.id
                                ),
                                snapshot(&t, Some(epoch)),
                            );
                            input_seen += 1;
                        }
                        Step::Feed { bytes_hex } => {
                            let data = bytes(bytes_hex);
                            let mut start = 0;
                            for (feed, end) in ends(
                                data.len(),
                                schedule,
                                input.partition_policy == "bulk_chunked_4",
                            )
                            .into_iter()
                            .enumerate()
                            {
                                assert!(end >= start && end <= data.len());
                                let epoch = t.search_scrollback_epoch;
                                t.process_input(&data[start..end]);
                                start = end;
                                visit(format!("seed_{seed}_{}_partition_{schedule}_step_{step}_feed_{feed}",input.id),snapshot(&t, Some(epoch)));
                                input_seen += 1;
                            }
                            assert_eq!(start, data.len());
                        }
                    }
                }
            }
            assert_eq!(
                input_seen, input.expected_snapshots_per_seed,
                "{} count",
                input.id
            );
            seen += input_seen;
        }
    }
    assert_eq!(seen, corpus.expected_record_count);
}
#[test]
#[ignore = "explicit pre-extraction production capture, stdout only"]
fn print_color_baseline() {
    for_each_case(|key, lanes| {
        println!(
            "\nCOLOR_BASELINE\t{}",
            serde_json::to_string(&(key, lanes)).unwrap()
        )
    });
}

#[test]
fn color_original_production_golden() {
    use sha2::Digest;
    // Set once from genuine capture before extraction. A missing baseline is
    // a hard failure, never a request to generate candidate expectations.
    const FROZEN_TRANSPORT_SHA256: &str =
        "04edcd7541838201e585d0505d2898e56a22f82fda2abfa917701255a558dfc7";
    #[derive(serde::Deserialize)]
    struct Part {
        file: String,
        bytes: usize,
        sha256: String,
    }
    #[derive(serde::Deserialize)]
    struct Manifest {
        original_bytes: usize,
        original_sha256: String,
        parts: Vec<Part>,
    }
    #[derive(serde::Deserialize)]
    struct Captured {
        values: Vec<String>,
        cases: BTreeMap<String, BTreeMap<String, usize>>,
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/color_golden");
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json"))
            .expect("freeze original production capture first"),
    )
    .unwrap();
    assert_eq!(
        manifest.original_sha256, FROZEN_TRANSPORT_SHA256,
        "immutable capture identity"
    );
    let mut original = Vec::new();
    for (index, part) in manifest.parts.iter().enumerate() {
        assert_eq!(part.file, format!("part-{index:02}.json-fragment"));
        let bytes = std::fs::read(dir.join(&part.file)).expect("original golden shard");
        assert_eq!(bytes.len(), part.bytes);
        assert_eq!(format!("{:x}", sha2::Sha256::digest(&bytes)), part.sha256);
        original.extend(bytes);
    }
    assert_eq!(original.len(), manifest.original_bytes);
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(&original)),
        FROZEN_TRANSPORT_SHA256
    );
    let encoded: serde_json::Value = serde_json::from_slice(&original).unwrap();
    let decoded = if encoded["format"] == "jterm-oracle-intern-v1" {
        super::super::oracle_fixture::decode(encoded).unwrap()
    } else {
        encoded
    };
    let expected: Captured = serde_json::from_value(decoded).unwrap();
    let mut seen = 0;
    for_each_case(|key, got| {
        let lanes = expected.cases.get(&key).expect("missing captured key");
        assert_eq!(lanes.len(), got.len(), "{key}:lane count");
        for (lane, index) in lanes {
            assert_eq!(
                got.get(lane).expect("missing actual lane"),
                expected.values.get(*index).expect("invalid value index"),
                "{key}:{lane}"
            );
        }
        seen += 1;
    });
    assert_eq!(seen, expected.cases.len());
}
