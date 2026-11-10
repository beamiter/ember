# Shared terminal engine checkpoint

Ember now resolves `jterm_engine` and `jterm_core` from the same immutable
published Git revision, 0dd2563b7a0387649cffd1cdbaef4374b860f650. Normal builds
need no sibling checkout. `scripts/check-shared-core-pin.py` validates both
packages, the lockfile, and the absence of checked-in source overrides.

The engine owns live cell storage, the incremental parser, protocol input state,
local VT semantic dispatch, physical resize, and structural screen primitives.
Ember preserves its existing host effects, history/graphics policies and GUI
adapter. Worker wake requests use the shared `WakeSink` through an egui adapter.
This is an incremental migration, not completion of the four-terminal runtime.
Alternate-screen orchestration, immutable frame publication to the renderer,
RIS and remaining host effects are still frontend-owned in this checkpoint.

The recovered migration fixtures retain their original production baselines:
916 parser cases, 3,160 semantic prefix/final cases, 106 structural cases and
305 physical resize cases. These are checked-in inputs, not regenerated from
the replacement engine. Existing wide-character URL/OSC8 fixes are retained.
The fixture provenance documents describe capture and scope boundaries.

## Recovery validation

The restored candidate was checked against the current published core, rather
than restoring the older standalone sibling engine from the archive.
`cargo check --all-targets`, formatting, source-pin and frozen host-bundle checks
pass. Cursor-projection extraction tests and their negative control pass after
updating the fixture to the new protocol-state owner. Native CI Python fixture
checks pass except for the system-motion private D-Bus fixture, which cannot
open a bus socket in the recovery sandbox. This is not native GUI acceptance.

The optional control-input extraction harness has pre-existing stale motion and
host-feedback stubs; its compile failure is not a full application compile
failure. Its relevant production methods and fixture were not changed here.

The recovered source also passes warnings-denied all-target Clippy, all 466
terminal tests (two capture printers ignored), and all 132 UI tests. The full
library run reports 1,441 passed, five ignored, and three failures in unchanged
trusted-Git tests because the sandbox root is not system-owned. Those security
checks were not weakened. Native GUI and exact-commit CI remain separate gates.
The optional raw-search extraction harness passes with a coherent cached
dependency set after its engine and module wiring was updated. Its existing
newest-artifact selector can mix incompatible Serde builds in a reused target
directory; use coherent artifacts or the full Cargo suite instead.
The extracted block-history/controller harness also passes 629 tests with two
capture printers ignored using that coherent dependency set.

## Lossless semantic-fixture transport

The semantic oracle is stored using the published core's `jterm-oracle-intern-v1`
format to stay within transport request limits. The original 17,995,503 bytes
round-trip exactly with SHA-256
`b21076900fb0ad83b76ae80e613150f1a0990dccce254dfb278f7bb40a3c1763`.
The packed fixture is 120,852 bytes. No terminal code was run to regenerate
expected values. `scripts/intern-oracle-fixture.py unpack INPUT NEW_OUTPUT`
independently checks the original length and SHA-256. The test-only decoder
supports the original arrays and the packed representation, with explicit
invalid-reference, invalid-tag and non-object-patch rejection tests.

## Reconstructed alternate-screen adapter

The production adapter delegates physical alternate entry/exit to the pinned
engine's `ScreenState::enter_alternate_screen` and `leave_alternate_screen`.
Selection, graphics/keyboard screen switching, watch boundaries, search seals,
working directory, provenance invalidation, dirty rows, revisions and global
background remain host effects. RIS remains host-owned. Engine entry/exit
finishes screen mutations before host hooks; the retained hooks only mutate
their own state/revision/cache records and do not read intermediate screen
fields. Clear effects are consumed after graphics/keyboard/watch switching.

`alternate_parity_tests.rs` preserves the pre-migration Ember transition and
clear algorithms as independent test-only controls. It compares 422 deterministic
snapshots and 12,000 generated transitions, with drawing, DECSC/DECRC, keyboard,
hyperlink, margins and RIS input interleaved. Snapshots cover both grids/IDs/wraps,
saved and live drawing state, selection, populated graphics/provenance, keyboard
stacks, watch, cache and dirty/search revisions. Seal epoch changes are compared
without equating process-global epoch identifiers. These are reconstructed
controls, not a recovered copy of the historical lost golden snapshot artifact.
