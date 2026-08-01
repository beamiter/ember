use super::hyperlink::{MAX_OSC8_PARAMS_BYTES, MAX_OSC8_URI_BYTES};
use super::{
    ClipboardReadKind, ClipboardReadRequest, Color, CommandState, ExtractedText, HyperlinkId,
    ScrollbackLine, TerminalCell, TerminalState, UnderlineStyle, MAX_CAPTURED_COMMAND_OUTPUT_BYTES,
    MAX_COMPLETED_COMMAND_OUTPUT_BYTES, MAX_OSC_133_COMMAND_BYTES, MAX_OSC_133_ID_BYTES,
    MAX_PENDING_ESCAPE,
};

// `a=t` is the protocol default. Omitting it also guards against regressing to
// heuristic routing based on searching the body for an `a=` substring.
const KITTY_ONE_PIXEL_RGBA_APC: &[u8] = b"\x1b_Gi=41,f=32,s=1,v=1;/wAA/w==\x1b\\";

#[test]
fn osc8_hyperlink_survives_every_input_batch_boundary() {
    const TARGET: &str = "https://example.test/real-target";
    const LABEL: &str = "masked label";
    let opening = format!("\x1b]8;id=fragmented;{TARGET}\x1b\\");

    for split_at in 1..opening.len() {
        let mut terminal = TerminalState::new(32, 2);
        terminal.process_input(&opening.as_bytes()[..split_at]);
        terminal.process_input(&opening.as_bytes()[split_at..]);
        terminal.process_input(LABEL.as_bytes());
        terminal.process_input(b"\x1b]8;;\x1b\\X");

        let id = terminal.grid[0][0].hyperlink_id;
        assert!(!id.is_none(), "OSC 8 was lost at input split {split_at}");
        assert_eq!(terminal.hyperlink_uri(id), Some(TARGET));
        assert!(terminal.grid[0][..LABEL.len()]
            .iter()
            .all(|cell| cell.hyperlink_id == id));
        assert_eq!(terminal.grid[0][LABEL.len()].character, 'X');
        assert_eq!(
            terminal.grid[0][LABEL.len()].hyperlink_id,
            HyperlinkId::NONE
        );
    }
}

#[test]
fn osc8_rejects_oversized_control_and_unsafe_targets() {
    fn rendered_id(params: &str, target: &str) -> HyperlinkId {
        let mut terminal = TerminalState::new(4, 1);
        let sequence = format!("\x1b]8;{params};{target}\x1b\\X");
        terminal.process_input(sequence.as_bytes());
        terminal.grid[0][0].hyperlink_id
    }

    assert!(rendered_id(&"p".repeat(MAX_OSC8_PARAMS_BYTES + 1), "https://safe.test").is_none());
    assert!(rendered_id("id=bad\u{1}", "https://safe.test").is_none());
    assert!(rendered_id(
        "",
        &format!("https://safe.test/{}", "x".repeat(MAX_OSC8_URI_BYTES))
    )
    .is_none());
    assert!(rendered_id("", "https://safe.test/bad\u{7f}").is_none());
    assert!(rendered_id("", "javascript:alert(1)").is_none());
    assert!(rendered_id("", "data:text/html,hello").is_none());
    assert!(rendered_id("", "unknown-scheme:payload").is_none());

    // A rejected opening must not leave the preceding safe target active.
    let mut terminal = TerminalState::new(4, 1);
    terminal.process_input(b"\x1b]8;;https://safe.test\x1b\\A\x1b]8;;javascript:alert(1)\x1b\\B");
    assert!(!terminal.grid[0][0].hyperlink_id.is_none());
    assert!(terminal.grid[0][1].hyperlink_id.is_none());
}

#[test]
fn osc8_link_ids_survive_scrollback_reflow_and_selection() {
    const TARGET: &str = "https://example.test/history";
    let mut terminal = TerminalState::new(5, 2);
    terminal.process_input(format!("\x1b]8;;{TARGET}\x1b\\abcdefghijk\x1b]8;;\x1b\\").as_bytes());

    assert!(!terminal.scrollback.is_empty());
    let archived = terminal.scrollback[0].decompress();
    let id = archived[0].hyperlink_id;
    assert!(!id.is_none());
    assert_eq!(terminal.hyperlink_uri(id), Some(TARGET));
    assert!(archived[..5].iter().all(|cell| cell.hyperlink_id == id));

    let historical: Vec<ScrollbackLine> = terminal.scrollback.iter().cloned().collect();
    let reflowed = TerminalState::reflow_lines(&historical, 3, &TerminalCell::default());
    let reflowed_linked: Vec<_> = reflowed
        .iter()
        .flat_map(ScrollbackLine::decompress)
        .filter(|cell| cell.character != ' ')
        .collect();
    assert_eq!(reflowed_linked.len(), 5);
    assert!(reflowed_linked.iter().all(|cell| cell.hyperlink_id == id));

    terminal.on_resize(3, 2);
    terminal.scroll(1);
    let visible = terminal.get_visible_cells();
    assert!(visible.iter().flatten().any(|cell| cell.hyperlink_id == id));

    // Selection copies the masked text and leaves the cell metadata intact.
    terminal.scroll(-1);
    terminal.start_selection((0, 0));
    terminal.update_selection((0, 2));
    let before = terminal.grid[0][0].hyperlink_id;
    assert_eq!(terminal.copy_selection().as_deref(), Some("fgh"));
    assert_eq!(terminal.grid[0][0].hyperlink_id, before);
    assert_eq!(terminal.hyperlink_uri(before), Some(TARGET));
}

#[test]
fn osc8_marks_both_cells_of_a_wide_masked_label() {
    let mut terminal = TerminalState::new(4, 1);
    terminal.process_input("\x1b]8;;https://example.test/wide\x1b\\点\x1b]8;;\x1b\\".as_bytes());

    let id = terminal.grid[0][0].hyperlink_id;
    assert!(!id.is_none());
    assert_eq!(terminal.grid[0][1].hyperlink_id, id);
    assert!(terminal.grid[0][1].flags.wide_continuation());
}

#[test]
fn kitty_graphics_routes_only_standard_g_apc() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(KITTY_ONE_PIXEL_RGBA_APC);

    let image = terminal
        .kitty_graphics
        .get_image(41)
        .expect("standard Kitty APC should reach the graphics state");
    assert_eq!((image.width, image.height), (1, 1));
    assert_eq!(image.data, [255, 0, 0, 255]);
}

#[test]
fn kitty_graphics_apc_survives_every_input_batch_boundary() {
    for split_at in 1..KITTY_ONE_PIXEL_RGBA_APC.len() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(&KITTY_ONE_PIXEL_RGBA_APC[..split_at]);
        assert!(
            terminal.kitty_graphics.get_image(41).is_none(),
            "incomplete APC was applied at split {split_at}"
        );
        terminal.process_input(&KITTY_ONE_PIXEL_RGBA_APC[split_at..]);

        assert!(
            terminal.kitty_graphics.get_image(41).is_some(),
            "APC was lost at input split {split_at}"
        );
    }
}

#[test]
fn fragmented_kitty_apc_advances_its_scan_cursor_and_stays_bounded() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b_Gi=52,f=32,s=1,v=1;");
    assert_eq!(
        terminal.pending_apc_scan_from,
        terminal.pending_apc.len().saturating_sub(1)
    );

    for fragment in [
        b"AQ".as_slice(),
        b"ID".as_slice(),
        b"BA".as_slice(),
        b"==".as_slice(),
    ] {
        let old_len = terminal.pending_apc.len();
        terminal.process_input(fragment);
        assert_eq!(terminal.pending_apc.len(), old_len + fragment.len());
        assert_eq!(
            terminal.pending_apc_scan_from,
            terminal.pending_apc.len().saturating_sub(1),
            "unterminated fragments must resume scanning at the previous tail"
        );
    }
    terminal.process_input(b"\x1b\\");
    assert!(terminal.pending_apc.is_empty());
    assert!(terminal.kitty_graphics.get_image(52).is_some());
    std::mem::take(&mut terminal.output_buffer);

    let mut oversized = b"\x1b_Ga=p,i=53,q=0;".to_vec();
    oversized.resize(MAX_PENDING_ESCAPE + 1, b'A');
    terminal.process_input(&oversized);
    assert!(terminal.pending_apc.is_empty());
    assert!(terminal.discarding_oversized_apc);
    let response = std::mem::take(&mut terminal.output_buffer);
    assert!(response.starts_with(b"\x1b_Gi=53;EINVAL:"));
    assert!(response.len() < 256);

    // Discard through ST without allocating the oversized packet, then resume
    // ordinary terminal parsing on the same input batch.
    terminal.process_input(b"\x1b\\Z");
    assert!(!terminal.discarding_oversized_apc);
    assert_eq!(terminal.grid[0][0].character, 'Z');

    // The bytes after ST belong to the normal stream, not the APC. Even when
    // they make the whole read exceed the cap, a packet whose terminator is
    // itself within the cap must be completed and the remainder preserved.
    let mut near_limit = b"\x1b_Gi=55,f=32,s=1,v=1,q=2;".to_vec();
    near_limit.resize(MAX_PENDING_ESCAPE - 2, b'A');
    terminal.process_input(&near_limit);
    terminal.process_input(b"\x1b\\Y");
    assert!(!terminal.discarding_oversized_apc);
    assert!(terminal.pending_apc.is_empty());
    assert_eq!(terminal.grid[0][1].character, 'Y');
}

#[test]
fn malformed_kitty_apc_reports_errors_unless_quiet_suppresses_them() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b_Ga=p,i=54,bad\x1b\\");
    let response = std::mem::take(&mut terminal.output_buffer);
    assert!(response.starts_with(b"\x1b_Gi=54;EINVAL:"));
    assert!(response.len() < 256);

    terminal.process_input(b"\x1b_Ga=p,i=54,bad,q=2\x1b\\");
    assert!(std::mem::take(&mut terminal.output_buffer).is_empty());

    terminal.process_input(b"\x1b_Ga=p,i=54,q=0;\xff\x1b\\");
    let response = std::mem::take(&mut terminal.output_buffer);
    assert!(response.starts_with(b"\x1b_Gi=54;EINVAL:"));
    assert!(response.len() < 256);
}

#[test]
fn ris_resets_graphics_and_parser_state_without_printing_the_final_byte() {
    let mut terminal = TerminalState::new(8, 3);
    terminal.set_max_scrollback(17);
    terminal.kitty_graphics.set_cell_size_pixels(9, 18);
    terminal.process_input(KITTY_ONE_PIXEL_RGBA_APC);
    terminal.process_input(b"\x1b_Ga=p,i=41,C=1\x1b\\");
    terminal.process_input(b"before\x1b[31m\x1b[?25l");
    assert!(terminal.kitty_graphics.get_image(41).is_some());

    terminal.process_input(b"\x1bcZ");

    assert!(terminal.kitty_graphics.get_image(41).is_none());
    assert!(terminal.kitty_graphics.get_placements().is_empty());
    assert_eq!(terminal.grid[0][0].character, 'Z');
    assert_eq!((terminal.cursor_col, terminal.cursor_row), (1, 0));
    assert!(terminal.is_cursor_visible());
    assert_eq!(terminal.current_fg, Color::Default);
    assert_eq!(terminal.max_scrollback(), 17);
    assert_eq!(terminal.kitty_graphics.cell_size_pixels(), (9, 18));
    assert!(terminal.pending_escape.is_empty());
    assert!(terminal.pending_apc.is_empty());
}

#[test]
fn kitty_placements_follow_text_into_and_out_of_scrollback_view() {
    let mut terminal = TerminalState::new(8, 3);
    terminal.process_input(KITTY_ONE_PIXEL_RGBA_APC);
    terminal.process_input(b"\x1b_Ga=p,i=41,c=2,r=2,C=1\x1b\\");
    terminal.process_input(b"\x1b[3;1H\n");

    let placement = &terminal.kitty_graphics.get_placements()[0];
    assert_eq!(placement.y, -1);
    assert_eq!(placement.viewport_row(0), -1);
    assert_eq!(terminal.scrollback_len(), 1);

    terminal.scroll(1);
    let placement = &terminal.kitty_graphics.get_placements()[0];
    assert_eq!(placement.viewport_row(terminal.scroll_offset), 0);
}

#[test]
fn kitty_chunked_display_uses_cursor_at_final_chunk() {
    let mut terminal = TerminalState::new(8, 3);

    terminal.process_input(b"\x1b_Ga=T,i=42,f=32,s=1,v=1,c=2,r=1,m=1;/wAA\x1b\\");
    assert!(terminal.kitty_graphics.get_placements().is_empty());

    terminal.process_input(b"\x1b[2;4H");
    terminal.process_input(b"\x1b_Gm=0;/w==\x1b\\");

    let placement = terminal
        .kitty_graphics
        .get_placements()
        .first()
        .expect("a=T should place the completed image");
    assert_eq!(placement.image_id, 42);
    assert_eq!((placement.x, placement.y), (3, 1));
    assert_eq!((placement.width, placement.height), (2, 1));
}

#[test]
fn kitty_placement_applies_explicit_cursor_policy_and_cell_offsets() {
    let mut terminal = TerminalState::new(10, 6);
    terminal.process_input(b"\x1b_Gf=32,i=50,s=1,v=1;AQIDBA==\x1b\\");
    terminal.process_input(b"\x1b_Ga=p,i=50,X=3,Y=4,c=2,r=3\x1b\\");

    assert_eq!((terminal.cursor_col, terminal.cursor_row), (2, 3));
    let placement = terminal.kitty_graphics.get_placements().last().unwrap();
    assert_eq!((placement.cell_x_offset, placement.cell_y_offset), (3, 4));

    terminal.process_input(b"\x1b[2;2H");
    terminal.process_input(b"\x1b_Ga=p,i=50,c=4,r=2,C=1\x1b\\");
    assert_eq!((terminal.cursor_col, terminal.cursor_row), (1, 1));
}

#[test]
fn text_erase_keeps_graphics_except_for_full_ed2() {
    let mut terminal = TerminalState::new(8, 4);
    terminal.process_input(b"\x1b_Gf=32,i=51,s=1,v=1;AQIDBA==\x1b\\");
    terminal.process_input(b"\x1b_Ga=p,i=51,C=1\x1b\\");

    for erase in [
        b"\x1b[K".as_slice(),
        b"\x1b[1K".as_slice(),
        b"\x1b[2K".as_slice(),
        b"\x1b[J".as_slice(),
        b"\x1b[1J".as_slice(),
    ] {
        terminal.process_input(erase);
        assert_eq!(
            terminal.kitty_graphics.get_placements().len(),
            1,
            "text erase {erase:?} must not clear graphics"
        );
    }

    terminal.process_input(b"\x1b[2J");
    assert!(terminal.kitty_graphics.get_placements().is_empty());
    assert!(terminal.kitty_graphics.get_image(51).is_some());
}

#[test]
fn kitty_graphics_does_not_route_dcs_sos_pm_or_non_g_apc() {
    let body = b"Ga=t,i=41,f=32,s=1,v=1;/wAA/w==";

    for introducer in *b"PX^" {
        let mut terminal = TerminalState::new(8, 2);
        let mut sequence = vec![0x1b, introducer];
        sequence.extend_from_slice(body);
        sequence.extend_from_slice(b"\x1b\\");

        terminal.process_input(&sequence);
        assert!(
            terminal.kitty_graphics.get_image(41).is_none(),
            "non-APC introducer {introducer:#x} was routed as Kitty graphics"
        );
    }

    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b_a=t,i=41,f=32,s=1,v=1;/wAA/w==\x1b\\");
    assert!(terminal.kitty_graphics.get_image(41).is_none());
}

#[test]
fn resize_preserves_full_screen_scroll_region() {
    let mut terminal = TerminalState::new(4, 3);

    terminal.on_resize(4, 6);

    assert_eq!(terminal.scroll_region_top, 0);
    assert_eq!(terminal.scroll_region_bottom, 5);
}

#[test]
fn alt_screen_resize_does_not_leak_background_into_primary_screen() {
    let mut terminal = TerminalState::new(4, 2);
    terminal.process_input(b"main\x1b[?1049h\x1b[44m");

    // Vim and similar full-screen applications resize while a non-default
    // background is active. The visible alternate screen should inherit it,
    // but the saved primary screen must remain independent.
    terminal.on_resize(8, 3);
    assert_eq!(terminal.grid[0][4].background, Color::Blue);
    assert_eq!(terminal.grid[2][0].background, Color::Blue);

    terminal.process_input(b"\x1b[?1049l");

    assert!(!terminal.is_alt_buffer_active());
    assert_eq!(terminal.grid[0][0].character, 'm');
    assert_eq!(terminal.grid[0][4].background, Color::Default);
    assert_eq!(terminal.grid[2][0].background, Color::Default);
}

#[test]
fn application_cursor_mode_is_tracked_independently_of_alt_screen() {
    let mut terminal = TerminalState::new(4, 2);

    assert!(!terminal.is_application_cursor_keys());
    terminal.process_input(b"\x1b[?1049h");
    assert!(terminal.is_alt_buffer_active());
    assert!(!terminal.is_application_cursor_keys());

    terminal.process_input(b"\x1b[?1h");
    assert!(terminal.is_application_cursor_keys());
    terminal.process_input(b"\x1b[?1l");
    assert!(!terminal.is_application_cursor_keys());
}

#[test]
fn osc7_rejects_remote_host_paths_for_local_session_restore() {
    assert_eq!(
        TerminalState::decode_osc7_cwd("file:///home/user/My%20Files"),
        Some("/home/user/My Files".to_string())
    );
    assert_eq!(
        TerminalState::decode_osc7_cwd("file://localhost/tmp"),
        Some("/tmp".to_string())
    );
    assert_eq!(
        TerminalState::decode_osc7_cwd("file://definitely-remote.invalid/etc"),
        None
    );
}

#[test]
fn sgr_mouse_coordinates_are_not_limited_to_one_byte() {
    let mut terminal = TerminalState::new(4, 2);
    terminal.process_input(b"\x1b[?1000h\x1b[?1006h");

    assert_eq!(
        terminal.get_mouse_report(0, 254, 255).as_deref(),
        Some(b"\x1b[<0;255;256M".as_slice())
    );
    assert_eq!(
        terminal.get_mouse_report(0, 255, 256).as_deref(),
        Some(b"\x1b[<0;256;257M".as_slice())
    );
    assert_eq!(
        terminal.get_mouse_release_report(0, 999, 1000).as_deref(),
        Some(b"\x1b[<0;1000;1001m".as_slice())
    );
}

#[test]
fn legacy_mouse_coordinates_clamp_before_narrowing() {
    let mut terminal = TerminalState::new(4, 2);
    terminal.process_input(b"\x1b[?1000h");

    let origin = terminal.get_mouse_report(0, 0, 0).unwrap();
    assert_eq!(origin, b"\x1b[M !!");

    let large = terminal.get_mouse_report(0, 255, usize::MAX).unwrap();
    assert_eq!(&large[..4], b"\x1b[M ");
    assert_eq!(large[4], 255);
    assert_eq!(large[5], 255);

    let larger = terminal.get_mouse_report(0, 511, 256).unwrap();
    assert_eq!(larger[4], large[4]);
    assert_eq!(larger[5], large[5]);
}

#[test]
fn mouse_motion_modes_distinguish_drag_and_all_motion() {
    let mut terminal = TerminalState::new(4, 2);
    terminal.process_input(b"\x1b[?1002h");
    assert!(!terminal.should_report_mouse_motion(false));
    assert!(terminal.should_report_mouse_motion(true));

    terminal.process_input(b"\x1b[?1002l\x1b[?1003h");
    assert!(terminal.should_report_mouse_motion(false));
    assert!(terminal.should_report_mouse_motion(true));
}

#[test]
fn decstbm_zero_bottom_defaults_to_full_screen() {
    let mut terminal = TerminalState::new(4, 4);

    terminal.process_input(b"\x1b[1;0r");

    assert_eq!(terminal.scroll_region_top, 0);
    assert_eq!(terminal.scroll_region_bottom, 3);
}

#[test]
fn codex_resume_style_output_populates_scrollback() {
    let mut terminal = TerminalState::new(8, 3);

    terminal.process_input(b"\x1b[?2026h\x1b[1;0r\x1b[1;1H");
    terminal.process_input(b"line-1\r\nline-2\r\nline-3\r\nline-4\r\nline-5\r\n");
    terminal.process_input(b"\x1b[?2026l");

    assert!(
        terminal.scrollback_len() >= 3,
        "expected resumed TUI output to enter scrollback"
    );

    terminal.scroll(2);
    let visible = terminal.get_visible_cells();
    let text: String = visible[0]
        .iter()
        .map(|cell| cell.character)
        .collect::<String>()
        .trim_end()
        .to_string();

    assert!(
        text.starts_with("line-"),
        "expected scrollback viewport to show historical output, got {text:?}"
    );
}

#[test]
fn synchronized_primary_screen_redraws_do_not_fill_scrollback() {
    let mut terminal = TerminalState::new(24, 4);

    for seconds in 1..=3 {
        terminal.process_input(b"\x1b[?2026h\x1b[1;1H\x1b[2J");
        terminal.process_input(b">_ OpenAI Codex\r\n");
        terminal.process_input(format!("Booting MCP server ({seconds}s)").as_bytes());
        terminal.process_input(b"\x1b[?2026l");
    }

    assert_eq!(
        terminal.scrollback_len(),
        0,
        "primary-screen synchronized redraws should not be recorded as history"
    );
}

#[test]
fn top_margin_scroll_region_pushes_scrolled_lines_to_scrollback() {
    let mut terminal = TerminalState::new(24, 6);

    terminal.process_input(b"\x1b[1;4r\x1b[1;1H");
    terminal.process_input(b"hist-1\r\nhist-2\r\nhist-3\r\nhist-4\r\nhist-5\r\n");
    terminal.process_input(b"\x1b[r\x1b[5;1Hprompt\r\nstatus");

    let history: Vec<String> = terminal
        .scrollback
        .iter()
        .map(|line| {
            line.decompress()
                .iter()
                .map(|cell| cell.character)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();

    assert_eq!(
        history,
        ["hist-1", "hist-2"],
        "expected lines scrolled off a top-anchored region to remain scrollable"
    );

    assert_eq!(terminal.grid[4][0].character, 'p');
    assert_eq!(terminal.grid[5][0].character, 's');
}

#[test]
fn synchronized_primary_screen_entry_preserves_existing_history() {
    let mut terminal = TerminalState::new(24, 4);

    terminal.process_input(b"previous log\r\nshell prompt");
    terminal.process_input(b"\x1b[?2026h\x1b[1;1H\x1b[2J");
    terminal.process_input(b">_ OpenAI Codex\r\nBooting MCP server");
    terminal.process_input(b"\x1b[?2026l");
    terminal.process_input(b"\x1b[?2026h\x1b[1;1H\x1b[2J");
    terminal.process_input(b">_ OpenAI Codex\r\nBooting MCP server");
    terminal.process_input(b"\x1b[?2026l");

    assert_eq!(terminal.scrollback_len(), 2);
    let history: Vec<String> = terminal
        .scrollback
        .iter()
        .map(|line| {
            line.decompress()
                .iter()
                .map(|cell| cell.character)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();

    assert_eq!(history, ["previous log", "shell prompt"]);
}

#[test]
fn synchronized_alt_screen_snapshots_can_be_scrolled() {
    let mut terminal = TerminalState::new(12, 3);

    terminal.process_input(b"\x1b[?1049h");
    assert!(terminal.is_alt_buffer_active());

    terminal.process_input(b"\x1b[?2026h\x1b[1;1H");
    terminal.process_input(b"first page\r\nalpha\r\nomega");
    terminal.process_input(b"\x1b[?2026l");
    terminal.process_input(b"\x1b[?2026h\x1b[1;1H");
    terminal.process_input(b"second page\r\nbeta\r\ndone ");
    terminal.process_input(b"\x1b[?2026l");

    assert!(
        terminal.scrollback_len() >= 6,
        "expected synchronized alt-screen snapshots in scrollback"
    );

    terminal.scroll(3);
    assert!(terminal.scroll_offset > 0);
    let visible = terminal.get_visible_cells();
    let text = visible
        .iter()
        .flat_map(|row| {
            row.iter()
                .map(|cell| cell.character)
                .chain(std::iter::once('\n'))
        })
        .collect::<String>();

    assert!(
        text.contains("first page") || text.contains("second page"),
        "expected archived synchronized screen content, got {text:?}"
    );
}

#[test]
fn linefeed_at_bottom_pushes_to_scrollback_for_full_screen_region() {
    let mut terminal = TerminalState::new(4, 2);
    terminal.grid[0][0].character = 'A';
    terminal.grid[1][0].character = 'B';
    terminal.cursor_row = 1;
    terminal.cursor_col = 0;

    terminal.process_input(b"\n");

    assert_eq!(terminal.scrollback.len(), 1);
    assert_eq!(terminal.scrollback[0].decompress()[0].character, 'A');
    assert_eq!(terminal.grid[0][0].character, 'B');
    assert_eq!(terminal.grid[1][0].character, ' ');
}

fn legacy_reflowed_visible_cells(terminal: &TerminalState) -> Vec<Vec<TerminalCell>> {
    let rows = terminal.grid.rows();
    let cols = terminal.grid.row_len();
    let blank_cell = terminal.create_blank_cell();
    let mut start_idx = terminal
        .scrollback
        .len()
        .saturating_sub(terminal.scroll_offset + rows);
    while start_idx > 0 && terminal.scrollback[start_idx - 1].is_wrapped {
        start_idx -= 1;
    }

    let copied_tail: Vec<ScrollbackLine> = terminal
        .scrollback
        .iter()
        .skip(start_idx)
        .cloned()
        .collect();
    let reflowed = TerminalState::reflow_lines(&copied_tail, cols, &blank_cell);
    let skip = reflowed.len().saturating_sub(terminal.scroll_offset + rows);
    let visible_start = skip + (reflowed.len() - skip).saturating_sub(terminal.scroll_offset);
    let mut result: Vec<Vec<TerminalCell>> = reflowed[visible_start..]
        .iter()
        .map(ScrollbackLine::decompress)
        .collect();
    result.truncate(rows);

    for row in terminal.grid.iter() {
        if result.len() >= rows {
            break;
        }
        result.push(terminal.normalize_line_width(row.to_vec(), cols));
    }
    while result.len() < rows {
        result.push(terminal.blank_line(cols));
    }
    result
}

fn assert_cell_grids_equal(
    actual: &[Vec<TerminalCell>],
    expected: &[Vec<TerminalCell>],
    context: &str,
) {
    assert_eq!(actual.len(), expected.len(), "{context}: row count");
    for (row_index, (actual_row, expected_row)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(
            actual_row.len(),
            expected_row.len(),
            "{context}: column count at row {row_index}"
        );
        for (column_index, (actual_cell, expected_cell)) in
            actual_row.iter().zip(expected_row).enumerate()
        {
            assert_eq!(
                actual_cell.character, expected_cell.character,
                "{context}: character at ({row_index}, {column_index})"
            );
            assert_eq!(
                actual_cell.foreground, expected_cell.foreground,
                "{context}: foreground at ({row_index}, {column_index})"
            );
            assert_eq!(
                actual_cell.background, expected_cell.background,
                "{context}: background at ({row_index}, {column_index})"
            );
            assert_eq!(
                actual_cell.flags, expected_cell.flags,
                "{context}: flags at ({row_index}, {column_index})"
            );
            assert_eq!(
                actual_cell.hyperlink_id, expected_cell.hyperlink_id,
                "{context}: hyperlink at ({row_index}, {column_index})"
            );
        }
    }
}

#[test]
fn streamed_scrollback_reflow_matches_legacy_results_across_offsets_and_resize() {
    fn line(text: &str, cols: usize, wrapped: bool) -> ScrollbackLine {
        let mut cells = vec![TerminalCell::default(); cols];
        for (cell, ch) in cells.iter_mut().zip(text.chars()) {
            cell.character = ch;
        }
        ScrollbackLine::compress(&cells, wrapped)
    }

    let mut terminal = TerminalState::new(5, 3);
    terminal.push_scrollback_compressed(line("abcd", 4, true));

    // Foreground/style-only trailing spaces are intentionally discarded by
    // the established logical-line join rule. Exercise the cached length
    // against that less-obvious behavior.
    let mut styled_tail = line("ef", 4, false).decompress();
    styled_tail[3].foreground = Color::Red;
    styled_tail[3].flags.set_bold(true);
    terminal.push_scrollback_compressed(ScrollbackLine::compress(&styled_tail, false));

    terminal.push_scrollback_compressed(line("ghijklm", 7, false));
    terminal.push_scrollback_compressed(line("", 6, false));
    terminal.push_scrollback_compressed(line("nopqr", 5, true));
    terminal.push_scrollback_compressed(line("stu", 3, false));

    for offset in 1..=terminal.scrollback.len() {
        terminal.scroll_offset = offset;
        let expected = legacy_reflowed_visible_cells(&terminal);
        let actual = terminal.get_visible_cells();
        assert_cell_grids_equal(
            actual.as_ref(),
            &expected,
            &format!("streamed reflow at offset {offset}"),
        );
    }

    // A width change must use the new chunking while preserving the same raw
    // scrollback/search coordinate model.
    terminal.on_resize(6, 4);
    for offset in 1..=terminal.scrollback.len() {
        terminal.scroll_offset = offset;
        let expected = legacy_reflowed_visible_cells(&terminal);
        let actual = terminal.get_visible_cells();
        assert_cell_grids_equal(
            actual.as_ref(),
            &expected,
            &format!("streamed reflow after resize at offset {offset}"),
        );
    }
}

#[test]
fn appending_scrollback_invalidates_a_cached_historical_viewport() {
    fn tagged_line(tag: char) -> ScrollbackLine {
        let mut cells = vec![TerminalCell::default(); 4];
        cells[0].character = tag;
        ScrollbackLine::compress(&cells, false)
    }

    let mut terminal = TerminalState::new(4, 2);
    terminal.push_scrollback_compressed(tagged_line('A'));
    terminal.push_scrollback_compressed(tagged_line('B'));
    terminal.scroll_offset = 1;

    let before = terminal.get_visible_cells();
    assert_eq!(before[0][0].character, 'B');

    // Keep grid_version and scroll_offset unchanged: only explicit scrollback
    // cache invalidation can make the newly appended tail visible here.
    let version = terminal.grid_version;
    terminal.push_scrollback_compressed(tagged_line('C'));
    assert_eq!(terminal.grid_version, version);
    let after = terminal.get_visible_cells();

    assert!(!std::sync::Arc::ptr_eq(&before, &after));
    assert_eq!(after[0][0].character, 'C');
}

#[test]
fn shrinking_scrollback_invalidates_both_historical_view_caches() {
    fn tagged_line(tag: char) -> ScrollbackLine {
        let mut cells = vec![TerminalCell::default(); 4];
        cells[0].character = tag;
        ScrollbackLine::compress(&cells, false)
    }

    let mut terminal = TerminalState::new(4, 2);
    for tag in ['A', 'B', 'C'] {
        terminal.push_scrollback_compressed(tagged_line(tag));
    }
    terminal.scroll_offset = 1;
    let before = terminal.get_visible_cells();
    let _ = terminal.viewport_buffer_mapping_is_exact();
    assert!(terminal.visible_cells_cache.is_some());
    assert!(terminal.viewport_mapping_exact_cache.get().is_some());

    terminal.set_max_scrollback(2);

    assert!(terminal.visible_cells_cache.is_none());
    assert!(terminal.viewport_mapping_exact_cache.get().is_none());
    let after = terminal.get_visible_cells();
    assert!(!std::sync::Arc::ptr_eq(&before, &after));
    assert_eq!(terminal.scrollback_len(), 2);
    assert_eq!(after[0][0].character, 'C');
}

#[test]
fn visible_cells_keep_rectangular_shape_after_resize_with_scrollback() {
    let mut terminal = TerminalState::new(4, 2);
    terminal.grid.get_mut(0, 0).character = 'A';
    terminal.grid.get_mut(1, 0).character = 'B';
    terminal.cursor_row = 1;

    terminal.process_input(b"\n");
    terminal.on_resize(5, 2);
    terminal.scroll(1);

    let visible = terminal.get_visible_cells();

    assert_eq!(visible.len(), 2);
    assert!(visible.iter().all(|row| row.len() == 5));
    assert_eq!(visible[0][0].character, 'A');
    assert_eq!(visible[0][4].character, ' ');
}

#[test]
fn resize_invalidates_an_already_populated_visible_cells_cache() {
    let mut terminal = TerminalState::new(4, 2);
    terminal.process_input(b"abcd");
    let cached = terminal.get_visible_cells();
    assert_eq!(cached.len(), 2);
    assert!(cached.iter().all(|row| row.len() == 4));
    let version_before = terminal.get_grid_version();

    terminal.on_resize(6, 3);
    let resized = terminal.get_visible_cells();

    assert!(terminal.get_grid_version() > version_before);
    assert_eq!(resized.len(), 3);
    assert!(resized.iter().all(|row| row.len() == 6));
    assert_eq!(resized[0][0].character, 'a');
}

#[test]
fn viewport_mapping_exactness_is_recomputed_when_height_changes() {
    let mut terminal = TerminalState::new(4, 2);
    let line = vec![TerminalCell::default(); 4];
    for index in 0..5 {
        terminal
            .scrollback
            .push_back(ScrollbackLine::compress(&line, index == 0));
    }
    terminal.total_lines_scrolled = 5;
    terminal.scroll_offset = 1;
    assert!(terminal.viewport_buffer_mapping_is_exact());

    terminal.on_resize(4, 4);
    terminal.scroll_offset = 1;
    assert!(!terminal.viewport_buffer_mapping_is_exact());
}

#[test]
fn cursor_is_hidden_while_viewing_scrollback() {
    let mut terminal = TerminalState::new(4, 2);
    terminal.grid.get_mut(0, 0).character = 'A';
    terminal.grid.get_mut(1, 0).character = 'B';
    terminal.cursor_row = 1;

    terminal.process_input(b"\n");

    assert!(terminal.is_cursor_visible());

    terminal.scroll(1);

    assert!(!terminal.is_cursor_visible());
}

#[test]
fn scroll_to_bottom_restores_live_cursor_visibility() {
    let mut terminal = TerminalState::new(4, 2);
    terminal.grid.get_mut(0, 0).character = 'A';
    terminal.grid.get_mut(1, 0).character = 'B';
    terminal.cursor_row = 1;

    terminal.process_input(b"\n");
    terminal.scroll(1);
    terminal.scroll_to_bottom();

    assert_eq!(terminal.scroll_offset, 0);
    assert!(terminal.is_cursor_visible());
}

#[test]
fn sgr_39_and_49_restore_default_colors() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[36;44mA\x1b[39;49mB");

    let first = &terminal.grid[0][0];
    let second = &terminal.grid[0][1];

    assert_eq!(first.foreground, Color::Cyan);
    assert_eq!(first.background, Color::Blue);
    assert_eq!(second.foreground, Color::Default);
    assert_eq!(second.background, Color::Default);
}

#[test]
fn cleared_cells_keep_active_background() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[44mAB\x1b[1;1H\x1b[K");

    assert_eq!(terminal.grid[0][0].background, Color::Blue);
    assert_eq!(terminal.grid[0][1].background, Color::Blue);
}

#[test]
fn empty_sgr_sequence_resets_attributes() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[7;36;44mA\x1b[mB");

    let first = &terminal.grid[0][0];
    let second = &terminal.grid[0][1];

    assert!(first.flags.inverse());
    assert_eq!(first.foreground, Color::Cyan);
    assert_eq!(first.background, Color::Blue);

    assert!(!second.flags.inverse());
    assert_eq!(second.foreground, Color::Default);
    assert_eq!(second.background, Color::Default);
}

#[test]
fn split_truecolor_sequence_does_not_leak_text() {
    let mut terminal = TerminalState::new(32, 2);

    terminal.process_input(b"\x1b[38");
    terminal.process_input(b";2;81;175;239msrc");

    assert_eq!(terminal.grid[0][0].character, 's');
    assert_eq!(terminal.grid[0][1].character, 'r');
    assert_eq!(terminal.grid[0][2].character, 'c');
    assert_eq!(terminal.grid[0][0].foreground, Color::Rgb(81, 175, 239));
}

#[test]
fn sgr_underline_with_semicolon_keeps_following_attr() {
    // `4;1` 是两个独立 SGR(下划线 + 粗体),分号不得被吞。
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b[4;1mA");
    let cell = &terminal.grid[0][0];
    assert_eq!(cell.flags.underline(), UnderlineStyle::Single);
    assert!(cell.flags.bold(), "粗体不应被下划线吞掉");
}

#[test]
fn sgr_underline_colon_substyle_is_extended() {
    // `4:3` 冒号子参数 = curly 下划线,且不应附带粗体。
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b[4:3mA");
    let cell = &terminal.grid[0][0];
    assert_eq!(cell.flags.underline(), UnderlineStyle::Curly);
    assert!(!cell.flags.bold());
}

#[test]
fn csi_empty_leading_param_defaults() {
    // `\x1b[;3H` 应定位到第 1 行第 3 列(空字段默认 1)。
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b[;3HX");
    assert_eq!(terminal.grid[0][2].character, 'X');
}

#[test]
fn ed_clear_screen_does_not_move_cursor() {
    // ED(`\x1b[2J`)不得移动光标。
    let mut terminal = TerminalState::new(8, 3);
    terminal.process_input(b"\x1b[2;3H"); // row2,col3
    terminal.process_input(b"\x1b[2J");
    terminal.process_input(b"X");
    assert_eq!(terminal.grid[1][2].character, 'X');
}

#[test]
fn vpa_and_hpa_position_cursor() {
    let mut terminal = TerminalState::new(8, 4);
    terminal.process_input(b"\x1b[3d"); // VPA -> row 3
    terminal.process_input(b"\x1b[5`"); // HPA -> col 5
    terminal.process_input(b"Z");
    assert_eq!(terminal.grid[2][4].character, 'Z');
}

#[test]
fn cuu_does_not_scroll_at_top_margin() {
    // 在滚动区顶部执行 CUU 不应滚动内容。
    let mut terminal = TerminalState::new(8, 4);
    terminal.process_input(b"\x1b[2;4r"); // 滚动区 2..4
    terminal.process_input(b"\x1b[2;1HABC"); // 在区顶写入
    terminal.process_input(b"\x1b[2;1H\x1b[A"); // 回到区顶再 CUU
                                                // 内容应原地保留,不被向下滚动
    assert_eq!(terminal.grid[1][0].character, 'A');
    assert_eq!(terminal.grid[1][1].character, 'B');
}

#[test]
fn trailing_escape_is_buffered_until_next_chunk() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b");
    terminal.process_input(b"[31mX");

    assert_eq!(terminal.grid[0][0].character, 'X');
    assert_eq!(terminal.grid[0][0].foreground, Color::Red);
}

#[test]
fn dec_special_graphics_charset_maps_line_drawing() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b(0qx\x0fA");

    assert_eq!(terminal.grid[0][0].character, '─');
    assert_eq!(terminal.grid[0][1].character, '│');
    assert_eq!(terminal.grid[0][2].character, 'A');
}

#[test]
fn decscusr_with_intermediate_space_does_not_leak_text() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[0 qX");

    assert_eq!(terminal.grid[0][0].character, 'X');
}

#[test]
fn private_csi_u_sequence_does_not_restore_cursor_or_leak() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"AB");
    terminal.process_input(b"\x1b[?4uC");

    assert_eq!(terminal.grid[0][0].character, 'A');
    assert_eq!(terminal.grid[0][1].character, 'B');
    assert_eq!(terminal.grid[0][2].character, 'C');
}

#[test]
fn csi_with_gt_prefix_is_consumed_without_printing_parameters() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[>4;1mZ");

    assert_eq!(terminal.grid[0][0].character, 'Z');
    assert_eq!(terminal.grid[0][1].character, ' ');
}

#[test]
fn dcs_sequence_is_consumed_without_leaking_text() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1bP$q q\x1b\\X");

    assert_eq!(terminal.grid[0][0].character, 'X');
    assert_eq!(terminal.grid[0][1].character, ' ');
}

#[test]
fn primary_and_secondary_device_attributes_are_reported() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[c\x1b[>c");

    assert_eq!(
        String::from_utf8(terminal.get_output()).unwrap(),
        "\x1b[?65;1;9c\x1b[>1;7802;0c"
    );
}

#[test]
fn xtversion_query_is_reported() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[>0q");

    assert_eq!(
        String::from_utf8(terminal.get_output()).unwrap(),
        "\x1bP>|VTE(7802)\x1b\\"
    );
}

#[test]
fn double_click_selects_full_url() {
    let mut terminal = TerminalState::new(64, 2);

    terminal.process_input(b"see https://example.com/path?a=1&b=2 now");
    terminal.select_word_at(0, 12);

    assert_eq!(
        terminal.copy_selection().as_deref(),
        Some("https://example.com/path?a=1&b=2")
    );
}

#[test]
fn double_click_selects_file_path_with_line_number() {
    let mut terminal = TerminalState::new(64, 2);

    terminal.process_input(b"open src/main.rs:1480 please");
    terminal.select_word_at(0, 8);

    assert_eq!(
        terminal.copy_selection().as_deref(),
        Some("src/main.rs:1480")
    );
}

#[test]
fn double_click_excludes_wrapping_punctuation() {
    let mut terminal = TerminalState::new(64, 2);

    terminal.process_input(b"(https://example.com/path), next");
    terminal.select_word_at(0, 10);

    assert_eq!(
        terminal.copy_selection().as_deref(),
        Some("https://example.com/path")
    );
}

#[test]
fn double_click_selects_extended_token_across_soft_wraps() {
    let mut terminal = TerminalState::new(12, 6);

    terminal.process_input(b"path=\"/home/yj/projects/jwm/submodules/dioxus_bar/target\"");
    terminal.select_word_at(2, 4);

    assert_eq!(
        terminal.copy_selection().as_deref(),
        Some("/home/yj/projects/jwm/submodules/dioxus_bar/target")
    );

    let selection = terminal.selection.expect("selection should exist");
    let (start, end) = if selection.anchor <= selection.active {
        (selection.anchor, selection.active)
    } else {
        (selection.active, selection.anchor)
    };
    assert!(end.0 > start.0, "test token must span visual rows");
    let viewport_base = terminal.scrollback_len();
    for abs_row in start.0..=end.0 {
        let viewport_row = abs_row - viewport_base;
        assert!(
            terminal.row_selection_cols(viewport_row).is_some(),
            "every selected visual row must expose highlight columns"
        );
    }
}

#[test]
fn alternate_screen_drops_primary_screen_selection() {
    let mut terminal = TerminalState::new(16, 2);
    terminal.process_input(b"selected");
    terminal.select_word_at(0, 2);
    assert!(terminal.selection.is_some());

    terminal.process_input(b"\x1b[?1049h");

    assert!(terminal.selection.is_none());
}

#[test]
fn scrolling_preserves_buffer_anchored_selection() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"one\r\ntwo\r\nthree");
    terminal.select_word_at(0, 1);
    let selection = terminal.selection.expect("selection should exist");
    assert_eq!(terminal.copy_selection().as_deref(), Some("two"));

    terminal.scroll(1);

    assert_eq!(terminal.selection, Some(selection));
    assert_eq!(terminal.copy_selection().as_deref(), Some("two"));
    assert_eq!(terminal.row_selection_cols(1), Some((0, 2)));

    terminal.scroll(-1);

    assert_eq!(terminal.selection, Some(selection));
    assert_eq!(terminal.row_selection_cols(0), Some((0, 2)));
}

#[test]
fn selection_can_extend_after_viewport_scroll() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"one\r\ntwo\r\nthree");
    terminal.start_selection((1, 4));
    assert_eq!(terminal.selection.unwrap().anchor, (2, 4));

    // Simulate holding the primary button while the wheel exposes older rows,
    // then moving the active endpoint over the newly visible top row.
    terminal.scroll(1);
    terminal.update_selection((0, 0));

    let selection = terminal
        .selection
        .expect("selection should survive scrolling");
    assert_eq!(selection.anchor, (2, 4));
    assert_eq!(selection.active, (0, 0));
    assert_eq!(terminal.row_selection_cols(0), Some((0, usize::MAX)));
    assert_eq!(terminal.row_selection_cols(1), Some((0, usize::MAX)));
    let copied = terminal.copy_selection();

    terminal.scroll(-1);

    assert_eq!(terminal.selection, Some(selection));
    assert_eq!(terminal.copy_selection(), copied);
    assert_eq!(terminal.row_selection_cols(0), Some((0, usize::MAX)));
    assert_eq!(terminal.row_selection_cols(1), Some((0, 4)));
}

#[test]
fn triple_click_selects_visual_line_without_padding() {
    let mut terminal = TerminalState::new(16, 2);

    terminal.process_input(b"hello line");
    terminal.select_line_at(0);

    assert_eq!(terminal.copy_selection().as_deref(), Some("hello line"));
}

#[test]
fn bracketed_paste_mode_is_tracked() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[?2004h");
    assert!(terminal.is_bracketed_paste_enabled());

    terminal.process_input(b"\x1b[?2004l");
    assert!(!terminal.is_bracketed_paste_enabled());
}

#[test]
fn kitty_keyboard_flags_can_be_set_queried_and_popped() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[=1u");
    assert_eq!(terminal.keyboard_enhancement_flags(), 1);

    terminal.process_input(b"\x1b[?u");
    assert_eq!(
        String::from_utf8(terminal.get_output()).unwrap(),
        "\x1b[?1u"
    );

    terminal.process_input(b"\x1b[>5u");
    assert_eq!(terminal.keyboard_enhancement_flags(), 5);

    terminal.process_input(b"\x1b[<u");
    assert_eq!(terminal.keyboard_enhancement_flags(), 1);
}

#[test]
fn xtmodkeys_and_xtfmtkeys_state_is_tracked() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[>4;2m\x1b[>4;1f");

    assert_eq!(terminal.xterm_modify_other_keys(), 2);
    assert_eq!(terminal.xterm_format_other_keys(), 1);
}

#[test]
fn vte_report_all_keys_mode_is_tracked() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[?2031h");
    assert!(terminal.is_report_all_keys_enabled());

    terminal.process_input(b"\x1b[?2031l");
    assert!(!terminal.is_report_all_keys_enabled());
}

fn paste_token_from_event(event: &[u8]) -> String {
    use base64::Engine as _;

    let event = std::str::from_utf8(event).expect("paste event must be UTF-8");
    let encoded = event
        .split_once(":pw=")
        .and_then(|(_, rest)| rest.split('\x1b').next())
        .expect("paste event must include pw metadata");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("pw must be valid base64");
    String::from_utf8(bytes).expect("pw must be UTF-8")
}

fn osc_5522_mime_read(mime: &str, token: &str, name: &str) -> Vec<u8> {
    use base64::Engine as _;

    let engine = base64::engine::general_purpose::STANDARD;
    let encoded_mime = engine.encode(mime.as_bytes());
    let encoded_token = engine.encode(token.as_bytes());
    let encoded_name = engine.encode(name.as_bytes());
    format!("\x1b]5522;type=read:pw={encoded_token}:name={encoded_name};{encoded_mime}\x1b\\")
        .into_bytes()
}

#[test]
fn osc_5522_mime_list_request_without_user_paste_is_denied() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b]5522;type=read;Lg==\x1b\\");

    assert!(terminal.take_clipboard_read_requests().is_empty());
    assert!(terminal.pending_paste_grant.is_none());
    assert_eq!(
        String::from_utf8(terminal.get_output()).unwrap(),
        "\x1b]5522;type=read:status=EPERM\x1b\\"
    );
}

#[test]
fn osc_5522_data_read_without_user_paste_is_denied() {
    let mut terminal = TerminalState::new(8, 2);
    let request = osc_5522_mime_read("text/plain", "guessed-token", "Paste event");

    terminal.process_input(&request);

    assert!(terminal.take_clipboard_read_requests().is_empty());
    assert_eq!(
        String::from_utf8(terminal.get_output()).unwrap(),
        "\x1b]5522;type=read:status=EPERM\x1b\\"
    );
}

#[test]
fn osc_5522_paste_grant_is_mode_bound_and_single_use() {
    let mut terminal = TerminalState::new(8, 2);
    assert!(terminal
        .build_paste_event(&["text/plain".to_string()])
        .is_empty());

    terminal.process_input(b"\x1b[?5522h");
    let event = terminal.build_paste_event(&["text/plain".to_string()]);
    let event_text = String::from_utf8(event.clone()).unwrap();
    assert!(event_text.contains(":pw="));
    assert!(!event_text.contains(":password="));
    let token = paste_token_from_event(&event);
    let request = osc_5522_mime_read("text/plain", &token, "Paste event");

    terminal.process_input(&request);
    assert_eq!(
        terminal.take_clipboard_read_requests(),
        vec![ClipboardReadRequest {
            kind: ClipboardReadKind::MimeData("text/plain".to_string()),
        }]
    );

    terminal.process_input(&request);
    assert!(terminal.take_clipboard_read_requests().is_empty());
    assert_eq!(
        String::from_utf8(terminal.get_output()).unwrap(),
        "\x1b]5522;type=read:status=EPERM\x1b\\"
    );
}

#[test]
fn osc_5522_paste_grant_rejects_wrong_name_and_unoffered_mime() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b[?5522h");
    let event = terminal.build_paste_event(&["text/plain".to_string()]);
    let token = paste_token_from_event(&event);

    terminal.process_input(&osc_5522_mime_read("text/plain", &token, "Other app"));
    assert!(terminal.take_clipboard_read_requests().is_empty());
    assert!(String::from_utf8(terminal.get_output())
        .unwrap()
        .contains("status=EPERM"));

    // A failed credential check does not reveal or consume the grant. Once
    // authenticated, however, even an invalid MIME consumes the one-time token.
    terminal.process_input(&osc_5522_mime_read(
        "application/octet-stream",
        &token,
        "Paste event",
    ));
    assert!(terminal.take_clipboard_read_requests().is_empty());
    assert!(terminal.pending_paste_grant.is_none());

    terminal.process_input(&osc_5522_mime_read("text/plain", &token, "Paste event"));
    assert!(terminal.take_clipboard_read_requests().is_empty());
}

#[test]
fn osc_5522_paste_grant_expires_and_is_revoked_with_mode() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b[?5522h");
    let event = terminal.build_paste_event(&["text/plain".to_string()]);
    let token = paste_token_from_event(&event);
    terminal.pending_paste_grant.as_mut().unwrap().expires_at =
        std::time::Instant::now() - std::time::Duration::from_millis(1);

    terminal.process_input(&osc_5522_mime_read("text/plain", &token, "Paste event"));
    assert!(terminal.take_clipboard_read_requests().is_empty());
    assert!(terminal.pending_paste_grant.is_none());

    let event = terminal.build_paste_event(&["text/plain".to_string()]);
    let token = paste_token_from_event(&event);
    terminal.process_input(b"\x1b[?5522l");
    terminal.process_input(&osc_5522_mime_read("text/plain", &token, "Paste event"));
    assert!(terminal.take_clipboard_read_requests().is_empty());
    assert!(terminal.pending_paste_grant.is_none());
}

#[test]
fn osc_5522_new_user_paste_invalidates_the_previous_token() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b[?5522h");
    let old_event = terminal.build_paste_event(&["text/plain".to_string()]);
    let old_token = paste_token_from_event(&old_event);
    let new_event = terminal.build_paste_event(&["image/png".to_string()]);
    let new_token = paste_token_from_event(&new_event);
    assert_ne!(old_token, new_token);

    terminal.process_input(&osc_5522_mime_read("text/plain", &old_token, "Paste event"));
    assert!(terminal.take_clipboard_read_requests().is_empty());

    terminal.process_input(&osc_5522_mime_read("image/png", &new_token, "Paste event"));
    assert_eq!(
        terminal.take_clipboard_read_requests(),
        vec![ClipboardReadRequest {
            kind: ClipboardReadKind::MimeData("image/png".to_string()),
        }]
    );
}

#[test]
fn decrqm_reports_5522_support() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b[?5522$p");

    assert_eq!(
        String::from_utf8(terminal.get_output()).unwrap(),
        "\x1b[?5522;2$y"
    );
}

#[test]
fn combining_mark_composes_onto_previous_cell() {
    let mut terminal = TerminalState::new(8, 2);

    // 'e' followed by U+0301 (combining acute) should compose to 'é'.
    terminal.process_input("e\u{0301}".as_bytes());

    assert_eq!(terminal.grid[0][0].character, 'é');
    // The mark consumes no column; cursor stays just past the base glyph.
    assert_eq!(terminal.cursor_col, 1);
    // The second column is untouched.
    assert_eq!(terminal.grid[0][1].character, ' ');
}

#[test]
fn combining_mark_at_line_start_is_dropped() {
    let mut terminal = TerminalState::new(8, 2);

    // A combining mark with no base character is ignored.
    terminal.process_input("\u{0301}".as_bytes());

    assert_eq!(terminal.grid[0][0].character, ' ');
    assert_eq!(terminal.cursor_col, 0);
}

#[test]
fn pending_wrap_defers_line_break_until_next_char() {
    let mut terminal = TerminalState::new(3, 3);

    // Fill the row exactly; cursor latches at the last column (no wrap yet).
    terminal.process_input(b"abc");
    assert_eq!(terminal.cursor_row, 0);
    assert_eq!(terminal.cursor_col, 2);
    assert_eq!(terminal.grid[0][2].character, 'c');

    // The next printable char triggers the deferred wrap.
    terminal.process_input(b"d");
    assert_eq!(terminal.cursor_row, 1);
    assert_eq!(terminal.cursor_col, 1);
    assert_eq!(terminal.grid[1][0].character, 'd');
}

#[test]
fn carriage_return_cancels_pending_wrap() {
    let mut terminal = TerminalState::new(3, 3);

    terminal.process_input(b"abc");
    // CR cancels the latched wrap; the next char overwrites column 0.
    terminal.process_input(b"\rd");

    assert_eq!(terminal.cursor_row, 0);
    assert_eq!(terminal.cursor_col, 1);
    assert_eq!(terminal.grid[0][0].character, 'd');
}

#[test]
fn osc_133_a_records_command_mark_at_cursor_row() {
    let mut terminal = TerminalState::new(8, 4);

    terminal.process_input(b"hello\n");
    terminal.process_input(b"\x1b]133;A\x07");

    assert_eq!(terminal.command_marks.len(), 1);
    let mark = terminal.command_marks[0];
    assert_eq!(mark.exit_code, None);
    // Cursor is on row 1 (after the LF) so line_id == 1.
    assert_eq!(mark.line_id, 1);
}

#[test]
fn osc_133_d_attaches_exit_code_to_last_mark() {
    let mut terminal = TerminalState::new(8, 4);

    terminal.process_input(b"\x1b]133;A\x07");
    terminal.process_input(b"\x1b]133;D;42\x07");

    assert_eq!(terminal.command_marks.len(), 1);
    assert_eq!(terminal.command_marks[0].exit_code, Some(42));
}

#[test]
fn osc_133_d_without_exit_code_leaves_none() {
    let mut terminal = TerminalState::new(8, 4);

    terminal.process_input(b"\x1b]133;A\x07");
    terminal.process_input(b"\x1b]133;D\x07");

    assert_eq!(terminal.command_marks.len(), 1);
    assert_eq!(terminal.command_marks[0].exit_code, None);
}

#[test]
fn osc_133_records_full_lifecycle_metadata_and_completed_output() {
    let mut terminal = TerminalState::new(16, 5);

    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B;jsh_id=exec-7\x07");
    assert!(terminal.shell_is_prompt_ready());
    terminal.process_input(b"echo hi\r\n");
    terminal.process_input(b"\x1b]133;C;jsh_id=exec-7;cmdline_url=echo%20hi;cwd=%2Ftmp\x07");
    assert!(!terminal.shell_is_prompt_ready());
    terminal.process_input(b"hello\r\n");
    terminal.process_input(b"\x1b]133;D;0;jsh_id=exec-7;duration_ms=12\x07");

    let records = terminal.command_records();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.id, "exec-7");
    assert_eq!(record.command.as_deref(), Some("echo hi"));
    assert!(record.command_exact);
    assert!(!record.command_truncated);
    assert_eq!(record.cwd.as_deref(), Some("/tmp"));
    assert_eq!(record.exit_code, Some(0));
    assert_eq!(record.duration_ms, Some(12));
    assert_eq!(record.state, CommandState::Complete);
    assert!(record.complete);
    assert_eq!(record.prompt_start.column, 0);
    assert_eq!(record.command_start.expect("B anchor").column, 2);
    assert!(record.output_start.is_some());
    assert_eq!(record.output_end, record.end);

    let output = terminal
        .command_output_text("exec-7", 1024)
        .expect("retained command output");
    assert_eq!(output.text, "hello\n");
    assert!(!output.truncated);
    assert_eq!(output.total_bytes, output.text.len());

    let completed = terminal.take_completed_command_outputs();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].id, "exec-7");
    assert_eq!(completed[0].output, "hello\n");
    assert!(completed[0].output_available);
    assert!(terminal.take_completed_command_outputs().is_empty());
}

#[test]
fn agent_generation_is_local_one_shot_and_requires_a_fresh_empty_prompt() {
    let mut terminal = TerminalState::new(24, 5);
    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    terminal
        .arm_agent_execution(41, "ls -la")
        .expect("fresh empty prompt can be armed");
    terminal.process_input(b"ls -la\r\n\x1b]133;C;cmdline_url=ls%20-la\x07ok\r\n");
    terminal.process_input(b"\x1b]133;D;0\x07");

    let completed = terminal.take_completed_command_outputs();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].command.as_deref(), Some("ls -la"));
    assert_eq!(completed[0].agent_generation, Some(41));

    // A later identical command does not inherit the consumed generation.
    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07ls -la\r\n");
    terminal.process_input(b"\x1b]133;C;cmdline_url=ls%20-la\x07ok\r\n\x1b]133;D;0\x07");
    assert_eq!(
        terminal.take_completed_command_outputs()[0].agent_generation,
        None
    );
}

#[test]
fn agent_arm_rejects_input_before_pty_echo() {
    let mut terminal = TerminalState::new(24, 5);
    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    terminal.note_user_input(b"already queued");

    assert!(terminal.arm_agent_execution(1, "echo safe").is_err());
}

#[test]
fn agent_arm_fails_closed_when_the_prompt_anchor_is_unavailable() {
    let mut terminal = TerminalState::new(24, 5);
    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    terminal
        .command_records
        .back_mut()
        .expect("editing record")
        .command_start = None;

    assert!(terminal.arm_agent_execution(1, "echo safe").is_err());
}

#[test]
fn osc_133_truncated_command_is_never_reconstructed_as_exact() {
    let mut terminal = TerminalState::new(24, 4);
    terminal.process_input(b"\x1b]133;A\x07> \x1b]133;B\x07displayed editor text\r\n");
    terminal.process_input(
        b"\x1b]133;C;id=large;cmdline_url=unsafe-prefix;cmd_truncated=1;cwd_url=%2Ftmp\x07",
    );
    terminal.process_input(b"output\x1b]133;D;0;id=large\x07");

    let record = terminal.command_record("large").expect("semantic record");
    assert!(record.command.is_none());
    assert!(!record.command_exact);
    assert!(record.command_truncated);
    assert_eq!(record.cwd.as_deref(), Some("/tmp"));
}

#[test]
fn osc_133_invalid_utf8_command_is_rejected_instead_of_accepting_a_prefix() {
    let mut terminal = TerminalState::new(24, 4);
    terminal.process_input(b"\x1b]133;A\x07> \x1b]133;B\x07shown command\r\n");
    terminal.process_input(b"\x1b]133;C;id=invalid-utf8;cmdline_url=echo%20safe%FFignored\x07");

    let record = terminal
        .command_record("invalid-utf8")
        .expect("semantic record");
    assert!(record.command.is_none());
    assert!(!record.command_exact);
    assert!(!record.command_truncated);
}

#[test]
fn osc_133_oversized_command_is_retained_only_as_truncated_metadata() {
    let mut terminal = TerminalState::new(24, 4);
    let oversized = "x".repeat(MAX_OSC_133_COMMAND_BYTES + 1);
    let lifecycle = format!(
        "\x1b]133;A\x07> \x1b]133;B\x07shown command\r\n\x1b]133;C;id=oversized;cmdline_url={oversized}\x07"
    );
    terminal.process_input(lifecycle.as_bytes());

    let record = terminal
        .command_record("oversized")
        .expect("semantic record");
    assert!(record.command.is_none());
    assert!(!record.command_exact);
    assert!(record.command_truncated);
}

#[test]
fn osc_133_invalid_or_oversized_ids_keep_the_terminal_local_identity() {
    let mut invalid = TerminalState::new(24, 4);
    invalid.process_input(b"\x1b]133;A\x07\x1b]133;C;id=prefix%FFsuffix\x07");
    let invalid_record = invalid.command_records().back().expect("semantic record");
    assert!(invalid_record.id.starts_with("local:"));
    assert!(invalid.command_record("prefix").is_none());

    let mut oversized = TerminalState::new(24, 4);
    let id = "i".repeat(MAX_OSC_133_ID_BYTES + 1);
    let lifecycle = format!("\x1b]133;A\x07\x1b]133;C;id={id}\x07");
    oversized.process_input(lifecycle.as_bytes());
    assert!(oversized
        .command_records()
        .back()
        .expect("semantic record")
        .id
        .starts_with("local:"));
}

#[test]
fn osc_133_decodes_kitty_command_and_percent_encoded_jsh_id() {
    let mut terminal = TerminalState::new(24, 4);
    terminal.process_input(b"\x1b]133;A\x07> \x1b]133;B\x07");
    terminal.process_input(b"shown command\r\n");
    terminal.process_input(b"\x1b]133;C;jsh_id=jsh%3A42;cmdline_url=printf%20%27a%3Bb%2Bc%27\x07");
    terminal.process_input(b"a;b+c\x1b]133;D;exit_status=3;jsh_id=jsh%3A42\x07");

    let record = terminal.command_record("jsh:42").expect("decoded id");
    assert_eq!(record.command.as_deref(), Some("printf 'a;b+c'"));
    assert_eq!(record.exit_code, Some(3));
    assert_eq!(
        terminal
            .command_output_text("jsh:42", 1024)
            .expect("output")
            .text,
        "a;b+c"
    );
}

#[test]
fn command_output_joins_soft_wraps_and_skips_wide_continuations() {
    let mut terminal = TerminalState::new(4, 4);
    terminal.process_input(b"\x1b]133;A\x07\x1b]133;C;jsh_id=wide\x07");
    terminal.process_input("ab界c".as_bytes());
    terminal.process_input(b"\x1b]133;D;0;jsh_id=wide\x07");

    let output = terminal
        .command_output_text("wide", 1024)
        .expect("wide output");
    assert_eq!(output.text, "ab界c");
    assert_eq!(output.total_bytes, "ab界c".len());
}

#[test]
fn bounded_command_output_keeps_utf8_safe_head_and_tail() {
    let mut terminal = TerminalState::new(32, 3);
    terminal.process_input(b"\x1b]133;A\x07\x1b]133;C;jsh_id=bounded\x07");
    terminal.process_input("甲乙丙丁戊己".as_bytes());
    terminal.process_input(b"\x1b]133;D;1;jsh_id=bounded\x07");

    let output = terminal
        .command_output_text("bounded", 12)
        .expect("bounded output");
    assert_eq!(output.text, "甲乙戊己");
    assert!(output.truncated);
    assert_eq!(output.total_bytes, "甲乙丙丁戊己".len());
    assert!(output.text.len() <= 12);
}

#[test]
fn anchor_and_absolute_range_apis_preserve_soft_wrap_semantics() {
    let mut terminal = TerminalState::new(4, 3);
    terminal.process_input(b"abcde");

    let absolute = terminal
        .extract_absolute_text_range((0, 0), (1, 1), 1024)
        .expect("absolute range");
    assert_eq!(absolute.text, "abcde");

    let start = terminal.absolute_to_buffer_anchor((0, 0)).unwrap();
    let end = terminal.absolute_to_buffer_anchor((1, 1)).unwrap();
    assert_eq!(terminal.buffer_anchor_to_absolute(start), Some((0, 0)));
    assert_eq!(terminal.buffer_anchor_to_absolute(end), Some((1, 1)));
    assert_eq!(
        terminal
            .extract_text_range(start, end, 1024)
            .expect("line-id anchors")
            .text,
        "abcde"
    );
    assert_eq!(
        terminal
            .extract_text_by_line_ids(start.line_id, end.line_id, 1024)
            .expect("inclusive line-id range")
            .text,
        "abcde"
    );
}

#[test]
fn command_record_survives_output_eviction_but_anchors_report_unavailable() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.set_max_scrollback(1);
    terminal.process_input(b"\x1b]133;A\x07\x1b]133;C;jsh_id=evicted\x07");
    terminal.process_input(b"one\r\ntwo\r\nthree\r\nfour\r\n");
    terminal.process_input(b"\x1b]133;D;0;jsh_id=evicted\x07");

    assert!(terminal.command_record("evicted").is_some());
    assert!(terminal.command_output_text("evicted", 1024).is_none());
    assert!(!terminal.scroll_to_command("evicted"));
    let completed = terminal.take_completed_command_outputs();
    assert_eq!(completed.len(), 1);
    assert!(!completed[0].output_available);
}

#[test]
fn completed_snapshot_keeps_output_after_later_scrollback_eviction() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.set_max_scrollback(1);
    terminal.process_input(b"\x1b]133;A\x07\x1b]133;C;jsh_id=kept\x07");
    terminal.process_input(b"kept\r\n");
    terminal.process_input(b"\x1b]133;D;0;jsh_id=kept\x07");
    assert_eq!(
        terminal
            .command_output_text("kept", 1024)
            .expect("captured at D")
            .text,
        "kept\n"
    );

    terminal.process_input(b"later1\r\nlater2\r\nlater3\r\n");
    let prompt = terminal.command_record("kept").unwrap().prompt_start;
    assert!(terminal.buffer_anchor_to_absolute(prompt).is_none());
    assert_eq!(
        terminal
            .command_output_text("kept", 1024)
            .expect("snapshot survives eviction")
            .text,
        "kept\n"
    );
}

#[test]
fn captured_output_cache_evicts_oldest_payloads_at_session_cap() {
    let mut terminal = TerminalState::new(8, 2);
    for sequence in 0..65 {
        let lifecycle = format!(
            "\x1b]133;A\x07\x1b]133;C;jsh_id=cache-{sequence}\x07\x1b]133;D;0;jsh_id=cache-{sequence}\x07"
        );
        terminal.process_input(lifecycle.as_bytes());
        let index = terminal.command_records().len() - 1;
        terminal.store_captured_command_output(
            index,
            ExtractedText {
                text: "x".repeat(MAX_COMPLETED_COMMAND_OUTPUT_BYTES),
                truncated: false,
                total_bytes: MAX_COMPLETED_COMMAND_OUTPUT_BYTES,
            },
        );
    }

    assert!(terminal.captured_command_output_bytes <= MAX_CAPTURED_COMMAND_OUTPUT_BYTES);
    assert!(terminal
        .command_record("cache-0")
        .expect("metadata retained")
        .captured_output
        .is_none());
    assert!(terminal
        .command_record("cache-64")
        .expect("newest record")
        .captured_output
        .is_some());
}

#[test]
fn prompt_ready_requires_b_and_ends_at_c() {
    let mut terminal = TerminalState::new(8, 3);
    terminal.process_input(b"\x1b]133;A\x07");
    assert!(!terminal.shell_is_prompt_ready());
    terminal.process_input(b"$ \x1b]133;B\x07");
    assert!(terminal.shell_is_prompt_ready());
    terminal.process_input(b"cmd\r\n\x1b]133;C\x07");
    assert!(!terminal.shell_is_prompt_ready());
    terminal.process_input(b"\x1b]133;D;0\x07");
    assert!(!terminal.shell_is_prompt_ready());
}

#[test]
fn jump_to_prev_command_scrolls_into_history() {
    let mut terminal = TerminalState::new(8, 3);
    // Fill enough history that the first prompt rolls into scrollback.
    terminal.process_input(b"\x1b]133;A\x07$ a\n");
    terminal.process_input(b"out1\n");
    terminal.process_input(b"\x1b]133;A\x07$ b\n");
    terminal.process_input(b"out2\n");
    terminal.process_input(b"\x1b]133;A\x07$ c\n");

    // We should now have 3 marks. The latest one is on the live grid
    // (top of viewport in the live view), so jumping prev should scroll
    // up to land on the second prompt.
    assert!(terminal.command_marks.len() >= 2);
    let scroll_before = terminal.scroll_offset;
    let jumped = terminal.jump_to_prev_command();
    assert!(jumped, "expected jump_to_prev_command to succeed");
    assert!(
        terminal.scroll_offset > scroll_before,
        "scroll_offset should advance into scrollback"
    );
}

#[test]
fn semantic_command_jump_survives_a_noop_resize() {
    let mut terminal = TerminalState::new(8, 3);
    terminal.process_input(b"\x1b]133;A\x07$ first\r\n\x1b]133;C;id=first;cmdline_url=first\x07");
    terminal.process_input(b"one\r\ntwo\r\nthree\r\n\x1b]133;D;0;id=first\x07");
    terminal.process_input(b"later-1\r\nlater-2\r\nlater-3\r\n");

    let prompt = terminal
        .command_record("first")
        .expect("semantic command")
        .prompt_start;
    let target_row = terminal
        .buffer_anchor_to_absolute(prompt)
        .expect("prompt remains in scrollback")
        .0;

    assert!(terminal.scroll_to_command("first"));
    assert!(terminal.scroll_offset > 0);
    assert_eq!(terminal.viewport_row_to_absolute(0), target_row);
    let jumped_offset = terminal.scroll_offset;

    // This is the exact second half of the sidebar regression: the render
    // pass repeats unchanged dimensions after the jump.
    terminal.on_resize(8, 3);

    assert_eq!(terminal.scroll_offset, jumped_offset);
    assert_eq!(terminal.viewport_row_to_absolute(0), target_row);
}

#[test]
fn jump_to_next_command_returns_to_live_view() {
    let mut terminal = TerminalState::new(8, 3);
    terminal.process_input(b"\x1b]133;A\x07a\n");
    terminal.process_input(b"out\n");
    terminal.process_input(b"\x1b]133;A\x07b\n");
    terminal.process_input(b"out\n");

    // Scroll up far enough that we're definitely above the latest mark.
    terminal.scroll(10);
    assert!(terminal.scroll_offset > 0);

    // Next-command jump should bring us back to the live tail.
    let jumped = terminal.jump_to_next_command();
    assert!(jumped);
    assert_eq!(terminal.scroll_offset, 0);
}

#[test]
fn pending_wrap_not_set_when_autowrap_disabled() {
    let mut terminal = TerminalState::new(3, 3);

    // Disable autowrap (DECRST 7), then overflow the row.
    terminal.process_input(b"\x1b[?7l");
    terminal.process_input(b"abcd");

    // Without autowrap the last column is overwritten in place.
    assert_eq!(terminal.cursor_row, 0);
    assert_eq!(terminal.cursor_col, 2);
    assert_eq!(terminal.grid[0][2].character, 'd');
}

#[test]
fn decrc_restores_pending_wrap_so_right_prompt_does_not_drop_cursor() {
    // Repro for the starship cmd_duration / RPROMPT issue:
    // 左 prompt 后 ESC 7,移到右侧写满末列(置位 pending_wrap),
    // ESC 8 恢复光标。VT510 规范下 DECRC 必须恢复保存时的 Last Column
    // Flag(此处为 false),否则后续字符(zsh-autosuggestions ghost text)
    // 会立刻触发换行,在屏底引发滚动,看上去光标多下移一行。
    let mut terminal = TerminalState::new(6, 3); // 6 cols × 3 rows

    // 左 prompt 写到第 0 行第 2 列,保存光标(pending_wrap=false)
    terminal.process_input(b"P>");
    terminal.process_input(b"\x1b7");
    assert_eq!(terminal.cursor_row, 0);
    assert_eq!(terminal.cursor_col, 2);
    assert!(!terminal.pending_wrap);

    // 移到末列写入,触发 pending_wrap(末列延迟换行)
    terminal.process_input(b"\x1b[6GR");
    assert_eq!(terminal.cursor_row, 0);
    assert_eq!(terminal.cursor_col, 5);
    assert!(terminal.pending_wrap);

    // 恢复光标:应同时恢复 pending_wrap=false
    terminal.process_input(b"\x1b8");
    assert_eq!(terminal.cursor_row, 0);
    assert_eq!(terminal.cursor_col, 2);
    assert!(
        !terminal.pending_wrap,
        "DECRC must restore the Last Column Flag"
    );

    // 下一字符不应再触发换行
    terminal.process_input(b"x");
    assert_eq!(terminal.cursor_row, 0);
    assert_eq!(terminal.cursor_col, 3);
    assert_eq!(terminal.grid[0][2].character, 'x');
}

#[test]
fn osc_4_sets_queries_and_resets_palette_entries() {
    let mut terminal = TerminalState::new(8, 2);

    // Set index 1 via hex spec and index 2 via rgb spec in one sequence.
    terminal.process_input(b"\x1b]4;1;#ff0000;2;rgb:00/ff/00\x1b\\");
    assert_eq!(terminal.dynamic_palette[1], Some((255, 0, 0)));
    assert_eq!(terminal.dynamic_palette[2], Some((0, 255, 0)));

    // Query returns the override for set slots and xterm defaults otherwise.
    std::mem::take(&mut terminal.output_buffer);
    terminal.process_input(b"\x1b]4;1;?\x1b\\");
    let response = std::mem::take(&mut terminal.output_buffer);
    assert_eq!(
        String::from_utf8_lossy(&response),
        "\x1b]4;1;rgb:ffff/0000/0000\x1b\\"
    );
    terminal.process_input(b"\x1b]4;231;?\x1b\\");
    let response = std::mem::take(&mut terminal.output_buffer);
    assert_eq!(
        String::from_utf8_lossy(&response),
        "\x1b]4;231;rgb:ffff/ffff/ffff\x1b\\"
    );

    // OSC 104 with indices resets only those; empty resets everything.
    terminal.process_input(b"\x1b]104;1\x1b\\");
    assert_eq!(terminal.dynamic_palette[1], None);
    assert_eq!(terminal.dynamic_palette[2], Some((0, 255, 0)));
    terminal.process_input(b"\x1b]104\x1b\\");
    assert!(terminal.dynamic_palette.iter().all(|slot| slot.is_none()));
}

#[test]
fn osc_110_to_112_reset_dynamic_colors() {
    let mut terminal = TerminalState::new(8, 2);
    terminal.process_input(b"\x1b]10;#010203\x1b\\");
    terminal.process_input(b"\x1b]11;#040506\x1b\\");
    terminal.process_input(b"\x1b]12;#070809\x1b\\");
    assert_eq!(terminal.dynamic_fg, Some((1, 2, 3)));
    assert_eq!(terminal.dynamic_bg, Some((4, 5, 6)));
    assert_eq!(terminal.dynamic_cursor_color, Some((7, 8, 9)));

    terminal.process_input(b"\x1b]110\x1b\\");
    terminal.process_input(b"\x1b]111\x1b\\");
    terminal.process_input(b"\x1b]112\x1b\\");
    assert_eq!(terminal.dynamic_fg, None);
    assert_eq!(terminal.dynamic_bg, None);
    assert_eq!(terminal.dynamic_cursor_color, None);
}
