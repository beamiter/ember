//! Original Ember host algorithms frozen from candidate cdee016c before extraction.
use super::*;
use std::collections::BTreeMap;

impl TerminalState {
    fn frozen_soft_reset(&mut self) {
        self.screen.current_fg = Color::Default;
        self.screen.current_bg = Color::Default;
        self.global_bg = Color::Default;
        self.screen.current_flags = StyleFlags::default();
        self.screen.g0_charset = Charset::Ascii;
        self.screen.g1_charset = Charset::Ascii;
        self.screen.active_charset = Charset::Ascii;
        self.screen.scroll_region_top = 0;
        self.screen.scroll_region_bottom = self.screen.grid.rows().saturating_sub(1);
        self.screen.saved_state = None;
        self.screen.origin_mode = false;
        self.screen.insert_mode = false;
        // DECTCEM and DECAWM on, DECCKM (application cursor keys) off,
        // DECKPAM (application keypad) off. Alternate Scroll returns to the
        // xterm-resource default (enabled).
        self.protocol.modes_mut().insert(25);
        self.protocol.modes_mut().insert(7);
        self.protocol.modes_mut().insert(1007);
        self.protocol.modes_mut().remove(&1);
        self.protocol.modes_mut().remove(&66);
        self.screen.pending_wrap = false;
    }

    fn frozen_handle_decrqss(&mut self, pt: &[u8]) {
        let reply = match pt {
            b" q" => {
                let n = match self.screen.cursor_shape {
                    CursorShape::Underline => 4,
                    CursorShape::Beam => 6,
                    _ => 2,
                };
                format!("\x1bP1$r{n} q\x1b\\")
            }
            b"r" => format!(
                "\x1bP1$r{};{}r\x1b\\",
                self.screen.scroll_region_top + 1,
                self.screen.scroll_region_bottom + 1
            ),
            b"\"q" => {
                let n = if self.screen.current_flags.protected() {
                    1
                } else {
                    0
                };
                format!("\x1bP1$r{n}\"q\x1b\\")
            }
            b"m" => format!("\x1bP1$r{}\x1b\\", self.frozen_decrqss_sgr_payload()),
            b"\"p" => "\x1bP1$r65;1\"p\x1b\\".to_string(),
            _ => "\x1bP0$r\x1b\\".to_string(),
        };
        self.output_buffer.extend_from_slice(reply.as_bytes());
    }

    fn frozen_decrqss_sgr_payload(&self) -> String {
        let mut params: Vec<String> = Vec::new();
        if self.screen.current_flags.bold() {
            params.push("1".into());
        }
        if self.screen.current_flags.dim() {
            params.push("2".into());
        }
        if self.screen.current_flags.italic() {
            params.push("3".into());
        }
        match self.screen.current_flags.underline() {
            UnderlineStyle::None => {}
            UnderlineStyle::Single => params.push("4".into()),
            UnderlineStyle::Double => params.push("21".into()),
            UnderlineStyle::Curly => params.push("4:3".into()),
            UnderlineStyle::Dotted => params.push("4:4".into()),
            UnderlineStyle::Dashed => params.push("4:5".into()),
        }
        if self.screen.current_flags.blink() {
            params.push("5".into());
        }
        if self.screen.current_flags.inverse() {
            params.push("7".into());
        }
        if self.screen.current_flags.strikethrough() {
            params.push("9".into());
        }
        params.extend(Self::frozen_sgr_color_params(self.screen.current_fg, true));
        params.extend(Self::frozen_sgr_color_params(self.screen.current_bg, false));
        if params.is_empty() {
            "0m".into()
        } else {
            format!("{}m", params.join(";"))
        }
    }

    fn frozen_sgr_color_params(color: Color, foreground: bool) -> Vec<String> {
        let named = |n: u8| {
            vec![if foreground {
                n.to_string()
            } else {
                (n + 10).to_string()
            }]
        };
        match color {
            Color::Default => Vec::new(),
            Color::Black => named(30),
            Color::Red => named(31),
            Color::Green => named(32),
            Color::Yellow => named(33),
            Color::Blue => named(34),
            Color::Magenta => named(35),
            Color::Cyan => named(36),
            Color::White => named(37),
            Color::BrightBlack => named(90),
            Color::BrightRed => named(91),
            Color::BrightGreen => named(92),
            Color::BrightYellow => named(93),
            Color::BrightBlue => named(94),
            Color::BrightMagenta => named(95),
            Color::BrightCyan => named(96),
            Color::BrightWhite => named(97),
            Color::Indexed(index) => vec![
                if foreground { "38" } else { "48" }.into(),
                "5".into(),
                index.to_string(),
            ],
            Color::Rgb(red, green, blue) => vec![
                if foreground { "38" } else { "48" }.into(),
                "2".into(),
                red.to_string(),
                green.to_string(),
                blue.to_string(),
            ],
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
    out.insert("parser".into(), format!("{:?}", t.parser.snapshot()));
    out.insert(
        "kitty_image_count".into(),
        t.kitty_graphics.image_count().to_string(),
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
    super::super::structural_parity_tests::prime(&mut t);
    t
}
const COMMANDS: &[&[u8]] = &[
    b"\x1b[!p",
    b"\x1bP$qm\x1b\\",
    b"\x1bP$qr\x1b\\",
    b"\x1bP$q q\x1b\\",
    b"\x1bP$q\"q\x1b\\",
    b"\x1bP$q\"p\x1b\\",
    b"\x1bP$qx\x1b\\",
    b"\x1bP$q\x1b\\",
    b"\x1bPxqm\x1b\\",
    b"\x1b[!p\x1bP$qm\x1b\\",
];
fn cases() -> BTreeMap<String, Snapshot> {
    let mut all = BTreeMap::new();
    for seed in 0..4 {
        all.insert(format!("seed_{seed}"), snapshot(&seeded(seed)));
        for (cmd, bytes) in COMMANDS.iter().enumerate() {
            let mut t = seeded(seed);
            t.process_input(bytes);
            all.insert(format!("seed_{seed}_cmd_{cmd}"), snapshot(&t));
        }
    }
    all
}
#[test]
#[ignore = "explicit pre-change capture only, never updates a fixture"]
fn print_reset_query_baseline() {
    println!(
        "RESET_QUERY_BASELINE\t{}",
        serde_json::to_string(&cases()).unwrap()
    );
}
#[test]
fn reset_query_original_production_goldens_and_all_partitions() {
    #[derive(serde::Deserialize)]
    struct Captured {
        values: Vec<String>,
        cases: BTreeMap<String, BTreeMap<String, usize>>,
    }
    let captured: Captured = serde_json::from_str(include_str!(
        "../../tests/fixtures/reset_query_before_engine.json"
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
    assert_eq!(cases(), expected);
    let partitions: BTreeMap<String, Snapshot> = serde_json::from_str(include_str!(
        "../../tests/fixtures/reset_query_partition_overrides.json"
    ))
    .unwrap();
    for_each_partition(|seed, cmd, mask, t| {
        let key = format!("seed_{seed}_cmd_{cmd}_mask_{mask}");
        let mut want = expected[&format!("seed_{seed}_cmd_{cmd}")].clone();
        want.extend(partitions.get(&key).cloned().unwrap_or_default());
        let got = snapshot(t);
        for (lane, value) in &want {
            assert_eq!(&got[lane], value, "{key}: {lane}");
        }
        assert_eq!(got.len(), want.len());
    });
}
fn for_each_partition(mut check: impl FnMut(usize, usize, usize, &TerminalState)) {
    for seed in 0..4 {
        for (cmd, bytes) in COMMANDS.iter().enumerate() {
            for mask in 0..(1usize << (bytes.len() - 1)) {
                let mut t = seeded(seed);
                let start_time = t.sync_output_start;
                let mut start = 0;
                for end in 1..=bytes.len() {
                    if end == bytes.len() || mask & (1 << (end - 1)) != 0 {
                        t.process_input(&bytes[start..end]);
                        start = end;
                    }
                }
                assert_eq!(t.sync_output_start, start_time, "watchdog must not restart");
                check(seed, cmd, mask, &t);
            }
        }
    }
}
#[test]
#[ignore = "explicit pre-change capture only, never updates a fixture"]
fn print_reset_query_partition_baseline() {
    let base = cases();
    let mut overrides = BTreeMap::new();
    for_each_partition(|seed, cmd, mask, t| {
        let whole = &base[&format!("seed_{seed}_cmd_{cmd}")];
        let delta: Snapshot = snapshot(t)
            .into_iter()
            .filter(|(k, v)| whole[k] != *v)
            .collect();
        if !delta.is_empty() {
            overrides.insert(format!("seed_{seed}_cmd_{cmd}_mask_{mask}"), delta);
        }
    });
    println!(
        "RESET_QUERY_PARTITIONS\t{}",
        serde_json::to_string(&overrides).unwrap()
    );
}

#[test]
fn reset_query_frozen_host_differential() {
    for seed in 0..32 {
        let mut actual = seeded(seed);
        let mut reference = seeded(seed);
        actual.soft_reset();
        reference.frozen_soft_reset();
        assert_eq!(snapshot(&actual), snapshot(&reference));
        for selector in [
            b"m".as_slice(),
            b"r",
            b" q",
            b"\"q",
            b"\"p",
            b"",
            b"unknown",
        ] {
            let mut actual = seeded(seed);
            let mut reference = seeded(seed);
            actual.handle_decrqss(selector);
            reference.frozen_handle_decrqss(selector);
            assert_eq!(snapshot(&actual), snapshot(&reference));
        }
    }
}
#[test]
fn reset_query_exact_sgr_bytes() {
    for (setup, expected) in [
        ("0", "0m"),
        ("1;2;3;4;5;7;9", "1;2;3;4;5;7;9m"),
        ("21", "21m"),
        ("4:3", "4:3m"),
        ("4:4", "4:4m"),
        ("4:5", "4:5m"),
        ("31;44", "31;44m"),
        ("91;104", "91;104m"),
        ("38;5;0;48;5;255", "38;5;0;48;5;255m"),
        (
            "38;2;0;128;255;48;2;255;128;0",
            "38;2;0;128;255;48;2;255;128;0m",
        ),
    ] {
        let mut t = TerminalState::new(6, 5);
        t.process_input(format!("\x1b[{setup}m\x1bP$qm\x1b\\").as_bytes());
        assert_eq!(
            t.output_buffer,
            format!("\x1bP1$r{expected}\x1b\\").as_bytes()
        );
        assert_eq!(t.decrqss_sgr_payload(), t.frozen_decrqss_sgr_payload());
    }
    for fg in [true, false] {
        for c in [
            Color::Default,
            Color::Black,
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::White,
            Color::BrightBlack,
            Color::BrightRed,
            Color::BrightGreen,
            Color::BrightYellow,
            Color::BrightBlue,
            Color::BrightMagenta,
            Color::BrightCyan,
            Color::BrightWhite,
            Color::Indexed(0),
            Color::Indexed(255),
            Color::Rgb(0, 128, 255),
        ] {
            assert_eq!(
                TerminalState::sgr_color_params(c, fg),
                TerminalState::frozen_sgr_color_params(c, fg)
            );
        }
    }
}
#[test]
fn reset_query_mutation_negative_controls() {
    let mut expected = seeded(3);
    expected.soft_reset();
    for mutation in 0..7 {
        let mut mutant = seeded(3);
        mutant.soft_reset();
        match mutation {
            0 => mutant.screen.cursor_row += 1,
            1 => mutant.protocol.modes_mut().remove(&1007),
            2 => mutant.protocol.modes_mut().insert(1),
            3 => mutant.protocol.modes_mut().insert(66),
            4 => mutant.global_bg = Color::Red,
            5 => mutant.sync_output_active = false,
            _ => mutant.screen.grid[0][0].character = '!',
        }
        assert_ne!(
            snapshot(&mutant),
            snapshot(&expected),
            "reset mutation {mutation} escaped"
        );
    }
    let mut t = seeded(0);
    t.handle_decrqss(b"m");
    let expected = snapshot(&t);
    t.output_buffer.pop();
    assert_ne!(snapshot(&t), expected, "reply terminator mutation escaped");
    let mut t = seeded(0);
    t.handle_decrqss(b"unknown");
    assert!(t.output_buffer.ends_with(b"\x1bP0$r\x1b\\"));
    let expected = snapshot(&t);
    let n = t.output_buffer.len();
    t.output_buffer[n - 5] = b'1';
    assert_ne!(snapshot(&t), expected, "failure reply mutation escaped");
}
#[test]
fn reset_query_frost_policy_must_not_be_migrated_as_ember() {
    // Frozen Frost 9322..9345 resets only 6/4/25/7. This is a contract contrast,
    // not a claim to execute Frost's own production implementation here.
    let mut ember = seeded(0);
    let mut frost_modes = ember.protocol.modes().clone();
    frost_modes.remove(&6);
    frost_modes.remove(&4);
    frost_modes.insert(25);
    frost_modes.insert(7);
    ember.soft_reset();
    for mode in [1, 66] {
        assert!(frost_modes.contains(&mode));
        assert!(!ember.protocol.modes().contains(&mode));
    }
    assert!(!frost_modes.contains(&1007));
    assert!(ember.protocol.modes().contains(&1007));
}

#[test]
fn reset_query_sync_lifecycle_survives_decstr() {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    for alt in [false, true] {
        for explicit_end in [false, true] {
            let mut t = seeded(usize::from(alt));
            let base = Instant::now();
            let now = Arc::new(Mutex::new(base));
            let clock = now.clone();
            t.sync_output_clock =
                super::super::SyncOutputClock::new(move || *clock.lock().unwrap());
            t.process_input(b"\x1b[?2026h");
            let start = t.sync_output_start;
            *now.lock().unwrap() = base + Duration::from_millis(750);
            t.process_input(b"\x1b[!p\x1bP$qm\x1b\\");
            assert!(t.sync_output_active);
            assert_eq!(t.sync_output_start, start);
            assert_eq!(t.screen.use_alt_buffer, alt);
            if explicit_end {
                t.process_input(b"\x1b[?2026l");
            } else {
                *now.lock().unwrap() = base + Duration::from_secs(1);
                t.check_sync_output_timeout();
                assert!(t.sync_output_active);
                *now.lock().unwrap() = base + Duration::from_millis(1001);
                t.check_sync_output_timeout();
            }
            assert!(!t.sync_output_active);
            assert!(t.sync_output_start.is_none());
        }
    }
}
