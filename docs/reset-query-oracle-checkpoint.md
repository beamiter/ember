# Pre-extraction DECSTR / DECRQSS oracle checkpoint

This patch adds tests only. Production reset and query semantics are untouched.
It prepares the next local-dispatch slice, not an implemented engine migration.

## Provenance

Original candidate: Ember HEAD 025d157631f33c0dc367f75117d0d847055731d7,
staged tree cdee016cf2383b813c8520342be9a1e996f0947f. That exact tree was
subsequently published as 708099ed024e90de80cd7a61a9e364317496c290.
Both shared dependency pins remain a6e47726d02bed2ca4a6ba1a682511f970853527.
Source SHA-256 values are in tests/fixtures/reset_query_provenance.json.

Four original Ember implementations are copied into a test-only module with
frozen_ names. They must not be rewritten to call a future engine implementation.
The committed golden is captured from the original live production parser,
not from those helper copies. Capture is ignored during ordinary test runs.

## Coverage and limits

- Four seeded combinations of primary/alternate buffer and sync-output state.
- 44 full multi-lane snapshots: four seeds plus ten commands for each seed.
- 6,816 complete input partitions: every subset of byte boundaries for the
  bounded corpus, including reset followed by SGR query in the same stream.
- Fragmented feeds have existing search_source_revision differences: 6,776
  partition-specific overrides preserve the exact original counter. No lane is
  dropped to pretend whole-input and fragmented state are identical. All other
  captured lanes match their whole-input counterpart.
- Drawing state, active/hidden cells, row IDs/wraps, cursor/saved cursor,
  margins, history/provenance, dirty revisions, caches, selection, protocol
  (including active/inactive keyboard stacks), watch, replies, sync state,
  active Kitty placements and image count, and unrelated host fields retained.
- Process-global epochs and image revisions, wall-clock instants, and opaque
  implementation details are not serialized. Exact sync-start preservation is
  checked directly per terminal; separate controlled-clock tests cover timeout
  at 1000/1001 ms and explicit end, on both primary and alternate screens.
- Exact SGR ordering/terminators: default, all represented style bits,
  underline variants, named/bright/indexed/RGB colors; all supported DECRQSS
  selectors, empty/unknown selectors, and non-$q DCS no-op.
- Negative controls check cursor, grid, modes 1/66/1007, global background,
  sync state, reply termination and failure response corruption.

These are semantic/state snapshots, not a serialization of every field in
TerminalState and not a new parser. The tests run the existing production path.
Unknown DCS, RIS, ordinary OSC, graphics, clipboard, device identity and host
mode hooks are outside the migration. Graphics/host fields appear only as
retention sentinels. The existing alternate seed initializes graphics and
command provenance so unintended destructive effects become observable.

## Frost is not Ember

The observed Frost source at 1f69facea3b6c1cb1fcdd56e37ff6605e36f2486 is
hashed and its soft_reset is frozen verbatim in frost_soft_reset_frozen.txt.
Frost clears mode bits 6/4 and enables 25/7. It does not touch 1, 66 or 1007.
Ember stores origin/insert separately, removes 1/66 and restores 1007.
The explicit policy-contrast test must remain: Frost cannot inherit Ember's
DECSTR implementation without a separate adaptation and parity review.
This package does not alter Frost or claim an actual Frost production test run.

## Use for the next slice

Run the golden and frozen-reference tests unchanged before and after moving
Ember CSI ! p / DCS $q into dispatch_local. Apply ordered reply and global
background effects immediately. Unhandled events remain transactional. Do not
regenerate goldens after the move. Preserve consumer pins until publication and
consumer verification of the new shared revision are complete.
