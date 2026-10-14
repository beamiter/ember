#!/usr/bin/env python3
"""Run the configurable-control regression without building Ember's GUI.

Compiles current production command parsing/dispatch, session admission, and
OrganismHost feedback methods against the real pinned jterm_core dependency.
The fixture replaces shell, renderer, terminal-parser and host storage boundaries;
this is not a live PTY, egui event-loop, or rendered Block/Chat integration test.

By default Cargo builds only jterm_core using Ember's manifest and lockfile.
--core-rlib reuses a previously verified build, with its sibling dependency files.
--check-regression additionally removes the accepted-input notification from the
extracted method and requires the regression tests to fail.
"""
from pathlib import Path
import argparse
import json
import os
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def item(source, signature):
    """Extract one balanced production item; fail rather than silently skip it."""
    start = source.index(signature)
    opening = source.index("{", start)
    # Ignore strings/comments, including literal "ctrl+{" keybindings and
    # format strings. Character literals are recognized without eating lifetimes.
    tokens = re.compile(r"//[^\n]*|/\*.*?\*/|\"(?:\\.|[^\"\\])*\"|'(?:\\.|[^'\\])'|[{}]", re.DOTALL)
    depth = 1
    for token in tokens.finditer(source, opening + 1):
        if token[0] == "{":
            depth += 1
        elif token[0] == "}":
            depth -= 1
            if depth == 0:
                return source[start:token.end()]
    raise ValueError(f"unbalanced production item: {signature}")


def generate():
    source = {name: (ROOT / path).read_text() for name, path in {
        "input": "src/app/input.rs", "keys": "src/keybindings.rs",
        "session": "src/session.rs", "host": "src/organism_ui.rs",
        "organism": "src/organism.rs", "config": "src/config.rs",
    }.items()}
    specs = {
        "COMMAND": ("keys", "pub enum Command {"),
        "PARSE_COMMAND": ("keys", "impl std::str::FromStr for Command {"),
        "KEYS_NEW": ("keys", "pub fn new() -> Self {"),
        "KEYS_DEFAULT": ("keys", "pub fn default_bindings() -> Self {"),
        "KEYS_GET": ("keys", "pub fn get_command("),
        "KEYS_NORMALIZE": ("keys", "fn normalize_binding("),
        "MODE": ("config", "pub enum TerminalMode {"),
        "MOTION": ("config", "pub enum OrganismMotion {"),
        "EFFECTIVE_MODE": ("config", "pub fn effective_terminal_mode("),
        "CARDS_ENABLED": ("config", "pub fn command_cards_enabled("),
        "RESOLVED_MOTION": ("organism", "pub fn resolved_motion("),
        "POLICY": ("organism", "pub struct PresentationPolicy {"),
        "POLICY_IMPL": ("organism", "impl PresentationPolicy {"),
        "PURPOSE": ("session", "pub enum SessionPurpose {"),
        "APPEND_INPUT": ("session", "fn append_bounded_input("),
        "QUEUE_INPUT": ("session", "pub fn queue_input("),
        "READ_ONLY": ("input", "pub(crate) fn active_terminal_is_read_only("),
        "SELECTION_ADMISSION": ("input", "pub(crate) fn accepted_terminal_input_clears_block_selection("),
        "QUEUE_CONTROL": ("input", "fn queue_terminal_control_input("),
        "ACCEPTED_INPUT": ("host", "pub fn accepted_input("),
        "CANCEL_ATTENTION": ("host", "fn cancel_attention("),
        "REQUEST_HELLO": ("host", "fn request_hello_at("),
    }
    fragments = {key: item(source[name], signature) for key, (name, signature) in specs.items()}
    dispatch = item(source["input"], "pub(crate) fn dispatch_command(")
    arms = []
    for command in ("TerminalSendSigint", "TerminalSendEof", "TerminalClear"):
        pattern = rf"keybindings::Command::{command}\s*=>[^\n]+,"
        matches = re.findall(pattern, dispatch)
        assert len(matches) == 1, command
        arms.append(matches[0])
    arms.append(item(dispatch, "keybindings::Command::TerminalScrollUp => {"))
    fragments["DISPATCH_ARMS"] = "\n".join(arms)
    for name in ("INPUT_RETREAT", "FULL_FRAME_INTERVAL", "CALM_HEARTBEAT_INTERVAL"):
        fragments[name] = re.search(rf"pub const {name}: [^;]+;", source["organism"])[0]
    fragments["INPUT_CAP"] = re.search(r"pub const PENDING_INPUT_BYTE_CAP: [^;]+;", source["session"])[0]
    template = (ROOT / "scripts/control_input_harness.rs").read_text()
    for key, fragment in fragments.items():
        marker = "// @" + key + "@"
        assert template.count(marker) == 1, key
        template = template.replace(marker, fragment)
    assert not re.search(r"// @\w+@", template), "unexpanded fixture marker"
    return template, fragments["QUEUE_CONTROL"]


def core_rlib():
    build = subprocess.run([
        os.environ.get("CARGO", "cargo"), "build", "--locked", "--manifest-path",
        str(ROOT / "Cargo.toml"), "-p", "jterm_core", "--lib", "--message-format=json",
    ], stdout=subprocess.PIPE, text=True, check=True)
    artifacts = []
    for line in build.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message["target"]["name"] == "jterm_core":
            artifacts.extend(Path(name) for name in message["filenames"] if name.endswith(".rlib"))
    assert len(artifacts) == 1, "Cargo did not identify exactly one jterm_core rlib"
    return artifacts[0]


def run_test(directory, name, source, rlib):
    path, binary = directory / f"{name}.rs", directory / name
    path.write_text(source)
    dependency_dirs = [rlib.parent]
    # Cargo reports debug/libjterm_core.rlib; a manually supplied hashed rlib
    # usually lives in debug/deps. Both need the transitive dependency directory.
    if (rlib.parent / "deps").is_dir():
        dependency_dirs.append(rlib.parent / "deps")
    search_paths = [arg for path in dependency_dirs for arg in ("-L", f"dependency={path}")]
    subprocess.run([
        os.environ.get("RUSTC", "rustc"), "--edition=2021", "--test", str(path),
        "--extern", f"jterm_core={rlib}", *search_paths,
        "-o", str(binary),
    ], check=True)
    return subprocess.run([str(binary), "--test-threads=1"]).returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core-rlib", type=Path, help="verified pinned jterm_core artifact; skips Cargo")
    parser.add_argument("--check-regression", action="store_true")
    args = parser.parse_args()
    template, queue_control = generate()
    rlib = (args.core_rlib or core_rlib()).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="ember-control-input-") as directory:
        directory = Path(directory)
        if run_test(directory, "control_input", template, rlib):
            raise SystemExit(1)
        if args.check_regression:
            notification = "self.organism.accepted_input(&session_id);"
            assert queue_control.count(notification) == 1, "expected one accepted-input notification"
            regressed = template.replace(queue_control, queue_control.replace(notification, ""), 1)
            if run_test(directory, "missing_feedback", regressed, rlib) == 0:
                raise SystemExit("ERROR: the missing-feedback mutation was not detected")
            print("Regression check passed: removing the accepted-input notification fails the tests.")


if __name__ == "__main__":
    main()
