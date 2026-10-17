# Headless font-group regression suite

Run from the repository root with a Rust toolchain on PATH:

```sh
python3 font-group-harness/run.py
```

This small package builds egui 0.36.1, serde/TOML and filesystem helpers only.
It never builds eframe/wgpu, launches Ember, opens a display, creates a Unix
socket, or uses DBus/Xvfb. Cargo.lock retains the repository's relevant pinned
versions. CARGO_HOME and CARGO_TARGET_DIR can point to shared caches.

## What is exercised

- The actual src/font_zoom.rs router, including real headless egui Panels,
  ScrollArea, keyboard focus, floating overlays (including their first sizing
  frame), pointer event ordering, sidebar visibility/resize, shared edges,
  split-content ownership, smooth wheel magnitude and absence of global zoom
  or wheel scroll after consumption.
- The entire production config.rs and persistence_file.rs, restricted to font
  tests: legacy migration, independent finite/clamped values, serialization and
  real atomic save/reload of both groups.
- Exact-extracted Settings state, apply/sync/adopt methods and regression tests.
- The exact production handle_font_zoom consumer, with renderer rebuild and
  delayed-save scheduling replaced by counters to prove which branch runs.
- Static assertions for the actual raw-input/layout/style/renderer wiring.

The generator records SHA256 hashes for each exact source extract in
extraction-manifest.json. Regenerate before running tests so stale copied code
cannot be mistaken for the current implementation.

## Limits

Unrelated shared-core remote grammar/provider/security helpers and theme/font
metadata are explicitly stubbed only to compile the production config methods.
Do not run all config_harness tests: unrelated tests included from those full
production files are outside this fixture's scope. run.py selects only the
font-group tests. Actual renderer rebuilding and debounced save scheduling are
counted, not executed. This is not a full Ember application build, native GUI
visual validation, PTY-resize test, or desktop input-method test.
