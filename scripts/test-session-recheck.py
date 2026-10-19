#!/usr/bin/env python3
"""Pure tests of the exact async state and UI callback methods, with fake metadata.

No application/native build, real directory inspection, egui rendering or socket.
The fixture replaces Context repaint, text bounding and inspector storage only;
the production callback methods and notice formatter are extracted verbatim.
"""
import json
import os
import runpy
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / 'src/session_recovery.rs').read_text()


def item(marker):
    start = source.index(marker)
    opening = source.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


# Verify the real UI wiring separately; this is not a native click test.
show = item('pub fn show(')
assert show.index('self.cancel_recheck();') < show.index('self.poll_recheck();')
assert show.index('self.poll_recheck();') < show.index('if let Some(receiver) = &self.pending')
assert 'self.original.is_some() && !self.recheck.is_busy()' in show
assert '.clicked() { self.recheck(ctx); }' in show
assert 'self.open = open && !dismiss;' in show
assert show.rindex('self.cancel_recheck();') > show.index('self.open = open && !dismiss;')
assert 'ui.label(&self.recheck_notice)' in show and 'ui.label(&self.export_notice)' in show
assert 'inspect_recovery' not in show

code = '''#![allow(dead_code)]
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
mod egui { #[derive(Clone, Default)] pub struct Context; impl Context { pub fn request_repaint(&self) {} } }
mod review_text { pub fn bound_toast_text(text: String) -> String { text } }
'''
code += '#[path=' + json.dumps(str(ROOT / 'src/session_recovery/recheck.rs')) + '] mod recheck;\n'
code += '''use recheck::AsyncRecheck;
struct SessionRecovery {
    open: bool, original: Option<Arc<[u8]>>, summary: usize,
    recheck: AsyncRecheck, recheck_notice: String, export_notice: String,
}
impl SessionRecovery {
'''
for name in ['recheck', 'poll_recheck', 'cancel_recheck']:
    code += item('fn ' + name + '(') + '\n'
code += '}\n' + item('fn recheck_notice(') + '\n'
code += '''
struct Snapshot { sessions: Vec<()> }
struct Summary { skipped_sessions: usize, invalid_cwds: [bool; 64] }
struct Gates { started: mpsc::Sender<()>, release: mpsc::Receiver<()> }
static INSPECTOR: Mutex<Option<Gates>> = Mutex::new(None);
fn inspect_recovery(_: &str) -> Result<(Snapshot, Summary), ()> {
    let gates = INSPECTOR.lock().unwrap().take().expect("only one fake inspector");
    gates.started.send(()).unwrap();
    gates.release.recv().unwrap();
    Ok((Snapshot { sessions: vec![(); 2] }, Summary { skipped_sessions: 1, invalid_cwds: [false; 64] }))
}
fn fixture() -> SessionRecovery {
    SessionRecovery { open: true, original: Some(Arc::from(&b"synthetic metadata"[..])), summary: 7,
        recheck: AsyncRecheck::default(), recheck_notice: String::new(), export_notice: "original export notice".into() }
}
fn begin(mut recovery: SessionRecovery) -> (SessionRecovery, mpsc::Sender<()>) {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *INSPECTOR.lock().unwrap() = Some(Gates { started: started_tx, release: release_rx });
    let (returned_tx, returned_rx) = mpsc::channel();
    let invoker = std::thread::spawn(move || {
        recovery.recheck(&egui::Context::default());
        returned_tx.send(recovery).unwrap_or_else(|_| panic!("callback receiver dropped"));
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let returned = returned_rx.recv_timeout(Duration::from_secs(2));
    if returned.is_err() { let _ = release_tx.send(()); }
    invoker.join().unwrap();
    (returned.expect("production UI callback waited for fake inspector"), release_tx)
}
fn finish(recovery: &mut SessionRecovery) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while recovery.recheck.is_busy() {
        recovery.poll_recheck();
        assert!(Instant::now() < deadline, "fake inspector completion timeout");
        std::thread::yield_now();
    }
}
#[test]
fn exact_ui_callback_is_nonblocking_and_preserves_independent_export_results() {
    let (mut recovery, release) = begin(fixture());
    let original = recovery.original.clone().unwrap();
    recovery.recheck(&egui::Context::default()); // duplicate must not run another inspector
    recovery.export_notice = "Original exported locally.".into();
    release.send(()).unwrap();
    finish(&mut recovery);
    let expected = "Recheck: 2 session records readable; 1 skipped; 0 unavailable or invalid directories. Live sessions unchanged.";
    assert_eq!(recovery.export_notice, "Original exported locally.", "recheck completion must preserve export notice");
    assert_eq!(recovery.recheck_notice, expected);
    assert_eq!(recovery.summary, 7);
    assert!(Arc::ptr_eq(recovery.original.as_ref().unwrap(), &original));
    // Opposite completion order retains the later export failure as well.
    recovery.export_notice = "Export did not complete: synthetic failure".into();
    recovery.poll_recheck();
    assert_eq!(recovery.export_notice, "Export did not complete: synthetic failure");
    assert_eq!(recovery.recheck_notice, expected);

    let (mut recovery, release) = begin(recovery);
    recovery.open = false;
    recovery.cancel_recheck();
    let discarded = recovery.recheck_notice.clone();
    recovery.recheck(&egui::Context::default()); // cannot start while closed
    recovery.open = true;
    for _ in 0..16 { recovery.recheck(&egui::Context::default()); }
    assert!(recovery.recheck.is_busy());
    recovery.export_notice = "Export worker stopped. Check the chosen local file before trying again.".into();
    release.send(()).unwrap();
    finish(&mut recovery);
    assert_eq!(recovery.recheck_notice, discarded);
    assert_eq!(recovery.export_notice, "Export worker stopped. Check the chosen local file before trying again.");
    assert_eq!(recovery.summary, 7);
    assert!(Arc::ptr_eq(recovery.original.as_ref().unwrap(), &original));
    let (mut recovery, release) = begin(recovery);
    release.send(()).unwrap();
    finish(&mut recovery);
    assert_eq!(recovery.recheck_notice, expected);
}
'''
show_fixture = runpy.run_path(str(ROOT / 'scripts/session_recheck_show_fixture.py'))
show_code = show_fixture['build_show_code'](source, ROOT / 'src/session_recovery/recheck.rs')
with tempfile.TemporaryDirectory(prefix='ember-session-recheck-') as temp:
    temp = Path(temp)
    harness = temp / 'recheck-tests.rs'
    harness.write_text(code)
    binary = temp / 'recheck-tests'
    subprocess.run([os.environ.get('RUSTC', 'rustc'), '--edition=2021', '--test', str(harness), '-o', str(binary)], check=True)
    subprocess.run([str(binary), '--nocapture'], check=True)
    show_harness = temp / 'show-tests.rs'
    show_harness.write_text(show_code)
    show_binary = temp / 'show-tests'
    subprocess.run([os.environ.get('RUSTC', 'rustc'), '--edition=2021', '--test', str(show_harness), '-o', str(show_binary)], check=True)
    # Only three new show cases; the same four state tests are already run above.
    subprocess.run([str(show_binary), 'show_', '--test-threads=1', '--nocapture'], check=True)
print('Exact async state/callback (5) and actual show/receiver logic (3) passed with fake metadata/UI boundaries; no native or real-directory latency claim.')
