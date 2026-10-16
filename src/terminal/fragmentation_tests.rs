//! Metamorphic tests of the production parser, not a second parser oracle.
use super::*;
use std::collections::BTreeMap;

const CASES: &[(&str, &[u8])] = &[
    (
        "osc8",
        include_bytes!("../../tests/fixtures/parser/osc8.bin"),
    ),
    (
        "osc52",
        include_bytes!("../../tests/fixtures/parser/osc52.bin"),
    ),
    (
        "osc133",
        include_bytes!("../../tests/fixtures/parser/osc133.bin"),
    ),
    ("dcs", include_bytes!("../../tests/fixtures/parser/dcs.bin")),
    (
        "kitty",
        include_bytes!("../../tests/fixtures/parser/kitty.bin"),
    ),
    (
        "utf8",
        include_bytes!("../../tests/fixtures/parser/utf8.bin"),
    ),
    (
        "cancel",
        include_bytes!("../../tests/fixtures/parser/cancel.bin"),
    ),
    (
        "modes",
        include_bytes!("../../tests/fixtures/parser/modes.bin"),
    ),
    (
        "aborted",
        include_bytes!("../../tests/fixtures/parser/aborted.bin"),
    ),
];

// Debug formatting retains every cell attribute, including link IDs. Keep the
// fields separately named so a failed comparison identifies the semantic lane.
// Exclude batch-count versions/caches, wall clocks, and global image revisions:
// these legitimately vary between equivalent runs. This is not serialization.
fn snapshot(t: &TerminalState) -> BTreeMap<&'static str, String> {
    let mut result = BTreeMap::new();
    macro_rules! fields {
        ($($field:ident),* $(,)?) => {$(result.insert(stringify!($field), format!("{:?}", t.$field));)*};
    }
    fields!(
        cursor_row,
        cursor_col,
        cursor_shape,
        saved_cursor_row,
        saved_cursor_col,
        alt_cursor_row,
        alt_cursor_col,
        use_alt_buffer,
        insert_mode,
        origin_mode,
        pending_wrap,
        current_fg,
        current_bg,
        current_flags,
        current_hyperlink,
        window_title,
        icon_title,
        title_stack,
        announced_jsh_session_id,
        current_working_dir,
        global_bg,
        scroll_region_top,
        scroll_region_bottom,
        g0_charset,
        g1_charset,
        active_charset,
        tab_stops,
        last_printed_char,
        modes,
        output_buffer,
        keyboard_enhancement_flags,
        keyboard_enhancement_stack,
        alt_keyboard_enhancement_flags,
        alt_keyboard_enhancement_stack,
        xterm_modify_other_keys,
        xterm_format_other_keys,
        sync_output_active,
        pending_osc52_clipboard_set,
        pending_osc52_clipboard_queries,
        pending_clipboard_requests,
        pending_notifications,
        pending_bell,
        dynamic_fg,
        dynamic_bg,
        dynamic_cursor_color,
        dynamic_palette,
        command_marks,
        next_command_sequence,
        consumed_command_ids,
        total_lines_scrolled,
        pending_escape,
        pending_apc,
        pending_osc,
        pending_string,
        discarding_oversized_apc,
        discarding_apc_prev_escape
    );
    for (name, saved) in [
        ("saved_cursor", &t.saved_state),
        ("saved_primary", &t.saved_primary_screen_state),
    ] {
        result.insert(
            name,
            format!(
                "{:?}",
                saved.as_ref().map(|s| (
                    (s.row, s.col, s.fg, s.bg, s.flags, s.g0, s.g1, s.active),
                    (s.origin_mode, s.autowrap, s.pending_wrap)
                ))
            ),
        );
    }
    result.insert("grid", format!("{:?}", t.grid.to_vec()));
    result.insert("alt_grid", format!("{:?}", t.alt_grid.to_vec()));
    result.insert(
        "wraps",
        format!("{:?}", (&t.grid.row_wrapped, &t.alt_grid.row_wrapped)),
    );
    result.insert(
        "history",
        format!(
            "{:?}",
            t.scrollback
                .iter()
                .map(|r| (r.decompress(), r.is_wrapped))
                .collect::<Vec<_>>()
        ),
    );
    result.insert(
        "links",
        format!(
            "{:?}",
            t.grid
                .iter()
                .flatten()
                .map(|c| t.hyperlink_uri(c.hyperlink_id))
                .collect::<Vec<_>>()
        ),
    );
    result.insert(
        "utf8",
        format!("{:?}", (&t.utf8_buf[..t.utf8_len as usize], t.utf8_len)),
    );
    let records = t
        .command_records
        .iter()
        .cloned()
        .map(|mut r| {
            r.started_at = None;
            r.finished_at = None;
            r.started_instant = None;
            // A producer-supplied duration is deterministic; local elapsed time isn't.
            r.duration_ms = None;
            r
        })
        .collect::<Vec<_>>();
    result.insert("commands", format!("{records:?}"));
    let completed = t
        .pending_completed_command_outputs
        .iter()
        .cloned()
        .map(|mut e| {
            e.completed.duration_ms = None;
            e
        })
        .collect::<Vec<_>>();
    result.insert("completed", format!("{completed:?}"));
    result.insert("image_count", format!("{}", t.kitty_graphics.image_count()));
    result.insert(
        "placements",
        format!("{:?}", t.kitty_graphics.get_placements()),
    );
    // Corpus IDs are explicitly bounded, and pixels are compared, not revisions.
    result.insert(
        "images",
        format!(
            "{:?}",
            [41, 42, 99].map(|id| t.kitty_graphics.get_image(id).map(|i| (
                i.id,
                i.protocol_id,
                i.image_number,
                i.width,
                i.height,
                &i.data
            )))
        ),
    );
    result
}

fn run(bytes: &[u8], ends: impl IntoIterator<Item = usize>) -> TerminalState {
    let mut terminal = TerminalState::new(24, 6);
    let mut start = 0;
    for end in ends {
        assert!(end >= start && end <= bytes.len());
        terminal.process_input(&bytes[start..end]);
        for pending in [
            &terminal.pending_escape,
            &terminal.pending_apc,
            &terminal.pending_osc,
            &terminal.pending_string,
        ] {
            assert!(pending.len() <= MAX_PENDING_ESCAPE);
        }
        assert!(terminal.command_records.len() <= MAX_COMMAND_MARKS);
        start = end;
    }
    assert_eq!(
        start,
        bytes.len(),
        "partition must consume the entire fixture"
    );
    terminal
}

fn assert_partition(name: &str, bytes: &[u8], expected: &BTreeMap<&str, String>, ends: &[usize]) {
    let actual = snapshot(&run(bytes, ends.iter().copied()));
    for (field, expected) in expected {
        assert_eq!(
            &actual[field], expected,
            "case={name}, field={field}, cuts={ends:?}"
        );
    }
}

// Specified PRNG, seed, and bounded partition size make failures replayable
// without a fuzz dependency or a platform-dependent random distribution.
fn random_ends(len: usize, seed: u64, max_chunk: usize) -> Vec<usize> {
    let mut state = seed + 1;
    let mut end = 0;
    let mut ends = Vec::new();
    while end < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        end = (end + 1 + state as usize % max_chunk).min(len);
        ends.push(end);
    }
    ends
}

#[test]
fn corpus_every_two_way_cut_and_one_byte_reads() {
    for &(name, bytes) in CASES {
        let expected = snapshot(&run(bytes, [bytes.len()]));
        for cut in 0..=bytes.len() {
            assert_partition(name, bytes, &expected, &[cut, bytes.len()]);
        }
        assert_partition(
            name,
            bytes,
            &expected,
            &(1..=bytes.len()).collect::<Vec<_>>(),
        );
    }
}

#[test]
fn corpus_one_hundred_seeded_partitions() {
    for &(name, bytes) in CASES {
        let expected = snapshot(&run(bytes, [bytes.len()]));
        for seed in 0..100 {
            assert_partition(name, bytes, &expected, &random_ends(bytes.len(), seed, 23));
        }
    }
    // Composition catches carry-over between unlike protocols, not just reset cases.
    let bytes = CASES[..CASES.len() - 1]
        .iter()
        .flat_map(|(_, b)| b.iter().copied())
        .collect::<Vec<_>>();
    let expected = snapshot(&run(&bytes, [bytes.len()]));
    for seed in 0..100 {
        assert_partition(
            "combined",
            &bytes,
            &expected,
            &random_ends(bytes.len(), seed, 31),
        );
    }
}

#[test]
fn corpus_oracle_negative_controls() {
    let bytes = CASES[2].1;
    let terminal = run(bytes, [bytes.len()]);
    assert!(
        !terminal.command_records.is_empty(),
        "identity lane must be exercised"
    );
    let expected = snapshot(&terminal);
    let mut changed = run(bytes, [bytes.len()]);
    changed.grid[0][0].character = '!';
    assert_ne!(snapshot(&changed)["grid"], expected["grid"]);
    let mut changed = run(bytes, [bytes.len()]);
    changed.modes.insert(2004);
    assert_ne!(snapshot(&changed)["modes"], expected["modes"]);
    let mut changed = run(bytes, [bytes.len()]);
    changed.output_buffer.push(0);
    assert_ne!(
        snapshot(&changed)["output_buffer"],
        expected["output_buffer"]
    );
    let mut changed = run(bytes, [bytes.len()]);
    changed.command_records.front_mut().unwrap().id.push('!');
    assert_ne!(snapshot(&changed)["commands"], expected["commands"]);
}

#[test]
fn corpus_known_effects_and_abnormal_reset_recovery() {
    let t = run(CASES[1].1, [CASES[1].1.len()]);
    assert_eq!(t.pending_osc52_clipboard_set.as_deref(), Some("hello"));
    assert_eq!(t.pending_osc52_clipboard_queries.len(), 2);
    let t = run(CASES[4].1, [CASES[4].1.len()]);
    assert_eq!(
        t.kitty_graphics.get_image(41).unwrap().data,
        [255, 0, 0, 255]
    );
    assert!(!t.output_buffer.is_empty());
    let t = run(CASES[8].1, [CASES[8].1.len()]);
    assert!(!t.use_alt_buffer);
    assert!(!t.modes.contains(&2004));
    assert!(t.command_records.is_empty());
    assert!(t.pending_osc.is_empty());
    assert_eq!(t.grid[0][0].character, 'R');
    assert!(!t.output_buffer.is_empty());
}

#[test]
fn oversized_kitty_packet_partition_invariance() {
    let mut bytes = b"\x1b_Ga=p,i=99,q=0;".to_vec();
    bytes.resize(MAX_PENDING_ESCAPE + 1, b'A');
    bytes.extend_from_slice(b"\x1b\\Z\x1b[6n");
    let expected = snapshot(&run(&bytes, [bytes.len()]));
    // All cuts would be quadratic on a 4 MiB input. Exercise introducer,
    // cap +/- 1, split ST, and 100 fixed seeds with large bounded chunks.
    for cut in [
        0,
        1,
        2,
        3,
        MAX_PENDING_ESCAPE - 1,
        MAX_PENDING_ESCAPE,
        MAX_PENDING_ESCAPE + 1,
        MAX_PENDING_ESCAPE + 2,
        bytes.len(),
    ] {
        assert_partition("oversized-kitty", &bytes, &expected, &[cut, bytes.len()]);
    }
    for seed in 0..100 {
        assert_partition(
            "oversized-kitty",
            &bytes,
            &expected,
            &random_ends(bytes.len(), seed, 128 * 1024),
        );
    }
}

#[test]
fn oversized_osc_and_dcs_characterize_batch_scoped_drop_then_reset() {
    // Existing documented policy differs from Kitty: pending OSC/DCS overflow
    // drops the current read and returns to ground. Thus it is deliberately NOT
    // partition invariant beyond the cap. Keep that distinction visible rather
    // than silently claiming equivalence or changing production behavior here.
    for prefix in [b"\x1b]0;".as_slice(), b"\x1bP".as_slice()] {
        let mut bytes = prefix.to_vec();
        bytes.resize(MAX_PENDING_ESCAPE + 33, b'A');
        bytes.extend_from_slice(b"\x1b\\Z");
        let mut whole = run(&bytes, [bytes.len()]);
        let mut split = run(&bytes, [MAX_PENDING_ESCAPE + 1, bytes.len()]);
        assert!(whole.pending_osc.is_empty() && whole.pending_string.is_empty());
        assert!(split.pending_osc.is_empty() && split.pending_string.is_empty());
        assert_ne!(
            snapshot(&whole)["grid"],
            snapshot(&split)["grid"],
            "revisit this characterization if the overflow policy changes"
        );
        whole.process_input(b"\x1bcRESTORED\x1b[6n");
        split.process_input(b"\x1bcRESTORED\x1b[6n");
        assert_eq!(snapshot(&whole), snapshot(&split));
    }
}

#[test]
fn minimal_interrupted_utf8_prefix_has_one_replacement() {
    let bytes = b"\xe2\x82X";
    for ends in [vec![3], vec![1, 3], vec![2, 3], vec![1, 2, 3]] {
        let t = run(bytes, ends);
        assert_eq!(t.cursor_col, 2);
        assert_eq!(t.grid[0][0].character, '\u{fffd}');
        assert_eq!(t.grid[0][1].character, 'X');
    }
}

#[test]
fn minimal_kitty_cancel_after_introducer_preserves_reply() {
    let bytes = b"\x1b_Gi=99;\x18Z";
    let whole = run(bytes, [bytes.len()]);
    assert!(whole.output_buffer.starts_with(b"\x1b_Gi=99;EINVAL:"));
    assert_partition(
        "early-kitty-cancel",
        bytes,
        &snapshot(&whole),
        &[2, bytes.len()],
    );
}

#[test]
fn utf8_replacement_matches_standard_library_for_all_partitions() {
    let cases: &[&[u8]] = &[
        b"\xe2\x82X",
        b"\xe0\x80\x80",
        b"\xed\xa0\x80",
        b"\xf4\x90\x80\x80",
        b"\xc0\xaf",
        b"\xc1\xbf",
        b"\xf5\x80\x80\x80",
        b"\xff\xfe",
        b"\xe0\x9f",
        b"\xed\xbf",
        b"\xf0\x8f",
        b"\xf4\x90",
        b"\xc2",
        b"\xe2\x82",
        b"\xf0\x90\x80",
        b"\x80\xbf",
        "é界🙂".as_bytes(),
        b"\xf0\x90X\x80",
        b"\xe2\xc2\xa2",
    ];
    for bytes in cases {
        // An unfinished prefix stays buffered at a read boundary, not replaced
        // merely because read() ended. ASCII X then makes EOF lossiness explicit.
        let mut terminated = bytes.to_vec();
        terminated.push(b'X');
        let expected = String::from_utf8_lossy(&terminated);
        for cut in 0..=terminated.len() {
            let t = run(&terminated, [cut, terminated.len()]);
            let rendered: String = t.grid[0][..t.cursor_col]
                .iter()
                .filter(|c| !c.flags.wide_continuation())
                .map(|c| c.character)
                .collect();
            assert_eq!(rendered, expected, "bytes={bytes:?}, cut={cut}");
        }
        let t = run(&terminated, 1..=terminated.len());
        let rendered: String = t.grid[0][..t.cursor_col]
            .iter()
            .filter(|c| !c.flags.wide_continuation())
            .map(|c| c.character)
            .collect();
        assert_eq!(rendered, expected, "byte reads: {bytes:?}");
    }
    for bytes in [
        b"\xc2".as_slice(),
        b"\xe2\x82".as_slice(),
        b"\xf0\x90\x80".as_slice(),
    ] {
        let t = run(bytes, 1..=bytes.len());
        assert_eq!(t.cursor_col, 0, "unfinished scalar is pending, not EOF");
        assert_eq!(&t.utf8_buf[..t.utf8_len as usize], bytes);
    }
}

#[test]
fn kitty_recovery_controls_at_cap_keep_quiet_and_identity() {
    for quiet in [0, 2] {
        let suffix = format!(",i=99,q={quiet}");
        let mut bytes = b"\x1b_G".to_vec();
        bytes.resize(
            3 + jterm_core::kitty_graphics::MAX_CONTROL_BYTES - suffix.len(),
            b'x',
        );
        bytes.extend_from_slice(suffix.as_bytes());
        bytes.extend_from_slice(b";\x18Z");
        let whole = run(&bytes, [bytes.len()]);
        assert_eq!(whole.output_buffer.is_empty(), quiet == 2);
        if quiet == 0 {
            assert!(whole.output_buffer.starts_with(b"\x1b_Gi=99;EINVAL:"));
        }
        for cut in [1, 2, 3, bytes.len() - 4, bytes.len() - 2] {
            assert_partition(
                "control-cap",
                &bytes,
                &snapshot(&whole),
                &[cut, bytes.len()],
            );
        }
    }
}

#[test]
fn oversized_kitty_discard_is_bounded_and_does_not_amplify_replies() {
    for quiet in [0, 2] {
        for terminator in [b"\x1b\\".as_slice(), b"\x18".as_slice(), b"\x1a".as_slice()] {
            let mut t = TerminalState::new(24, 6);
            t.process_input(format!("\x1b_Gi=99,q={quiet};").as_bytes());
            t.process_input(&vec![b'A'; MAX_PENDING_ESCAPE]);
            assert!(t.pending_apc.is_empty());
            assert!(t.discarding_oversized_apc);
            let reply = t.get_output();
            assert_eq!(reply.is_empty(), quiet == 2);
            assert!(reply.len() < 256);
            for _ in 0..8 {
                t.process_input(&[b'A'; 1024]);
                assert!(t.pending_apc.is_empty());
                assert!(t.get_output().is_empty(), "one rejection per packet");
            }
            t.process_input(terminator);
            t.process_input(b"RECOVERED");
            assert!(!t.discarding_oversized_apc);
            assert_eq!(t.grid[0][0].character, 'R');
            assert!(t.command_records.is_empty());
            assert!(t.pending_completed_command_outputs.is_empty());
        }
    }
}
