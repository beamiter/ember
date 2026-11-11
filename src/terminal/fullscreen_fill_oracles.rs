//! Original full-screen fill bodies frozen at fc5bd956. Never delegate to candidate engine.
use super::*;
use std::collections::BTreeMap;
impl TerminalState {
    pub(super) fn frozen_fill_erase_screen(&mut self) {
        self.screen.pending_wrap = false;
        let bg_color = self.screen.current_bg;
        let cols = self.screen.grid.row_len();
        let spans: Vec<_> = (0..self.screen.grid.rows())
            .map(|row| (row, 0, cols))
            .collect();
        self.invalidate_grid_mutation_spans(&spans);
        for row in self.screen.grid.iter_mut() {
            for cell in row.iter_mut() {
                *cell = TerminalCell {
                    character: ' ',
                    foreground: Color::Default,
                    background: bg_color,
                    flags: StyleFlags::default(),
                    hyperlink_id: HyperlinkId::NONE,
                };
            }
        }
        // Mark all rows as dirty
        self.dirty_region.mark_all(self.screen.grid.rows());
        self.mark_rows_dirty(0, self.screen.grid.rows().saturating_sub(1));
        self.kitty_graphics.clear_placements();
    }
    fn frozen_fill_screen_alignment_test(&mut self) {
        let cols = self.screen.grid.row_len();
        let rows = self.screen.grid.rows();
        let spans: Vec<_> = (0..rows).map(|row| (row, 0, cols)).collect();
        self.invalidate_grid_mutation_spans(&spans);
        let filled = TerminalCell {
            character: 'E',
            ..TerminalCell::default()
        };
        for row in self.screen.grid.iter_mut() {
            row.fill(filled);
        }
        self.screen.scroll_region_top = 0;
        self.screen.scroll_region_bottom = rows.saturating_sub(1);
        self.screen.cursor_row = 0;
        self.screen.cursor_col = 0;
        self.screen.pending_wrap = false;
        self.note_output_cursor_reposition(true);
        self.dirty_region.mark_all(rows);
        self.mark_rows_dirty(0, rows.saturating_sub(1));
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
    let mut t = super::super::alternate_parity_tests::seeded(seed % 4);
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
    if seed >= 4 {
        t.on_resize(1, if seed >= 6 { 1 } else { 5 });
    }
    let rows = t.screen.grid.rows();
    let cols = t.screen.grid.row_len();
    t.screen.cursor_row = rows - 1;
    t.screen.cursor_col = cols - 1;
    t.screen.scroll_region_top = if rows > 2 { 1 } else { 0 };
    t.screen.scroll_region_bottom = rows.saturating_sub(2).max(t.screen.scroll_region_top);
    t.screen.current_bg = if seed & 1 == 0 {
        Color::Indexed(201)
    } else {
        Color::Rgb(19, 37, 91)
    };
    t.screen.pending_wrap = true;
    t.screen.last_printed_char = Some('R');
    for r in 0..rows {
        t.screen.grid.row_wrapped[r] = r % 2 == 0;
        for c in 0..cols {
            let cell = &mut t.screen.grid[r][c];
            cell.flags.set_protected(true);
            cell.hyperlink_id = HyperlinkId::from_raw(17);
        }
    }
    if cols == 1 {
        t.screen.grid[0][0].character = '界';
        t.screen.grid[0][0].flags.set_wide(true);
    }
    if seed % 4 >= 2 {
        let sequence = t.command_records.back().unwrap().sequence;
        t.inline_cli_command_scope = Some((sequence, t.total_lines_scrolled + 9));
        t.inline_cli_launch_floor = Some(t.total_lines_scrolled + 9);
    }
    if let Some(active) = t.active_output_provenance.as_mut() {
        active.cursor_moved_since_output = false;
        active.invalid = false;
    }
    t.scroll_offset = 2;
    super::super::structural_parity_tests::prime(&mut t);
    t
}
fn corpus() -> Vec<(String, Vec<u8>)> {
    serde_json::from_str::<Vec<(String, Vec<u8>)>>(include_str!(
        "../../tests/fixtures/fullscreen_fill_corpus.json"
    ))
    .unwrap()
}
fn for_each_case(mut visit: impl FnMut(String, Snapshot)) {
    for seed in 0..8 {
        visit(format!("seed_{seed}"), snapshot(&seeded(seed)));
        for alignment in [false, true] {
            let mut t = seeded(seed);
            for repeat in 0..3 {
                if alignment {
                    t.screen_alignment_test();
                } else {
                    t.erase_screen();
                }
                visit(
                    format!("seed_{seed}_direct_{alignment}_{repeat}"),
                    snapshot(&t),
                );
            }
        }
        for (id, data) in corpus() {
            // Whole feed, every single split, and byte-at-a-time. Snapshot every feed.
            let mut partitions = vec![vec![data.len()], (1..=data.len()).collect()];
            partitions.extend((1..data.len()).map(|at| vec![at, data.len()]));
            let mut rng = 0x5eed_u32;
            for _ in 0..4 {
                let mut ends = Vec::new();
                let mut at = 0;
                while at < data.len() {
                    rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                    at = (at + 1 + rng as usize % 5).min(data.len());
                    ends.push(at);
                }
                partitions.push(ends);
            }
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
fn print_fullscreen_fill_baseline() {
    for_each_case(|key, lanes| {
        println!(
            "\nFILL_BASELINE\t{}",
            serde_json::to_string(&(key, lanes)).unwrap()
        )
    });
}
#[test]
fn fullscreen_fill_original_production_golden() {
    // Missing capture is a hard failure, never an implicit capture or skipped pass.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fullscreen_fill_golden.json");
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
fn fullscreen_fill_frozen_direct_differential() {
    for seed in 0..8 {
        for alignment in [false, true] {
            let mut live = seeded(seed);
            let mut reference = seeded(seed);
            for _ in 0..3 {
                if alignment {
                    live.screen_alignment_test();
                    reference.frozen_fill_screen_alignment_test();
                } else {
                    live.erase_screen();
                    reference.frozen_fill_erase_screen();
                }
                assert_eq!(
                    snapshot(&live),
                    snapshot(&reference),
                    "seed={seed} alignment={alignment}"
                );
            }
        }
    }
}
