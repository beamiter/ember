use super::{ClipboardReadKind, Color, TerminalState, UnderlineStyle};

#[test]
fn resize_preserves_full_screen_scroll_region() {
    let mut terminal = TerminalState::new(4, 3);

    terminal.on_resize(4, 6);

    assert_eq!(terminal.scroll_region_top, 0);
    assert_eq!(terminal.scroll_region_bottom, 5);
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

#[test]
fn osc_5522_read_request_is_queued() {
    let mut terminal = TerminalState::new(8, 2);

    terminal.process_input(b"\x1b]5522;type=read;Lg==\x1b\\");

    let requests = terminal.take_clipboard_read_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, ClipboardReadKind::MimeList);
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
