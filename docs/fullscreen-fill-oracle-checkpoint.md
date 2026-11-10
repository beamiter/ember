# Fullscreen fill baseline (pre-extraction)

Exact source: Ember fc5bd956472223218ec06c9cb208287fb43b2fb6; published core a7c0bc9d308ae0bed9cc6d431fddf988b821ff8a. Independent detached clone, own target, official 1.99.1 toolchain, jobs=2, no dependency overrides. The prior completed target was copied only after stopping the cold build; prior worktrees and target were never modified.

Only production-file change is a cfg(test) module declaration. Original erase_screen and screen_alignment_test bodies are frozen in the independent test module and separately archived verbatim. Existing mode/reset/query/DECRQM/alternate sources and fixtures are unchanged. Source preservation is checked against every tracked file of the exact commit.

The seed derives the existing rich alternate/structural fixture and adds fixed sync clock, RGB/indexed BCE, pending wrap, REP character, origin/margins, saved cursor/drawing state, protected hyperlinked cells, row wraps, viewport offset, graphics on both screens, finished/running command ownership and primed caches. Eight seeds cover primary/alternate and sync off/on, ordinary 6×5, 1×5, and 1×1 grids. One-column seeds explicitly retain a truncated wide lead.

Capture executes real original production code, not copied reference bodies. Every seed and three repeated direct ED2/DECALN wrapper calls are captured. Parser corpus covers repeated fills, immediate REP/mixed print, alternate and sync transitions; controls cover ED0/1/3, selective ED2, invalid arguments, prefixes/intermediates/colon subparameters, ESC #3..6, and incomplete sequences. Whole feeds, every split, byte-wise feeds, and four bounded deterministic random partitions are individually snapshotted, with their own partition-sensitive revision state.

Capture is explicitly ignored/stdout-only. The separate parser exclusively creates raw-golden.json and rejects duplicate keys, failed runs and missing seed records. Lossless interning is used for the committed fixture. Ordinary tests never generate expected values. The direct differential compares copied original implementations against production, including all existing snapshot lanes and host state.

Validation and source-mutant results will be appended after completion. No architecture implementation or publication is part of this checkpoint.

## Final validation

- Production golden replay and 48 direct frozen-reference comparisons pass.
- 21 source mutants fail by runtime assertions, not compile errors: archive removal/reordering, inline-CLI hook, provenance invalidation, margins, wide/protected retention, both wrong background policies, ED2 cursor home / DECALN missing home, both wrap-latch retentions, inactive-grid alteration, row-identity revision, grid revision, both graphics hooks, DECALN reposition, and row-wrap metadata.
- Final terminal regression suite: 484 passed, 0 failed, 7 intentional ignored captures. Existing oracle and alternate/structural fixtures remain byte-identical.
- Formatting, Clippy --lib --tests with warnings denied, shared-core pin guard and host-bundle guard pass.
- Captured records: 4,128 = 8 seeds + 48 direct wrappers + 4,072 parser feeds across 1,432 partitions (21 streams).
- Packed golden: 605,241 bytes, SHA256 d484bee1faf4fdac2f364d766c4fff22139166467daabbc715760ddde63a73b7.
- Decoded canonical golden: 7,586,847 bytes, SHA256 89e854995933868bccf1f21e1fbe044c7f12cde96d9428d8493f2abc08542cd4.

The initial seed audit found inherited already-true provenance flags obscured the DECALN reposition hook. The final seeds explicitly start these false and recaptured the unchanged original production before any implementation. Both alternative-screen ED3 ownership guard states are represented through seeded inline-CLI ownership. Final source verification checks every tracked production file and both independently frozen function bodies. Capture and packing both require that guard. The earlier provisional capture is not the final fixture.

No production implementation changes, external writes, commits or publication were performed. This checkpoint is ready for independent review before engine extraction.
