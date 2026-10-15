#!/usr/bin/env python3
"""Check projection-aware cursor admission with explicit viewport fixtures.

Extracts production visibility methods. Does not execute projection construction,
PTY parsing or GUI rendering; native terminal tests cover those when Cargo works.
"""
from pathlib import Path
import argparse
import os
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def item(source, marker):
    start = source.index(marker)
    opening = source.index("{", start)
    depth = 1
    for token in re.finditer(r'//[^\n]*|"(?:\\.|[^"\\])*"|[{}]', source[opening + 1:]):
        depth += (token[0] == "{") - (token[0] == "}")
        if depth == 0:
            return source[start:opening + 1 + token.end()]
    raise ValueError(marker)


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-regression", action="store_true")
    args = parser.parse_args()
    source = (ROOT / "src/terminal/state.rs").read_text()
    methods = item(source, "    pub fn is_cursor_visible(") + "\n" + item(source, "    pub fn is_projected_cursor_visible(")
    fixture = r'''
#![allow(dead_code)]
struct Terminal{modes:std::collections::HashSet<u16>,scroll_offset:usize}
struct ProjectedViewport{transformed:bool,mapped:bool}
impl ProjectedViewport{fn is_transformed(&self)->bool{self.transformed}fn cursor(&self)->usize{0}fn raw_anchor_at(&self,_:usize)->Option<()>{self.mapped.then_some(())}}
impl Terminal{ METHODS }
#[test]fn chat_dock_ignores_legacy_offset(){for offset in [1,5,99999]{let t=Terminal{modes:[25].into(),scroll_offset:offset};let v=ProjectedViewport{transformed:true,mapped:true};assert!(!t.is_cursor_visible());assert!(t.is_projected_cursor_visible(&v));}}
#[test]fn offscreen_block_cursor_is_hidden(){for offset in [0,1,99]{let t=Terminal{modes:[25].into(),scroll_offset:offset};assert!(!t.is_projected_cursor_visible(&ProjectedViewport{transformed:true,mapped:false}));}}
#[test]fn vte_keeps_native_scrollback_rule(){for offset in [0,1,99]{let t=Terminal{modes:[25].into(),scroll_offset:offset};assert_eq!(t.is_projected_cursor_visible(&ProjectedViewport{transformed:false,mapped:true}),offset==0);}}
#[test]fn dec25_hide_is_respected_in_every_projection(){for transformed in [false,true]{for mapped in [false,true]{let t=Terminal{modes:std::collections::HashSet::new(),scroll_offset:0};assert!(!t.is_projected_cursor_visible(&ProjectedViewport{transformed,mapped}));}}}
#[test]fn history_live_and_mode_roundtrips_follow_current_projection(){let t=Terminal{modes:[25].into(),scroll_offset:5};for (transformed,mapped,expected) in [(false,true,false),(true,true,true),(true,false,false),(true,true,true),(false,true,false)]{assert_eq!(t.is_projected_cursor_visible(&ProjectedViewport{transformed,mapped}),expected);}}
'''
    variants = [("current", methods)]
    if args.check_regression:
        old = methods.replace("self.modes.contains(&25) && viewport.raw_anchor_at(viewport.cursor()).is_some()", "self.is_cursor_visible()")
        assert old != methods
        variants.append(("regression", old))
    with tempfile.TemporaryDirectory(prefix="ember-projected-cursor-") as temp:
        temp = Path(temp)
        for name, body in variants:
            path = temp / (name + ".rs")
            path.write_text(fixture.replace("METHODS", body))
            binary = temp / name
            subprocess.run([os.environ.get("RUSTC", "rustc"), "--edition=2021", "--test", str(path), "-o", str(binary)], check=True)
            result = subprocess.run([str(binary)], capture_output=True, text=True)
            print(name + ":\n" + result.stdout)
            if name == "current":
                assert result.returncode == 0, result.stderr
            else:
                assert result.returncode != 0
                assert "chat_dock_ignores_legacy_offset ... FAILED" in result.stdout
    main = (ROOT / "src/main.rs").read_text()
    assert main.count("projected_cursor_visible_for_session(session, renderer)") == 2
    render = (ROOT / "src/app/rendering.rs").read_text()
    assert "terminal_guard.is_projected_cursor_visible(&viewport)" in render
    ui = (ROOT / "src/ui.rs").read_text()
    assert "&& terminal.is_projected_cursor_visible(&viewport)" in ui
    print("Blink scheduling, split-pane and final-paint wiring checks passed.")


if __name__ == "__main__":
    run()
