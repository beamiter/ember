//! Captured original production vertical-motion observations.
use super::*;
use std::collections::BTreeMap;
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
thread_local! {
    static LAST_SEARCH_EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static TRACE: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}
pub(in crate::terminal) fn observe(t: &TerminalState, event: &str, detail: String) {
    TRACE.with(|slot| {
        if let Some(trace) = slot.borrow_mut().as_mut() {
            trace.push(format!("{event}:{detail}:cursor={:?}:pending={}:history={:?}:identity={}:dirty={:?}:provenance={:?}",
                (t.screen.cursor_row,t.screen.cursor_col),t.screen.pending_wrap,
                (t.total_lines_scrolled,t.scrollback.len(),t.scroll_offset),t.row_identity_revision,t.dirty_region,t.active_output_provenance));
        }
    });
}
fn start_trace(t: &TerminalState) {
    LAST_SEARCH_EPOCH.with(|v| v.set(t.search_scrollback_epoch));
    TRACE.with(|v| *v.borrow_mut() = Some(Vec::new()));
}
fn stop_trace() {
    TRACE.with(|v| *v.borrow_mut() = None);
}
fn observed(t: &TerminalState) -> Snapshot {
    let mut s = snapshot(t);
    s.insert(
        "search_scrollback_epoch_changed".into(),
        LAST_SEARCH_EPOCH.with(|v| {
            let changed = v.get() != t.search_scrollback_epoch;
            v.set(t.search_scrollback_epoch);
            changed.to_string()
        }),
    );
    s.insert(
        "callbacks".into(),
        TRACE.with(|v| format!("{:?}", v.borrow_mut().as_mut().map(std::mem::take))),
    );
    s.insert(
        "command_started_age_ms".into(),
        format!(
            "{:?}",
            t.command_records
                .iter()
                .map(|r| r.started_instant.map(|start| t
                    .sync_output_clock
                    .now()
                    .saturating_duration_since(start)
                    .as_millis()))
                .collect::<Vec<_>>()
        ),
    );
    s.insert(
        "command_records".into(),
        format!(
            "{:?}",
            t.command_records
                .iter()
                .cloned()
                .map(|mut record| {
                    record.started_instant = None;
                    record
                })
                .collect::<Vec<_>>()
        ),
    );
    s
}
fn seeded(context: usize, geometry: usize, row: usize) -> TerminalState {
    stop_trace();
    let mut t = super::super::alternate_parity_tests::seeded(0);
    let now = std::time::Instant::now();
    t.sync_output_clock = super::super::SyncOutputClock::new(move || now);
    t.protocol.modes_mut().insert(7);
    t.screen.active_charset = Charset::Ascii;
    t.screen.g0_charset = Charset::Ascii;
    t.screen.scroll_region_top = 0;
    t.screen.scroll_region_bottom = 4;
    t.set_max_scrollback(2);
    t.screen.cursor_row = 4;
    t.process_input(b"\n\n\n");
    if context & 1 != 0 {
        t.process_input(b"\x1b[?1049h");
    }
    if context & 2 != 0 {
        t.process_input(b"\x1b[?2026h");
    }
    if context & 4 != 0 {
        let sequence = t.command_records.back().unwrap().sequence;
        t.inline_cli_command_scope = Some((sequence, t.total_lines_scrolled + 9));
        t.inline_cli_launch_floor = Some(t.total_lines_scrolled + 9);
    }
    t.process_input(b"\x1b[2;1H\x1b_Ga=p,i=41,p=3,r=2,c=1,C=1\x1b\\");
    for record in &mut t.command_records {
        record.started_at = Some(std::time::SystemTime::UNIX_EPOCH);
        if record.finished_at.is_some() {
            record.finished_at = Some(std::time::SystemTime::UNIX_EPOCH);
        }
        record.started_instant = record.started_instant.map(|_| now);
        record.duration_ms = record.duration_ms.map(|_| 0);
    }
    if geometry == 6 {
        t.on_resize(1, 1);
    }
    if geometry == 7 {
        t.on_resize(1, 5);
    }
    if geometry == 8 {
        t.on_resize(5, 1);
    }
    let rows = t.screen.grid.rows();
    let cols = t.screen.grid.row_len();
    let (top, bottom) = match geometry {
        0 => (0, 4),
        1 => (0, 2),
        2 => (1, 3),
        3 => (2, 2),
        4 => (3, 1),
        5 => (0, 9),
        _ => (0, rows - 1),
    };
    t.screen.scroll_region_top = top;
    t.screen.scroll_region_bottom = bottom;
    t.screen.cursor_row = row.min(rows - 1);
    t.screen.cursor_col = cols - 1;
    t.screen.pending_wrap = true;
    t.screen.current_bg = Color::Indexed(201);
    t.scroll_offset = 1;
    t.selection = Some(Selection {
        anchor: (0, 0),
        active: (1, 1),
        mode: SelectionMode::Normal,
    });
    for r in 0..rows {
        t.screen.grid.row_wrapped[r] = r % 2 == 0;
    }
    if let Some(a) = t.active_output_provenance.as_mut() {
        a.invalid = false;
        a.cursor_moved_since_output = false;
    }
    t.chat_native_takeover_sequence = None;
    super::super::structural_parity_tests::prime(&mut t);
    t
}
fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("controls",b"A\nB\x0bC\x0cD\x1bDE\x1bEF\x1bMG".to_vec()),
        ("ascii_wrap",b"abcdefghijklmno\nPQRSTUV\x1bE!".to_vec()),
        ("wide_wrap","界界界界\n界\u{1b}D界\u{1b}M界".as_bytes().to_vec()),
        ("mode_alt_ris",b"\x1b[?7l1234567\x1b[?7hX\x1b[?1049h\x1b[?2026h\n\x1bE\x1bM\x1b[?2026l\x1b[?1049l\x1bc\n!".to_vec()),
        ("margins",b"\x1b[1;3r\x1b[3;6HAB\n\x1bM\x1b[5;6HX\x1bD\x1bE\x1bM".to_vec()),
    ]
}
fn for_each_case(mut visit: impl FnMut(String, Snapshot)) {
    for context in 0..8 {
        for geometry in 0..9 {
            for row in 0..5 {
                // Invalid internal regions are intentional. Valid cursor positions only.
                for (op, bytes) in [
                    ("LF", &b"\n"[..]),
                    ("VT", &b"\x0b"[..]),
                    ("FF", &b"\x0c"[..]),
                    ("IND", &b"\x1bD"[..]),
                    ("NEL", &b"\x1bE"[..]),
                    ("RI", &b"\x1bM"[..]),
                    ("ascii", &b"X"[..]),
                    ("wide", "界".as_bytes()),
                ] {
                    let mut t = seeded(context, geometry, row);
                    start_trace(&t);
                    visit(
                        format!("c{context}_g{geometry}_r{row}_{op}_seed"),
                        observed(&t),
                    );
                    for repeat in 0..2 {
                        t.process_input(bytes);
                        visit(
                            format!("c{context}_g{geometry}_r{row}_{op}_{repeat}"),
                            observed(&t),
                        );
                    }
                }
            }
        }
    }
    for context in 0..8 {
        for geometry in [0, 2, 6, 7, 8] {
            for (id, data) in corpus() {
                let mut partitions = vec![vec![data.len()], (1..=data.len()).collect()];
                partitions.extend((1..data.len()).map(|at| vec![at, data.len()]));
                let mut rng = 0x5eed_u32;
                for _ in 0..3 {
                    let mut ends = Vec::new();
                    let mut at = 0;
                    while at < data.len() {
                        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                        at = (at + 1 + rng as usize % 7).min(data.len());
                        ends.push(at);
                    }
                    partitions.push(ends);
                }
                partitions.sort();
                partitions.dedup();
                for (p, ends) in partitions.into_iter().enumerate() {
                    let mut t = seeded(context, geometry, 4);
                    start_trace(&t);
                    let mut start = 0;
                    for (feed, end) in ends.into_iter().enumerate() {
                        t.process_input(&data[start..end]);
                        start = end;
                        visit(
                            format!("parser_c{context}_g{geometry}_{id}_p{p}_f{feed}"),
                            observed(&t),
                        );
                    }
                }
            }
        }
    }
    // Live-edge preservation with empty and nonfull history, plus both
    // pending-wrap states and ordinary interior-column hard controls.
    for history_rows in 0..2 {
        for pending in [false, true] {
            for (op, bytes) in [
                ("LF", b"\n".as_slice()),
                ("NEL", b"\x1bE".as_slice()),
                ("RI", b"\x1bM".as_slice()),
                ("ascii", b"abcde".as_slice()),
            ] {
                stop_trace();
                let mut t = TerminalState::new(4, 3);
                let now = std::time::Instant::now();
                t.sync_output_clock = super::super::SyncOutputClock::new(move || now);
                t.set_max_scrollback(2);
                t.process_input(b"\x1b[3;2H");
                for _ in 0..history_rows {
                    t.process_input(b"\n");
                }
                t.screen.pending_wrap = pending;
                assert_eq!(t.scroll_offset, 0);
                assert_eq!(t.scrollback.len(), history_rows);
                start_trace(&t);
                visit(
                    format!("live_h{history_rows}_p{pending}_{op}_seed"),
                    observed(&t),
                );
                for repeat in 0..4 {
                    t.process_input(bytes);
                    visit(
                        format!("live_h{history_rows}_p{pending}_{op}_{repeat}"),
                        observed(&t),
                    );
                }
            }
        }
    }
    // Fresh C with no output exercises the successful first-output rebase,
    // not merely the callback's common early-return path.
    for context in 0..8 {
        for geometry in 0..9 {
            for row in 0..5 {
                for (op, bytes) in [
                    ("hard", b"\n".as_slice()),
                    ("soft_ascii", b"X".as_slice()),
                    ("soft_wide", "界".as_bytes()),
                ] {
                    let mut t = seeded(context, geometry, row);
                    t.process_input(b"\x1b]133;A\x07\x1b]133;C;id=first-output\x07");
                    let now = t.sync_output_clock.now();
                    for record in &mut t.command_records {
                        record.started_at = Some(std::time::SystemTime::UNIX_EPOCH);
                        if record.finished_at.is_some() {
                            record.finished_at = Some(std::time::SystemTime::UNIX_EPOCH);
                        }
                        record.started_instant = record.started_instant.map(|_| now);
                        record.duration_ms = record.duration_ms.map(|_| 0);
                    }
                    if context & 4 != 0 {
                        let sequence = t.command_records.back().unwrap().sequence;
                        t.inline_cli_command_scope = Some((sequence, t.total_lines_scrolled + 9));
                    }
                    t.screen.pending_wrap = true;
                    if !t.screen.use_alt_buffer {
                        let active = t.active_output_provenance.unwrap();
                        assert!(
                            active.extent.is_none()
                                && active.last_write_end.is_none()
                                && !active.invalid
                        );
                        assert_eq!(
                            active.start_row_id,
                            t.screen.grid.row_id(t.screen.cursor_row)
                        );
                    }
                    start_trace(&t);
                    visit(
                        format!("first_c{context}_g{geometry}_r{row}_{op}_seed"),
                        observed(&t),
                    );
                    t.process_input(bytes);
                    visit(
                        format!("first_c{context}_g{geometry}_r{row}_{op}_after"),
                        observed(&t),
                    );
                    t.process_input(b"123456789");
                    visit(
                        format!("first_c{context}_g{geometry}_r{row}_{op}_later"),
                        observed(&t),
                    );
                }
            }
        }
    }
    for context in 0..8 {
        let mut t = seeded(context, 0, 4);
        start_trace(&t);
        for (step, (rows, cols)) in [(3, 4), (1, 1), (5, 6), (2, 3)].into_iter().enumerate() {
            t.on_resize(rows, cols);
            visit(format!("resize_c{context}_{step}"), observed(&t));
            for (feed, bytes) in [
                b"1234567".as_slice(),
                "界\n".as_bytes(),
                b"\x1bE\x1bM",
                b"\x1b[?1049h\n\x1b[?1049l",
                b"\x1bc\n",
            ]
            .into_iter()
            .enumerate()
            {
                t.process_input(bytes);
                visit(format!("resize_c{context}_{step}_f{feed}"), observed(&t));
            }
        }
    }
    stop_trace();
}
#[test]
#[ignore = "explicit original production capture, stdout only"]
fn print_vertical_motion_baseline() {
    for_each_case(|key, lanes| {
        println!(
            "\nVERTICAL_BASELINE\t{}",
            serde_json::to_string(&(key, lanes)).unwrap()
        )
    });
}
#[test]
fn vertical_motion_original_production_golden() {
    #[derive(serde::Deserialize)]
    struct Captured {
        values: Vec<String>,
        cases: BTreeMap<String, BTreeMap<String, usize>>,
    }
    // Transport-only sharding: reconstruct and verify the exact frozen bytes
    // before using the unchanged intern decoder and original expected values.
    use sha2::Digest;
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/vertical_motion_golden");
    let mut original = Vec::with_capacity(28_821_908);
    for part in [
        "part-00.json-fragment",
        "part-01.json-fragment",
        "part-02.json-fragment",
        "part-03.json-fragment",
        "part-04.json-fragment",
        "part-05.json-fragment",
        "part-06.json-fragment",
    ] {
        original
            .extend(std::fs::read(directory.join(part)).expect("missing original golden shard"));
    }
    assert_eq!(original.len(), 28_821_908, "original golden byte count");
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(&original)),
        "4012a7af5a2820cbbfb26f25cd1d9668b2c190fbfb90615d4cb9e1baeaca68b9",
        "original golden SHA256"
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
        let lanes = expected.cases.get(&key).expect("missing case");
        assert_eq!(lanes.len(), got.len(), "{key}: lane count");
        for (lane, index) in lanes {
            assert_eq!(
                got.get(lane).unwrap(),
                &expected.values[*index],
                "{key}: {lane}"
            );
        }
        seen += 1;
    });
    assert_eq!(seen, expected.cases.len());
}
#[test]
fn vertical_motion_frozen_direct_differential() {
    for context in 0..8 {
        for geometry in 0..9 {
            for row in 0..5 {
                for wrap in [false, true] {
                    let mut live = seeded(context, geometry, row);
                    let mut reference = seeded(context, geometry, row);
                    for _ in 0..3 {
                        if wrap {
                            live.wrap_to_next_line();
                            reference.frozen_vertical_wrap_to_next_line();
                        } else {
                            live.line_feed();
                            reference.frozen_vertical_line_feed();
                        }
                        assert_eq!(
                            snapshot(&live),
                            snapshot(&reference),
                            "c{context} g{geometry} r{row} wrap{wrap}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn vertical_motion_frozen_reverse_differential() {
    for context in 0..8 {
        for geometry in 0..9 {
            for row in 0..5 {
                let mut live = seeded(context, geometry, row);
                let mut reference = seeded(context, geometry, row);
                for _ in 0..3 {
                    live.process_input(b"\x1bM");
                    reference.invalidate_search_source();
                    reference.frozen_vertical_reverse_index();
                    reference.observe_chat_native_takeover();
                    assert_eq!(
                        snapshot(&live),
                        snapshot(&reference),
                        "RI c{context} g{geometry} r{row}"
                    );
                }
            }
        }
    }
}
