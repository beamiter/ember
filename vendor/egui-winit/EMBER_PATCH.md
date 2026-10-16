# Ember keyboard fallback patch

Upstream egui-winit 0.36.1, crates.io SHA256
9327fc8edef2c57db9bcbcacc82c8a4b1e8cc64cd41a67db4419dcf643d88c83,
upstream commit 4c1f2fae95475a40e524884ebb298bcb1714b08e.
UPSTREAM_INTEGRITY.json lists the original archive's file hashes. Root licenses
are retained from that same upstream commit already vendored with eframe.

Only src/lib.rs is behaviorally changed. Keyboard resolution now refuses a
control/navigation/function physical-key fallback when the logical key is a
known printable Character that egui cannot name. Such text keeps its normal Text
event. Physical printable keys remain available for non-Latin shortcut layouts;
mapped logical keys, Named Enter, dead/unidentified keys, and IME flow remain
unchanged. Modifier state does not weaken this restriction. No app-side event
cancellation, production test mode, terminal guard, or execution bypass is added.

Observed regression: the hosted Wayland run executed a Bash command prefix
before its intended redirection. A source-supported diagnosis is that a logical
`>` mapped to physical Enter triggers the old fallback, emitting Enter then `>`.
The captured screenshot and shell trace establish premature execution; native
key tracing is still needed to confirm that exact logical/physical mapping. The native desktop
script retains the original redirection command and adds a raw PTY-byte oracle.

Run dependency regressions explicitly with:
cargo test -p egui-winit --lib ember_keyboard_fallback_tests --locked
