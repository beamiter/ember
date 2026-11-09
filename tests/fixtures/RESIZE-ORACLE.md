# Physical resize baseline

`resize_before_engine.json` records 305 cases from actual Ember `on_resize`
before physical resizing moved to `ScreenState::resize_geometry`. It was captured
and replayed against the unchanged original implementation before integration.

Source SHA-256:

- `state.rs`: `56f1829fe8641cf543a7398f024087bfe0e408bb97e1017a384e972b7c271cea`
- `parser.rs`: `6841a4102e291c617017c3436252c19ed41f9f7b8dc0b9f1929130971930c95d`

The test compares exact values, not hashes. Identical snapshot lane strings are
interned. Cell rows use lossless run-length encoding; wide and continuation flags
are explicit because `StyleFlags` diagnostic formatting omits them. Snapshots
include both real grids, row IDs/wraps, pen/cursors/saved states, history, selection,
provenance, revision and cache lanes, Kitty placements, and lazily materialized
visible/projected cells. Nondeterministic clocks and process-global epochs/image
revisions are excluded.

Cases cover zero/same/clamped dimensions, one-cell grids, growth/shrink, colored
active versus default hidden blanks, wide edges (including height-only repair),
custom tabs/margins, DECSC versus saved-primary retention, blank-tail priority,
primary/alternate/synchronized/inline-CLI archive policy, capped history and
pinned/reversed selections, exhausted IDs, completed/in-flight output provenance,
Kitty placements, resize inside 47/1047/1049 then exit, and repeated resize.

The standalone engine test additionally retains the original physical algorithm
and compares 10,125 matrix cases, including allocation order/exhaustion. This is a
geometry extraction, not reflow, alternate-transition, reset, compression or
terminal-semantics change.

`print_resize_baseline` is ignored **capture tooling**, not an ignored acceptance
test. It only prints records; it never overwrites this fixture. Do not regenerate
expected values from a changed implementation. Existing parser, semantic and
structural baselines remain independent and unchanged.
