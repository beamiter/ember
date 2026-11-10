# Pre-extraction live mode mutation checkpoint

This milestone changes test infrastructure only. Production functions retain
exact original bodies; the parser adds only a cfg(test) module declaration.
Nothing is published by this checkpoint.

## Exact provenance

Ember master commit `9ca7891d58939a368c4e62ee9dd9ad338d944731`, tree
`d4430bb4bd0c84e5cba3ffd7efc60ca48e4aac65`; jterm_core dependency
`598f15808086dbea14d56fc1b86e77b6cb2e3201`. A fresh isolated clone of the
retained base plus verified published files reproduced that exact Git tree
before tests were added. Source hashes are recorded in the provenance fixture.

## Boundary for subsequent implementation

The prospective engine operation is a **single-mode** pure mutation called by
Ember's existing ordered parameter loop. Retain the outer parser dispatch and
its observer cadence. Do not normalize empty parameters, duplicate parameters,
private prefixes, colon subparameters, intermediates, unknown values, or order.

Host hooks remain in Ember:
- 1004: focus mode plus immediate reply only on newly enabled, focused window.
- 47/1047/1049: alternate transition, grids, graphics, watch and cursor ownership.
- 1048: save/restore the full DECSC cursor slot, distinct from 1049 primary slot.
- 2026: controlled clock start/restart, archives, snapshots and dirty state.
- 5522 reset: immediate revocation of the pending paste capability.

Private 6 changes origin and cursor without clearing pending wrap in Ember.
Private 4 passes through fallback insertion, while ANSI 4 changes insert state.
TerminalModes only stores recognized bit slots: private 4 and truly unknown
values remain absent. Preserve fallback insertion for recognized modes such as
1, 12, 1005, 1015, 2031, 1034, 1036 and 5522; do not replace it with a no-op.
Frost's contrasting behavior must not be inherited by this Ember extraction.

## Oracle design

All four original mutation functions are copied in the new test-only module.
These references must remain independent of a future engine implementation.
The golden is captured from the genuine production parser, not from the copies.
The ignored stdout-only capture test never updates expectations automatically.
The capture parser exclusively creates its destination and rejects duplicate,
invalid or missing records and failed test runs. Existing lossless interning
stores exact lane strings; ordinary tests read the fixture and compare every
lane and record count.

Four seeds combine primary/alternate screens and sync output on/off, with both
focused/unfocused windows. Existing primary/hidden grids, graphics placements,
keyboard stacks, saved cursors, margins, wrap, command provenance, selection,
watch, caches, dirty revisions, clipboard and unrelated host state are retained.
Controlled-clock age and paste-grant expiry are recorded as relative durations;
process-global identities and wall-clock instants are intentionally not encoded.

The corpus contains whole inputs, every single byte-boundary split and
byte-at-a-time feeds. Every feed is snapshotted, including incomplete sequences.
Short-corpus partitions are bounded rather than exhaustive powersets. The direct
reference differential separately covers 28 mode values × set/reset ×
ANSI/private × four seeds. Future engine testing should add exhaustive all-u16
pure-state differential checks without making the host runtime corpus enormous.

Existing DECRQM/reset/query frozen modules, corpora and goldens are unmodified.

## Capture and validation results

The final 44-stream corpus produces 2,492 seeded partitions and 6,776 complete
per-feed/seed snapshots. Unchanged production replay and all 448 direct
frozen-reference combinations pass. Lossless interning stores the capture in
697,809 bytes; packed fixture SHA-256 is
`3862037fedb5a5e8c4c5607df6f7c8335d197ce3192ef5921f6042113e6a18b8`.
Decoded canonical JSON SHA-256 is
`dd5f7aaf1ab5b27d093491e4cc79cc303b78c6ff14cab687cb80e734b1a46c27`.
The full raw production capture is included with the checkpoint logs.

Negative controls mutate actual production function/dispatch source one at a
time, build and run the golden test, then restore the original source. They do
not alter expected snapshots or the frozen copies. Assertion failures are
recorded separately from compiler failures. The `drop_unknown` mutant removes
the fallback insertion arm; its observed failure is loss of stored mode 1.
Truly unknown mode numbers have no slot and are not claimed to be persisted.

The eight controls cover DECOM wrap clearing, private/ANSI 4 conflation,
reversed parameter order, lost fallback storage, broken sync activation,
lost paste revocation, duplicated focus reply and missing sync clock restart.
See `mutant-results.json` and individual logs for their verified outcomes.

Limit: snapshots detect observable observer effects, not a hypothetical extra
idempotent callback with no state effect. Retaining the existing outer host
dispatch is required; this checkpoint does not instrument callback counts.

After source restoration, terminal tests, formatting, and strict Clippy
`--lib --tests` are the validation gates. This is not a full-application or
all-target CI claim. No remote push occurs in this baseline-only checkpoint.
