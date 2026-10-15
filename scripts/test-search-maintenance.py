#!/usr/bin/env python3
"""Exercise the production search-maintenance branch without GUI dependencies.

The native search/render state and terminal navigation are explicit fixtures.
This checks maintenance dispatch, not real projection or GUI integration.
--check-regression requires the former automatic reveal branch to fail.
"""
from pathlib import Path
import argparse
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-regression", action="store_true")
    args = parser.parse_args()
    source = (ROOT / "src/app/rendering.rs").read_text()
    body = source.split("    pub fn render_floating_panels(", 1)[1]
    body = body.split("        if self.search_state.is_open", 1)[1]
    body = "        if self.search_state.is_open" + body.split(
        "        let (search_needs_refresh, delayed_refresh)", 1
    )[0]
    fixture = r'''
#![allow(dead_code)]
struct Metadata { session_id: String }
struct Policy { revision: u64 }
impl Policy { fn revision(&self)->u64 {self.revision} }
struct Session { metadata: Metadata, projection_policy: Policy }
struct Manager { session: Session }
impl Manager { fn get_active_session_mut(&mut self)->&mut Session {&mut self.session} }
struct Search {is_open:bool,projection_message:Option<String>,session:String,revision:u64,hidden:bool}
impl Search {
fn projection_diagnostic_is_current(&self,id:&str,revision:u64)->bool {self.session==id && self.revision==revision}
fn clear_projection_diagnostic(&mut self){self.projection_message=None;self.hidden=false;}
}
struct App {search_state:Search,session_manager:Manager,scroll:usize,reveals:usize}
impl App {
fn reveal_current_search_match(&mut self){self.scroll=77;self.reveals+=1;}
fn maintain(&mut self){ BODY }
}
fn app(scroll:usize)->App {App{search_state:Search{is_open:true,projection_message:Some("hidden".into()),session:"a".into(),revision:1,hidden:true},session_manager:Manager{session:Session{metadata:Metadata{session_id:"a".into()},projection_policy:Policy{revision:2}}},scroll,reveals:0}}
#[test]fn return_live_survives_changed_projection(){let mut a=app(0);a.maintain();assert_eq!(a.scroll,0);assert_eq!(a.reveals,0);assert!(a.search_state.projection_message.is_none());assert!(!a.search_state.hidden);}
#[test]fn manual_reading_survives_changed_projection(){let mut a=app(15);a.maintain();assert_eq!(a.scroll,15);assert_eq!(a.reveals,0);}
#[test]fn stale_session_diagnostic_cannot_navigate_new_session(){let mut a=app(0);a.session_manager.session.metadata.session_id="b".into();a.maintain();assert_eq!(a.reveals,0);assert!(a.search_state.projection_message.is_none());}
#[test]fn valid_hidden_diagnostic_is_preserved(){let mut a=app(15);a.session_manager.session.projection_policy.revision=1;a.maintain();assert_eq!(a.scroll,15);assert!(a.search_state.hidden);assert!(a.search_state.projection_message.is_some());}
#[test]fn closed_search_does_not_run_maintenance(){let mut a=app(15);a.search_state.is_open=false;a.maintain();assert_eq!(a.scroll,15);assert!(a.search_state.projection_message.is_some());}
#[test]fn repeated_changes_do_not_resurrect_navigation(){let mut a=app(0);for revision in 2..50{a.session_manager.session.projection_policy.revision=revision;a.maintain();}assert_eq!(a.scroll,0);assert_eq!(a.reveals,0);}
'''
    with tempfile.TemporaryDirectory(prefix="ember-search-maintenance-") as temp:
        temp = Path(temp)
        variants = [("current", body)]
        if args.check_regression:
            assert body.count("self.search_state.clear_projection_diagnostic();") == 1
            variants.append(("regression", body.replace(
                "self.search_state.clear_projection_diagnostic();",
                "self.reveal_current_search_match();")))
        for name, branch in variants:
            path = temp / (name + ".rs")
            path.write_text(fixture.replace("BODY", branch))
            binary = temp / name
            subprocess.run([os.environ.get("RUSTC", "rustc"), "--edition=2021", "--test", str(path), "-o", str(binary)], check=True)
            result = subprocess.run([str(binary)], capture_output=True, text=True)
            print(name + ":\n" + result.stdout)
            if name == "current":
                assert result.returncode == 0, result.stderr
            else:
                assert result.returncode != 0
                assert "return_live_survives_changed_projection ... FAILED" in result.stdout
                assert "manual_reading_survives_changed_projection ... FAILED" in result.stdout
    navigation = (ROOT / "src/app/input.rs").read_text()
    explicit = navigation.split("    pub(super) fn reveal_hidden_search_match(", 1)[1].split("    fn scroll_active_terminal(", 1)[0]
    assert "self.reveal_current_search_match();" in explicit
    print("Explicit reveal remains wired; maintenance regression checks passed.")


if __name__ == "__main__":
    run()
