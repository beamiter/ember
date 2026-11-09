//! Independent production resize snapshots captured before physical extraction.
//! Excludes process-global epochs, image revisions and clock-valued host state.
use super::*;
use std::collections::BTreeMap;
type Snapshot = BTreeMap<String, String>;

fn seeded(cols: usize, rows: usize, variant: usize) -> TerminalState {
    let mut t = TerminalState::new(cols, rows);
    for (buffer, grid) in [&mut t.screen.grid, &mut t.screen.alt_grid]
        .into_iter()
        .enumerate()
    {
        for row in 0..rows {
            grid.row_wrapped[row] = (row + buffer) % 2 == 0;
            for col in 0..cols {
                let cell = grid.get_mut(row, col);
                cell.character = char::from(b'A' + ((row + buffer * 8) % 26) as u8);
                cell.foreground = Color::Red;
                cell.background = Color::Green;
                cell.flags.set_bold(row % 2 == 0);
                cell.flags.set_protected(row % 2 == 1);
                cell.hyperlink_id = HyperlinkId::from_raw(row as u16 + 1);
            }
            if cols > 1 {
                grid[row][cols - 2].character = '界';
                grid[row][cols - 2].flags.set_wide(true);
                grid[row][cols - 1].character = ' ';
                grid[row][cols - 1].flags.set_wide_continuation(true);
            }
            if variant & 1 == 0 {
                grid[row][cols - 1].flags.set_wide(true);
            }
        }
    }
    t.screen.current_fg = Color::Magenta;
    t.screen.current_bg = Color::Blue;
    t.screen.current_flags.set_protected(true);
    t.screen.current_hyperlink = HyperlinkId::from_raw(99);
    t.screen.cursor_row = variant % rows;
    t.screen.cursor_col = cols - 1;
    t.screen.saved_cursor_row = rows + 1;
    t.screen.saved_cursor_col = cols + 1;
    t.screen.alt_cursor_row = rows + 2;
    t.screen.alt_cursor_col = cols + 2;
    t.screen.pending_wrap = true;
    t.screen.g1_charset = Charset::DecSpecialGraphics;
    t.screen.active_charset = Charset::DecSpecialGraphics;
    t.screen.origin_mode = true;
    t.screen.insert_mode = true;
    t.screen.last_printed_char = Some('界');
    t.screen.tab_stops.fill(false);
    t.screen.tab_stops[cols - 1] = true;
    t.screen.save_cursor_state(&t.protocol);
    t.screen.saved_state.as_mut().unwrap().row = rows + 3;
    t.screen.saved_state.as_mut().unwrap().col = cols + 3;
    t.screen.saved_primary_screen_state = t.screen.saved_state.clone();
    match variant % 4 {
        1 => {
            t.screen.scroll_region_top = 1.min(rows - 1);
            t.screen.scroll_region_bottom = rows - 1;
        }
        2 => {
            t.screen.scroll_region_top = rows + 2;
            t.screen.scroll_region_bottom = 0;
        }
        3 => {
            t.screen.scroll_region_top = 0;
            t.screen.scroll_region_bottom = rows + 2;
        }
        _ => {}
    }
    t
}

fn prime(t: &mut TerminalState) {
    t.get_visible_cells();
    t.projected_viewport(HistoryProjection::identity(), true);
    t.dirty_region.clear();
    t.grid_version = 77;
    t.row_versions.fill(3);
}

// Lossless run-length encoding keeps huge clamped blank grids compact. Wide
// flags are explicit because StyleFlags' diagnostic Debug omits those two bits.
fn exact_rows<'a>(rows: impl Iterator<Item = &'a [TerminalCell]>) -> String {
    let rows: Vec<_> = rows
        .map(|row| {
            let mut runs: Vec<(usize, TerminalCell, bool, bool)> = Vec::new();
            for &cell in row {
                if let Some(last) = runs.last_mut().filter(|last| last.1 == cell) {
                    last.0 += 1;
                } else {
                    runs.push((1, cell, cell.flags.wide(), cell.flags.wide_continuation()));
                }
            }
            runs
        })
        .collect();
    format!("{rows:?}")
}

fn snapshot(t: &mut TerminalState) -> Snapshot {
    let mut out = Snapshot::new();
    macro_rules! host {
        ($($field:ident),* $(,)?) => {$(out.insert(stringify!($field).into(), format!("{:?}", t.$field));)*};
    }
    host!(
        next_raw_row_id,
        total_lines_scrolled,
        row_identity_revision,
        full_screen_scroll_revision,
        grid_version,
        row_versions,
        dirty_region,
        selection,
        selection_revision,
        scroll_offset,
        max_scrollback,
        search_source_revision,
        finished_output_revision,
        active_output_provenance,
        output_buffer,
        last_archived_screen_snapshot,
        last_synced_primary_screen_snapshot
    );
    macro_rules! screen {
        ($($field:ident),* $(,)?) => {$(out.insert(stringify!($field).into(), format!("{:?}", t.screen.$field));)*};
    }
    screen!(
        cursor_row,
        cursor_col,
        pending_wrap,
        use_alt_buffer,
        current_fg,
        current_bg,
        current_flags,
        current_hyperlink,
        scroll_region_top,
        scroll_region_bottom,
        saved_cursor_row,
        saved_cursor_col,
        alt_cursor_row,
        alt_cursor_col,
        cursor_shape,
        saved_primary_cursor_shape,
        saved_state,
        saved_primary_screen_state,
        tab_stops,
        g0_charset,
        g1_charset,
        active_charset,
        origin_mode,
        insert_mode,
        last_printed_char
    );
    out.insert("grid".into(), exact_rows(t.screen.grid.iter()));
    out.insert("grid_ids".into(), format!("{:?}", t.screen.grid.row_ids()));
    out.insert("wraps".into(), format!("{:?}", t.screen.grid.row_wrapped));
    out.insert("hidden".into(), exact_rows(t.screen.alt_grid.iter()));
    out.insert(
        "hidden_ids".into(),
        format!("{:?}", t.screen.alt_grid.row_ids()),
    );
    out.insert(
        "hidden_wraps".into(),
        format!("{:?}", t.screen.alt_grid.row_wrapped),
    );
    out.insert(
        "history".into(),
        format!(
            "{:?}",
            t.scrollback
                .iter()
                .map(|r| (
                    r.raw_row_id(),
                    r.is_wrapped,
                    exact_rows(std::iter::once(r.decompress().as_slice()))
                ))
                .collect::<Vec<_>>()
        ),
    );
    out.insert(
        "owners".into(),
        format!(
            "{:?}",
            t.finished_output_owners.iter().collect::<BTreeMap<_, _>>()
        ),
    );
    out.insert(
        "provenance".into(),
        format!(
            "{:?}",
            t.finished_output_provenance
                .iter()
                .collect::<BTreeMap<_, _>>()
        ),
    );
    out.insert(
        "kitty".into(),
        format!("{:?}", t.kitty_graphics.get_placements()),
    );
    out.insert(
        "caches".into(),
        format!(
            "{:?}",
            (
                t.visible_cells_cache.is_some(),
                t.projected_viewport_cache.is_some(),
                t.projection_plan_cache.is_some(),
                t.transformed_viewport_cache.is_some(),
                t.viewport_mapping_exact_cache.get().is_some()
            )
        ),
    );
    out.insert("dimensions".into(), format!("{:?}", t.get_dimensions()));
    out.insert(
        "visible".into(),
        exact_rows(t.get_visible_cells().iter().map(Vec::as_slice)),
    );
    let viewport = t.projected_viewport(HistoryProjection::identity(), true);
    out.insert(
        "projected".into(),
        exact_rows(viewport.cells().iter().map(Vec::as_slice)),
    );
    out
}

fn cases() -> BTreeMap<String, Snapshot> {
    let mut all = BTreeMap::new();
    for (old_cols, old_rows) in [(1, 1), (2, 2), (6, 5), (17, 5)] {
        for (cols, rows) in [(0, 3), (3, 0), (1, 1), (2, 3), (6, 5), (9, 7), (18, 2)] {
            for variant in 0..8 {
                let mut t = seeded(old_cols, old_rows, variant);
                prime(&mut t);
                t.on_resize(cols, rows);
                all.insert(
                    format!("geometry_{old_cols}_{old_rows}_{cols}_{rows}_{variant}"),
                    snapshot(&mut t),
                );
            }
        }
    }
    for alt in [false, true] {
        for blanks in 0..5 {
            for cursor in [0, 2, 4] {
                let mut t = seeded(6, 5, cursor);
                t.screen.use_alt_buffer = alt;
                for row in 5 - blanks..5 {
                    t.screen.grid[row].fill(TerminalCell::default());
                }
                prime(&mut t);
                t.on_resize(4, 2);
                all.insert(
                    format!("blank_tail_{alt}_{blanks}_{cursor}"),
                    snapshot(&mut t),
                );
            }
        }
    }
    // BCE-colored whitespace is not a trimmable blank tail.
    for style in ["bg", "fg", "link", "protected", "wrapped"] {
        let mut t = seeded(6, 5, 1);
        for row in 2..5 {
            t.screen.grid[row].fill(TerminalCell::default());
            for cell in &mut t.screen.grid[row] {
                match style {
                    "bg" => cell.background = Color::Red,
                    "fg" => cell.foreground = Color::Red,
                    "link" => cell.hyperlink_id = HyperlinkId::from_raw(99),
                    "protected" => cell.flags.set_protected(true),
                    _ => {}
                }
            }
            t.screen.grid.row_wrapped[row] = true;
        }
        prime(&mut t);
        t.on_resize(6, 2);
        all.insert(format!("blank_style_{style}"), snapshot(&mut t));
    }
    for alt in [false, true] {
        for sync in [false, true] {
            for cli in [false, true] {
                let mut t = seeded(6, 5, 4);
                t.screen.use_alt_buffer = alt;
                if sync {
                    t.process_input(b"\x1b[?2026h");
                }
                t.set_inline_cli_launch(cli);
                prime(&mut t);
                t.on_resize(4, 2);
                all.insert(
                    format!("archive_policy_{alt}_{sync}_{cli}"),
                    snapshot(&mut t),
                );
            }
        }
    }
    for reverse in [false, true] {
        for cap in [0, 1, 2, 10] {
            let mut t = seeded(6, 5, 4);
            t.set_max_scrollback(cap);
            for row in 0..3 {
                t.push_scrollback_compressed(ScrollbackLine::compress(
                    &t.screen.grid[row],
                    row % 2 == 0,
                ));
            }
            let (a, b) = ((1, 0), (5, 5));
            t.selection = Some(Selection {
                anchor: if reverse { b } else { a },
                active: if reverse { a } else { b },
                mode: SelectionMode::Normal,
            });
            t.scroll_offset = 1;
            prime(&mut t);
            t.on_resize(3, 2);
            all.insert(format!("history_{reverse}_{cap}"), snapshot(&mut t));
        }
    }
    for next in [0, u64::MAX, u64::MAX - 1] {
        for (cols, rows) in [(4, 2), (9, 7), (6, 5)] {
            let mut t = seeded(6, 5, 4);
            t.next_raw_row_id = next;
            t.screen.grid.row_ids_mut()[1] = RawRowId::UNTRACKED;
            t.screen.alt_grid.row_ids_mut()[0] = RawRowId::UNTRACKED;
            prime(&mut t);
            t.on_resize(cols, rows);
            all.insert(format!("identity_{next}_{cols}_{rows}"), snapshot(&mut t));
        }
    }
    for (cols, rows) in [(6, 5), (4, 2), (9, 7), (0, 0)] {
        let mut t = seeded(6, 5, 4);
        t.process_input(b"\x1b[?6l\x1b[r\x1b[1;1H\x1b]133;A\x07\x1b]133;C;id=first\x07AA\x1b]133;D;0;id=first\x07\x1b[2;1H\x1b]133;A\x07\x1b]133;C;id=active\x07BB");
        prime(&mut t);
        t.on_resize(cols, rows);
        all.insert(format!("provenance_{cols}_{rows}"), snapshot(&mut t));
    }
    for alt in [false, true] {
        for (cols, rows) in [(4, 2), (9, 7)] {
            let mut t = seeded(6, 5, 4);
            t.process_input(b"\x1b[?6l\x1b[r\x1b[1;1H\x1b_Ga=T,i=41,f=32,s=1,v=1,r=2,c=1,C=1;/wAA/w==\x1b\\\x1b[3;2H\x1b_Ga=p,i=41,p=2,r=2,c=1,C=1\x1b\\");
            if alt {
                t.process_input(b"\x1b[?1049h");
            }
            t.screen.cursor_row = 4;
            prime(&mut t);
            t.on_resize(cols, rows);
            all.insert(format!("kitty_{alt}_{cols}_{rows}"), snapshot(&mut t));
        }
    }
    for mode in [47, 1047, 1049] {
        let mut t = seeded(6, 5, 4);
        t.process_input(format!("\x1b[?{mode}h\x1b[44mALT").as_bytes());
        prime(&mut t);
        t.on_resize(9, 2);
        all.insert(format!("alt_resize_{mode}"), snapshot(&mut t));
        t.process_input(format!("\x1b[?{mode}l").as_bytes());
        all.insert(format!("alt_exit_{mode}"), snapshot(&mut t));
    }
    let mut t = seeded(6, 5, 4);
    for (step, (cols, rows)) in [(3, 2), (18, 7), (1, 1), (6, 5), (6, 5)]
        .into_iter()
        .enumerate()
    {
        prime(&mut t);
        t.on_resize(cols, rows);
        all.insert(format!("sequence_{step}"), snapshot(&mut t));
    }
    // Clamp each axis independently, avoiding a million-cell fixture.
    for (cols, rows) in [(usize::MAX, 1), (1, usize::MAX)] {
        let mut t = TerminalState::new(1, 1);
        prime(&mut t);
        t.on_resize(cols, rows);
        all.insert(format!("clamp_{cols}_{rows}"), snapshot(&mut t));
    }
    all
}

#[test]
fn resize_matches_pre_extraction_production() {
    #[derive(serde::Deserialize)]
    struct CapturedCases {
        values: Vec<String>,
        cases: BTreeMap<String, BTreeMap<String, usize>>,
    }
    let captured: CapturedCases = serde_json::from_str(include_str!(
        "../../tests/fixtures/resize_before_engine.json"
    ))
    .unwrap();
    let actual = cases();
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        captured.cases.keys().collect::<Vec<_>>()
    );
    for (name, expected) in captured.cases {
        assert_eq!(
            actual[&name].keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>(),
            "case {name}"
        );
        for (lane, index) in expected {
            assert_eq!(
                actual[&name][&lane], captured.values[index],
                "case {name}, lane {lane}"
            );
        }
    }
}

#[test]
#[ignore = "explicit capture tooling only; never rewrites the committed baseline"]
fn print_resize_baseline() {
    for case in cases() {
        println!("RESIZE_BASELINE\t{}", serde_json::to_string(&case).unwrap());
    }
}
