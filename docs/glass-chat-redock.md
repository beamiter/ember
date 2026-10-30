# Glass Chat redocking — 2026-10-30

Base: Ember `b662eaa` (includes the `fcc56fe` glass workspace and subsequent readability changes).

## Behavior

Single and split panes now compute the native PTY budget first, apply terminal
resize/reflow, then resolve the current presentation from the resulting prompt
extent. The same resolver sets the presentation rectangle, footer, and virtual
projection row count before projection, hit testing, cursor and IME painting.
Previously those decisions used the pre-resize prompt extent for the frame.

Chat keeps its requested font role during oversized-input fallback. Otherwise a
later font sync could drop the Chat control line-height floor, changing the PTY
row budget and the next docking decision. Switching explicitly to Block clears
that requested role before measuring fonts.

The existing glass palette, panel geometry, long-input cap, and intact native
fallback remain. The resolver does not write PTY bytes, move the terminal cursor,
clear composition, request focus, reset the session canvas, or invalidate the
whole render cache. History continues through the existing raw-anchor rebase.

## Regression coverage

- Same-frame resize through wide/narrow pane sizes and Chat/Block round trips.
- Long input fallback, repeated idle resolution, shortened-input redocking, and
  foreground command → prompt transitions.
- Native cells, cursor, focus ownership and composition text/cursor preservation.
- 128 consecutive fractional scroll events across row boundaries, checking the
  dock position and inverse pointer-to-native-cell mapping on every frame.
- Existing retained-canvas, history anchor and preedit/candidate tests.

## Native acceptance still required

The current executor could not start Xvfb: local/unix display socket creation
failed. No actual native window, hardware WGPU or OS IME acceptance is claimed.

The extended native smoke entry point is:

```sh
python3 scripts/reference-native-smoke.py \
  --binary target/debug/ember --fixture /absolute/path/to/output-fixture.sh \
  --out /absolute/path/to/new-results --dock-transitions --stream
```

Use an output-only shell fixture, an absolute fresh results directory, and an
X11 desktop test host with Xvfb, xdotool, xdpyinfo, ImageMagick and Pillow. The
new option types a long real readline command, changes window sizes and modes,
checks selected-mode paint and window focus, asserts no premature execution,
and compares the eventual PTY output byte-for-byte. It is an X11/Glow smoke
check, not proof of WGPU or an OS composition session. Screenshots record each
transition for visual review. This new path has not run successfully here.

For hardware acceptance, repeat under original WGPU with the user's input
method: begin Chinese preedit, resize and drag split dividers, toggle Chat/Block,
then commit/cancel; check candidate location, text committed exactly once and
focus ownership. Repeat while reading history and during continuous output;
verify the anchor and fractional scrolling, no full-page flash, and recovery
from oversized input after deleting text. Include inspector open/close and DPI
changes. Preserve shell resize/redraw output rather than manufacturing a dock.

## Validation results

Run with Rust/Cargo 1.99.1, locked Ember dependencies, and sibling
`jterm_core` at `b45cba5` (no shared-core changes):

| Check | Result |
| --- | --- |
| `cargo build --locked --bin ember` | Passed |
| `cargo test --locked --lib glass_` | 6 passed |
| `cargo test --locked --lib transcript_canvas_` | 14 passed |
| `cargo test --locked --lib preedit` | 5 passed |
| `cargo test --locked --lib ime` | 36 passed; substring filter also selects time-related tests |
| `python3 scripts/test-native-glass-profile.py` | 11 passed; synthetic observer checks only |
| `python3 scripts/test-projected-cursor.py` | 5 extracted-method tests and wiring checks passed |
| `python3 scripts/test-native-x11-display.py` | 3 orchestration tests passed; not a live display |
| `cargo fmt --all -- --check`, `git diff --check`, native-smoke Python compilation | Passed |

Filters overlap; do not add these counts as unique tests. The first long-input
fixture was below the actual font-height cap; it was corrected to 13 real rows
and the glass and canvas suites passed afterwards.

`python3 scripts/test-ime-ownership.py` could not run: its existing source
extractor expects `let private_title` inside `handle_ime_events` and raises
`ValueError: substring not found` on this baseline. No result from that harness
is claimed. The Cargo preedit/IME tests above ran actual Rust code instead.
Full all-target tests and Clippy were not run in this focused validation.

