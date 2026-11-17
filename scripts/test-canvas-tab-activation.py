#!/usr/bin/env python3
"""Compile exact canvas/activation production slices, not native GUI acceptance.

Requires rustc and the transcript_canvas.rs from the Cargo.toml-pinned core.
The session/tab containers below are explicit test doubles. The activation
method, both canvas state machines and their existing unit tests are real.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile


def extract(source, signature):
    start = source.index(signature)
    body = source.index('{', start)
    depth = 1
    end = body + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--core-canvas', type=Path, required=True)
    parser.add_argument('--rustc', default='rustc')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    tabs = (root / 'src/app/tabs.rs').read_text()
    input_source = (root / 'src/app/input.rs').read_text()
    activation = extract(tabs, '    pub(super) fn select_tab_and_suspend_canvas(')
    # Verify the real entry points use this transition, not just the test double.
    assert 'self.select_tab_and_suspend_canvas(tab_idx)' in extract(tabs, '    pub fn activate_tab(')
    assert 'self.select_tab_and_suspend_canvas(tab_idx)' in extract(input_source, '    pub fn activate_session(')
    assert tabs.split('#[cfg(test)]')[0].count('self.tabs.set_active(tab_idx)') == 1
    assert 'self.tabs.set_active(tab_idx)' not in input_source.split('#[cfg(test)]')[0]
    harness = r'''
extern crate self as jterm_core;
#[path = "CORE"] pub mod transcript_canvas;
#[path = "EMBER"] mod ember_canvas;
mod app {
    use super::ember_canvas::TranscriptCanvas;
    struct Tabs { active: usize }
    impl Tabs {
        fn len(&self) -> usize { 2 }
        fn active_index(&self) -> usize { self.active }
        fn sessions_in(&self, index: usize) -> Vec<usize> {
            if index == 0 { vec![0, 1] } else { vec![2] }
        }
        fn set_active(&mut self, index: usize) { if index < 2 { self.active = index; } }
    }
    struct Session { transcript_canvas: TranscriptCanvas }
    struct Sessions(Vec<Session>);
    impl Sessions { fn sessions_mut(&mut self) -> &mut [Session] { &mut self.0 } }
    struct TerminalApp { tabs: Tabs, session_manager: Sessions }
    impl TerminalApp {
METHOD
    }
    fn running() -> TerminalApp {
        let mut sessions = vec![];
        for _ in 0..3 {
            let mut canvas = TranscriptCanvas::default();
            canvas.present(0, 200, 1, 20, 80, true, 20.0, 0.0, false);
            let frame = canvas.present(0, 205, 2, 20, 80, true, 20.0, 0.016, false);
            assert!(frame.animating);
            sessions.push(Session { transcript_canvas: canvas });
        }
        TerminalApp { tabs: Tabs { active: 0 }, session_manager: Sessions(sessions) }
    }
    #[test]
    fn round_trip_without_intermediate_render_retires_both_outgoing_panes() {
        let mut app = running();
        app.select_tab_and_suspend_canvas(1);
        app.select_tab_and_suspend_canvas(0);
        // No render happened while B was active. A resumes directly at live.
        for index in [0, 1] {
            let frame = app.session_manager.0[index].transcript_canvas.present(
                5, 205, 2, 20, 80, true, 20.0, 0.032, false);
            assert_eq!(frame.offset_rows, 0);
            assert!(!frame.animating);
            assert!(frame.follows_live);
        }
    }
    #[test]
    fn same_tab_or_invalid_target_does_not_interrupt_motion() {
        for index in [0, 2] {
            let mut app = running();
            app.select_tab_and_suspend_canvas(index);
            let frame = app.session_manager.0[0].transcript_canvas.present(
                5, 205, 2, 20, 80, true, 20.0, 0.032, false);
            assert!(frame.animating);
        }
    }
    #[test]
    fn reader_fast_round_trip_retains_fractional_anchor_and_not_live() {
        let mut app = running();
        let canvas = &mut app.session_manager.0[0].transcript_canvas;
        canvas.seek_pixels(203.25);
        canvas.present(5, 205, 2, 20, 80, true, 20.0, 0.032, false);
        app.select_tab_and_suspend_canvas(1);
        app.select_tab_and_suspend_canvas(0);
        let frame = app.session_manager.0[0].transcript_canvas.present(
            16, 210, 3, 20, 80, false, 20.0, 0.048, false);
        assert_eq!(frame.offset_rows, 16);
        assert_eq!(frame.translation, 16.75);
        assert!(!frame.follows_live);
        assert!(!frame.animating);
    }
    #[test]
    fn new_resume_input_wins_after_fast_round_trip() {
        let mut app = running();
        app.select_tab_and_suspend_canvas(1);
        app.select_tab_and_suspend_canvas(0);
        let canvas = &mut app.session_manager.0[0].transcript_canvas;
        assert!(canvas.seek_pixels(123.25));
        let frame = canvas.present(5, 205, 2, 20, 80, true, 20.0, 0.032, false);
        assert_eq!(frame.offset_rows, 7);
        assert_eq!(frame.translation, 16.75);
        assert!(!frame.follows_live);
        assert!(!frame.animating);
    }
}
'''
    harness = harness.replace('CORE', str(args.core_canvas.resolve())).replace(
        'EMBER', str(root / 'src/transcript_canvas.rs')).replace('METHOD', activation)
    with tempfile.TemporaryDirectory(prefix='canvas-activation-') as tmp:
        source, binary = Path(tmp) / 'test.rs', Path(tmp) / 'test'
        source.write_text(harness)
        subprocess.run([args.rustc, '--edition=2021', '--test', str(source), '-o', str(binary)], check=True)
        subprocess.run([str(binary)], check=True)


if __name__ == '__main__':
    main()
