//! Frozen original scalar-cell composition bodies, independent of the shared
//! target-selection and NFC composition methods being introduced.
use super::*;
use unicode_normalization::UnicodeNormalization;

impl TerminalState {
    fn frozen_compose_mark(&mut self, mark: char) {
        let mut base_col = if self.vt.screen.pending_wrap {
            self.vt.screen.cursor_col
        } else {
            let Some(col) = self.vt.screen.cursor_col.checked_sub(1) else {
                return;
            };
            col
        };
        if self
            .vt.screen
            .grid
            .get(self.vt.screen.cursor_row, base_col)
            .flags
            .wide_continuation()
        {
            let Some(col) = base_col.checked_sub(1) else {
                return;
            };
            base_col = col;
        }
        let base = self
            .vt.screen
            .grid
            .get(self.vt.screen.cursor_row, base_col)
            .character;
        if matches!(base, ' ' | '\0') {
            return;
        }
        let mut composed = String::with_capacity(2);
        composed.push(base);
        composed.push(mark);
        let nfc: String =
            unicode_normalization::UnicodeNormalization::nfc(composed.as_str()).collect();
        let mut chars = nfc.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            if ch != base {
                let effect = self.vt.screen.replace_combining_target(base_col, ch);
                self.apply_screen_mutation(effect);
            }
        }
    }
}

#[test]
fn shared_composition_matches_frozen_host_state() {
    for seed in [0, 3, 7] {
        for cursor_col in [0, 1, 5] {
            for pending_wrap in [false, true] {
                for continuation in [false, true] {
                    for base in [
                        'e', 'A', '\u{00c5}', '\u{1100}', '\u{ac00}', ' ', '\0', '界', '\u{0344}',
                        '\u{0340}', '\u{05c8}',
                    ] {
                        let mut actual = alternate_parity_tests::seeded(seed);
                        let mut original = alternate_parity_tests::seeded(seed);
                        for t in [&mut actual, &mut original] {
                            if seed & 1 != 0 {
                                t.process_input(b"\x1b[?1049h");
                            }
                            t.vt.screen.cursor_row = seed % 5;
                            t.vt.screen.cursor_col = cursor_col;
                            t.vt.screen.pending_wrap = pending_wrap;
                            for col in 0..6 {
                                let cell = t.vt.screen.grid.get_mut(t.vt.screen.cursor_row, col);
                                cell.character = base;
                                cell.flags.set_wide_continuation(continuation);
                            }
                        }
                        for mark in [
                            '\u{0301}', '\u{0308}', '\u{030a}', '\u{1161}', '\u{11a8}', '\u{fe0f}',
                            '\u{200d}', '\u{05c8}',
                        ] {
                            actual.apply_combining_mark(mark);
                            original.frozen_compose_mark(mark);
                            assert_eq!(alternate_parity_tests::snapshot(&actual), alternate_parity_tests::snapshot(&original),
                                "seed={seed} col={cursor_col} pending={pending_wrap} continuation={continuation} base={base:?} mark={mark:?}");
                        }
                    }
                }
            }
        }
    }
}
