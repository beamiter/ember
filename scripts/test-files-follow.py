#!/usr/bin/env python3
"""Exercise production Files completion paths using only Python and rustc.

The fixture mocks UI, process, and transport boundaries. It does not verify egui
rendering or live SSH. Current production fragments are compiled on every run.
"""
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap

ROOT = Path(__file__).resolve().parents[1]

def item(source, signature):
    start = source.index(signature)
    opening = source.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]

def main():
    follow = (ROOT / "src/ssh_files_follow.rs").read_text()
    app = (ROOT / "src/main.rs").read_text()
    sidebar = (ROOT / "src/sidebar.rs").read_text()
    state = (ROOT / "src/app/state.rs").read_text()
    template = (ROOT / "scripts/files_follow_harness.rs").read_text()
    signatures = {
        "OBSERVATION_KEY": "pub(crate) struct ObservationKey {",
        "OBSERVATION": "pub(crate) enum Observation {",
        "FOLLOW_COMMIT": "pub(crate) enum FollowCommit {",
        "PENDING_PROBE": "pub(crate) struct PendingProbe {",
        "SIDEBAR_UI": "pub(crate) struct SidebarUiSnapshot {",
        "SIDEBAR_UI_IMPL": "impl SidebarUiSnapshot {",
        "PROBE_SNAPSHOT": "pub(crate) struct ProbeSnapshot {",
        "PROBE_RESULT": "pub(crate) struct ProbeResult {",
        "RESULT_CURRENT": "pub(crate) fn result_is_current(",
        "FILES_AUTHORITY": "pub(crate) fn files_authority_is_current(",
        "OBSERVATION_CURRENT": "pub(crate) fn pending_observation_is_current(",
        "PROCESS_CHANGED": "pub(crate) fn process_authority_changed_since_probe(",
        "STALE_REARM": "pub(crate) fn stale_probe_should_rearm(",
    }
    fragments = {key: item(follow, signature) for key, signature in signatures.items()}
    fragments["SYNC_COMMIT"] = item(sidebar, "pub fn commit_probed_location_listing(")
    fragments["BUMP_FOCUS"] = item(state, "pub(crate) fn bump_active_session_epoch(")
    update = item(app, "fn update_ssh_files_follow(")
    fragments["COMPLETION_LOOP"] = item(update, "while let Some(result) = self.ssh_files_follow.try_result() {")
    worker = item(follow, "|| -> std::io::Result<ProbeSnapshot> {")
    fragments["WORKER"] = "(" + worker + ")()"
    scan = item(sidebar, "pub fn poll_scan_results(")
    start = scan.index("if result.generation != self.scan_generation {")
    end = scan.index("let Some(node) = self.find_node_mut(&result.path) else {", start)
    fragments["MANUAL_ADMISSION"] = scan[start:end]
    for key, fragment in fragments.items():
        marker = "// @" + key + "@"
        assert template.count(marker) == 1, key
        template = template.replace(marker, fragment)
    with tempfile.TemporaryDirectory(prefix="ember-files-follow-") as directory:
        source = Path(directory) / "tests.rs"
        binary = Path(directory) / "tests"
        source.write_text(template)
        subprocess.run(["rustc", "--edition=2021", "--test", str(source), "-o", str(binary)], check=True)
        subprocess.run([str(binary), "--test-threads=1"], check=True)

if __name__ == "__main__":
    main()
