//! Captured production structural behavior before the engine region migration.
//! Deliberately excludes clocks, process-global image revisions and source epochs.
use super::*;
use std::collections::BTreeMap;

type Snapshot = BTreeMap<String, String>;

fn seeded() -> TerminalState {
    let mut t = TerminalState::new(6, 5);
    seed_active(&mut t);
    for row in 0..5 {
        t.screen.alt_grid[row][0].character = 'H';
    }
    t
}

fn seed_active(t: &mut TerminalState) {
    for row in 0..5 {
        t.screen.grid.row_wrapped[row] = row % 2 == 0;
        for col in 0..6 {
            let c = t.screen.grid.get_mut(row, col);
            *c = TerminalCell::default();
            c.character = char::from(b'A' + row as u8);
            c.foreground = Color::Indexed(row as u8);
            c.flags.set_bold(row % 2 != 0);
            c.flags.set_italic(col % 2 != 0);
            c.hyperlink_id = HyperlinkId::from_raw(row as u16 + 1);
        }
    }
    t.screen.grid[2][1].character = '界';
    t.screen.grid[2][1].flags.set_wide(true);
    t.screen.grid[2][2].character = ' ';
    t.screen.grid[2][2].flags.set_wide_continuation(true);
    t.screen.current_bg = Color::Blue;
    t.screen.current_flags.set_protected(true);
    t.screen.cursor_row = 2;
    t.screen.cursor_col = 5;
    t.screen.pending_wrap = true;
}

fn prime(t: &mut TerminalState) {
    t.get_visible_cells();
    t.projected_viewport(HistoryProjection::identity(), true);
    t.dirty_region.clear();
    t.grid_version = 77;
    t.row_versions.fill(3);
}

fn snapshot(t: &TerminalState) -> Snapshot {
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
        scroll_region_bottom
    );
    out.insert("grid".into(), format!("{:?}", t.screen.grid.to_vec()));
    out.insert("grid_ids".into(), format!("{:?}", t.screen.grid.row_ids()));
    out.insert("wraps".into(), format!("{:?}", t.screen.grid.row_wrapped));
    out.insert("hidden".into(), format!("{:?}", t.screen.alt_grid.to_vec()));
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
                .map(|r| (r.raw_row_id(), r.is_wrapped, r.decompress()))
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
    out
}

fn cases() -> BTreeMap<String, Snapshot> {
    let mut all = BTreeMap::new();
    for up in [false, true] {
        for top in 0..5 {
            for bottom in top..5 {
                let mut t = seeded();
                prime(&mut t);
                if up {
                    t.scroll_region_up(top, bottom);
                } else {
                    t.scroll_region_down(top, bottom);
                }
                all.insert(format!("scroll_{up}_{top}_{bottom}"), snapshot(&t));
            }
        }
        for count in [0, 1, 2, 65535] {
            for cursor in [0, 2, 4] {
                let mut t = seeded();
                t.screen.scroll_region_top = 1;
                t.screen.scroll_region_bottom = 3;
                t.screen.cursor_row = cursor;
                prime(&mut t);
                t.process_input(format!("\x1b[{count}{}", if up { 'M' } else { 'L' }).as_bytes());
                all.insert(format!("lines_{up}_{count}_{cursor}"), snapshot(&t));
            }
            for (top, bottom) in [(0, 4), (1, 3)] {
                let mut t = seeded();
                t.screen.scroll_region_top = top;
                t.screen.scroll_region_bottom = bottom;
                prime(&mut t);
                t.process_input(format!("\x1b[{count}{}", if up { 'S' } else { 'T' }).as_bytes());
                all.insert(format!("count_{up}_{count}_{top}_{bottom}"), snapshot(&t));
            }
        }
    }
    for alt in [false, true] {
        for sync in [false, true] {
            for cli in [false, true] {
                for top in [0, 1] {
                    let mut t = seeded();
                    if alt {
                        t.process_input(b"\x1b[?1049h");
                        seed_active(&mut t);
                    }
                    if sync {
                        t.process_input(b"\x1b[?2026h");
                    }
                    t.set_inline_cli_launch(cli);
                    prime(&mut t);
                    t.scroll_region_up(top, 3);
                    all.insert(format!("policy_{alt}_{sync}_{cli}_{top}"), snapshot(&t));
                }
            }
        }
    }
    for reverse in [false, true] {
        for copy in [false, true] {
            let mut t = seeded();
            t.set_max_scrollback(2);
            for row in 0..2 {
                let line = ScrollbackLine::compress(&t.screen.grid[row], false);
                t.push_scrollback_compressed(line);
            }
            let (a, b) = ((1, 0), (4, 3));
            t.selection = Some(Selection {
                anchor: if reverse { b } else { a },
                active: if reverse { a } else { b },
                mode: SelectionMode::Normal,
            });
            t.scroll_offset = 1;
            prime(&mut t);
            if copy {
                t.archive_visible_screen_to_scrollback();
            } else {
                for _ in 0..3 {
                    t.scroll_region_up(0, 4);
                }
            }
            all.insert(format!("selection_{reverse}_{copy}"), snapshot(&t));
        }
    }
    for command in [b"\x1b[2S".as_slice(), b"\x1b[2T", b"\x1b[2L", b"\x1b[2M"] {
        let mut t = seeded();
        t.next_raw_row_id = u64::MAX;
        t.screen.grid.row_ids_mut()[1] = RawRowId::UNTRACKED;
        prime(&mut t);
        t.process_input(command);
        all.insert(format!("exhaustion_{command:?}"), snapshot(&t));
    }
    for command in [b"\x1b[2S".as_slice(), b"\x1b[2T", b"\x1b[2L", b"\x1b[2M"] {
        let mut t = seeded();
        t.process_input(b"\x1b[1;1H\x1b]133;A\x07\x1b]133;C;id=first\x07AA\x1b]133;D;0;id=first\x07\x1b[2;1H\x1b]133;A\x07\x1b]133;C;id=second\x07BB\x1b]133;D;0;id=second\x07\x1b[3;1H\x1b]133;A\x07\x1b]133;C;id=active\x07CC");
        prime(&mut t);
        t.process_input(command);
        all.insert(format!("provenance_{command:?}"), snapshot(&t));
    }
    for command in [b"\x1b[S".as_slice(), b"\x1b[T", b"\x1b[L", b"\x1b[M"] {
        let mut t = seeded();
        t.process_input(b"\x1b[1;1H\x1b_Ga=T,i=41,f=32,s=1,v=1,r=2,c=1,C=1;/wAA/w==\x1b\\\x1b[3;2H\x1b_Ga=p,i=41,p=2,r=2,c=1,C=1\x1b\\");
        prime(&mut t);
        t.process_input(command);
        all.insert(format!("kitty_{command:?}"), snapshot(&t));
    }
    for command in [b"\n".as_slice(), b"\x1bD", b"\x1bM", b"x"] {
        let mut t = seeded();
        t.screen.cursor_row = 4;
        prime(&mut t);
        t.process_input(command);
        all.insert(format!("advance_{command:?}"), snapshot(&t));
    }
    all
}

#[test]
fn structural_operations_match_pre_extraction_production() {
    #[derive(serde::Deserialize)]
    struct CapturedCases {
        values: Vec<String>,
        cases: BTreeMap<String, BTreeMap<String, usize>>,
    }
    // Intern identical lane values to keep the exact baseline compact, without
    // replacing actual values with hashes or recalculating expected behavior.
    let captured: CapturedCases = serde_json::from_str(include_str!(
        "../../tests/fixtures/structural_before_engine.json"
    ))
    .unwrap();
    let expected: BTreeMap<String, Snapshot> = captured
        .cases
        .into_iter()
        .map(|(name, lanes)| {
            (
                name,
                lanes
                    .into_iter()
                    .map(|(lane, index)| (lane, captured.values[index].clone()))
                    .collect(),
            )
        })
        .collect();
    let actual = cases();
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    for (name, expected) in expected {
        assert_eq!(
            actual[&name].keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>(),
            "case {name}"
        );
        for (lane, value) in expected {
            assert_eq!(actual[&name][&lane], value, "case {name}, lane {lane}");
        }
    }
}

#[test]
#[ignore = "explicit capture only; never rewrites the committed baseline"]
fn print_structural_baseline() {
    for case in cases() {
        println!(
            "STRUCTURAL_BASELINE\t{}",
            serde_json::to_string(&case).unwrap()
        );
    }
}
