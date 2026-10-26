# E03 independent pixel-observer pilot (historical preregistration)

This frozen experiment is superseded by `bounded-pixel-v2`, documented in
[native-block-search-gate.md](native-block-search-gate.md). The marker words and
strict pixel predicates are retained; v2 uses quiet space padding after marker
words within the unchanged 97-byte row budget. Both live entry points now use that
observer, with bounded positive-live-paint waiting to fix the raw ingestion race.
The historical failed verdicts and preregistration below remain unchanged.

Base: e465d0e13a2319eab2a5706c5cd203563f00a7ce. Freeze this specification and code
before the first hosted run. This is an experiment, not a changed E03 gate.

## Evidence and hypothesis

Run 39020060189 / artifact 12096976614, archive SHA256
1bcb17f58857266063a6f5395b0b89e2af2d56e50f820f80927ac805c98c419a,
failed the unchanged ROW000017 final OCR check in raw and snapshot. Its failed
body trees each have 71 nodes / 48 submitted raw rows, including exact target
row 16 and adjacent rows 17/18. Row 17 bounds are [50,174,733,14]; its identities
are raw row=20/revision=356/frame=298 and snapshot row=21/revision=2552/frame=2666.
Each before/after PNG pair is byte-identical. These are renderer semantics plus
stable pixel evidence, not an accessibility substitute for a pixel oracle.

The saved OCR also misreads the target token itself: raw IEO3EARLYHIT and snapshot
IEO3SNAPSHOTOO; neighbors include ROvO00017 / ROWO00017. Thus the hypothesis is
not just repeated-zero ambiguity. The left-edge selection stripe is close to the
first glyphs; its contribution is a hypothesis, not a proved rendering defect.
The experiment places independent markers away from that edge while retaining
all original tokens and numbers. No production rendering change is proposed.

## Frozen data and method

- Raw remains 100000 rows / 9700000 bytes. Snapshot remains 64 x 2000 rows /
  12416000 bytes. Every line remains 96 ASCII columns plus newline; emitter
  batching remains at most 128 rows. Original source/window/retention limits,
  native 1200x800 app, 1280x900 capture and font configuration remain unchanged.
- Only in the explicitly selected pilot, record zero at row 16 gets TARGET PAPAYA
  (raw) or TARGET WALNUT (snapshot), starting at zero-based column 36. Row 17 gets
  NEIGHBOR PAPAYA or NEIGHBOR WALNUT at the same column. Each marker is followed by four spaces. Padding dots are replaced,
  so no line or byte budget grows. Pilot flush record zero row 255 gets the fixed
  live-endpoint negative-control marker LATEST CEDAR at column 36, also replacing
  padding with four spaces after the marker. Flush remains 256 rows / 24832 bytes.
  All other records/rows are byte-identical. This third marker was fixed before
  the first hosted run; it is not a response to a pilot OCR failure.
- Search still queries the original E03EARLYHIT / E03SNAPSHOT00. Public result
  identity and one-time pointer activation remain required. The unqueried
  neighbor marker is absent from the selected hit snippet.
- Use existing unscaled PSM11 OCR with exact case-sensitive contiguous words on
  one OCR line. No normalization, character repair, cropping, OCR parameter
  retries, marker revisions or success-driven font changes are allowed.
- Before EACH search, a new full screenshot must positively identify the unique
  LATEST CEDAR phrase and have no TARGET or NEIGHBOR word. This live baseline is
  bound to query, mode, current app PID and window, then consumed by exactly one
  activation. Raw's second navigation must acquire its own baseline. Immediately
  before clicking, another fresh screenshot must lack NEIGHBOR and begin after
  the baseline capture ended. Final pixels must lack both LATEST and CEDAR.
- Every phrase requires positive word rectangles, vertical overlap at least half
  the larger word height, and a nonnegative word gap no greater than that height.
  Sharing an OCR line ID alone cannot join distant or misaligned words.
- A success requires exactly one target phrase and one neighbor phrase from ONE
  newly captured full PNG. Neighbor top must be below target bottom; top-to-top
  distance must not exceed twice the SMALLER phrase-box height, and left origins
  must differ by no more than that height. The larger phrase height must be no
  more than twice the smaller one; anomalous tall boxes cannot expand distance. No intervening OCR baseline or duplicate
  OCR observation is accepted. These scale-relative spatial bounds are fixed now.
- The observation budget stays 30 seconds; there is no retry of activation.
  A PID/bus-owned connected public tree brackets the screenshot to reject an
  open picker/editor. Node body names/descriptions are never read as proof of
  target/neighbor success. The owned app/window must remain alive and unchanged.
- Capture begins strictly after activation. The output directory is new; every
  sequence-specific PNG/TSV path must be absent before capture. Capture start/end,
  activation ID and PNG/TSV hashes are retained. An older path, capture timestamp,
  or activation ID cannot be reused. Identical pixel bytes from two genuine fresh
  captures are allowed: static pixels do not imply a stale capture. This trusts
  the existing native screenshot command, not an arbitrary supplied image.

## Negative controls and regression requirements

Pure tests must reject: picker-only target snippet; a public tree that contains
both marker strings but no matching pixels; wrong neighbor; missing/unnavigated
viewport; duplicated markers; same-line or two-row-separated markers; intervening
OCR baseline; changed case/characters; foreign tree owner/PID; still-open editor;
old activation timestamp; old capture timestamp; reused PNG path; distant words
sharing a line ID; inflated word/phrase height; absent or target-contaminated live
baseline; wrong query/mode/PID/window baseline; reused baseline; final pixels
still showing the live marker. Positive controls
cover both predeclared marker pairs and exact generation/variant manifests.

The positive live marker establishes a different rendered body location before
search; missing target text alone would not establish that, because OCR may omit
text. OCR absence remains a fallible additional exclusion, not independent proof
of causation. The pilot depends on honest fresh native screenshot capture and
owned-window checks; it does not prove compositor-wide occlusion or arbitrary
capture-provider integrity. No accessibility body string supplies pixel success.

These are adversarial observer-unit tests, not fabricated native passes. Hosted
runs must exercise real emitted PTY data and the installed production binary.

## Hosted execution and decision rule

Keep the original `Gate real X11 bounded block-search correctness` command and
verdict unchanged. Run a separate `Independent E03 pixel observer pilot` step
on the same exact-head installed release, both raw and snapshot, with separate
fresh processes/HOMEs and output directories. Save every screenshot/TSV/tree and
result under `block-search-observer-pilot-{raw,snapshot}`. A failed original gate
must not skip the experiment; cancellation or a missing installed binary does.

Both pilot scenarios must pass their existing public interaction flow and all
new pixel predicates. Raw must complete both stopped-hit and restarted-complete
navigation, not just its first visible marker. Report each original/pilot result
separately. If the fixed pilot fails, preserve evidence and diagnose it; do not
iterate words, OCR parameters or geometry limits until something passes.

If both pass with the negative controls intact, a separate review can decide
whether to replace the ambiguous final numeric/token OCR fixture with these
same-strength deterministic pixel markers. That would be an explicit test-oracle
change, not silently relabeling the old failed run. The production search window
implementation is not changed by this experiment.

Even a future promoted first-stage native gate is not full E03 acceptance:
resident/peak memory accounting, cancellation/race/continuation coverage and
fixed-machine interaction p95 <100ms remain open. E11 partial results are unrelated.
