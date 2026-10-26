# Bounded Block search: first real-app gate

## Current observer: bounded-pixel-v2

The default command and the compatibility `--observer-pilot` command now use the
same strict pixel navigation observer. This explicitly replaces the old numeric
`ROW000017`/query-token OCR check described in the historical specification
below; it does not turn any historical failed result into a pass. Saved native
screenshots showed correct navigation while OCR read zeros as letters and
misread the leading token beside the selection stripe.

Both commands emit a fixed 97-byte marker variant: the queried
row 16 contains TARGET PAPAYA (raw) or TARGET WALNUT (snapshot), and the adjacent
unqueried row 17 contains NEIGHBOR with the same suffix. Flush row 248
contains LATEST CEDAR, seven rows above the end so card borders and following
prompts cannot crowd its OCR baseline. Marker rows use spaces in place of
trailing dot leaders so sparse-text OCR does not merge the control words into
dense filler; all other rows retain their
original bytes. Source byte counts, retention/search budgets, queries,
exact public result identity, stopped/restarted raw flows and one pointer
activation remain unchanged. Output manifests still verify the entire exact
variant stream, not just the markers.

Before opening search, the observer waits up to 30 seconds for a fresh screenshot
to positively show LATEST CEDAR with no target/neighbor. A generator manifest and
shell sentinel establish execution, but UI ingestion/painting of 100000 queued
rows can still lag; the old single baseline screenshot raced that queue. Only
observations repeat. Search input and pointer activation never repeat. An owned
picker still closing after the click is likewise observed until closed within
the existing final deadline; foreign PID/owner/tree failures remain fatal.
Raw completion uses the existing exact owned AT-SPI status predicate (with fresh
screenshot bracketing), since its long progress label can wrap across OCR lines.
Its exact grammar includes the current operation progress suffix; the old
adapter only matched the prefix and could never recognize production completion.
This never replaces the independent final navigation pixel proof.
Snapshot result identity omits only blank trailing terminal cells, while the
emitter manifest continues to hash every emitted padding byte.

Final proof requires the exact target and adjacent unqueried neighbor in one
newly captured full screenshot after activation, with the original spatial,
ownership, timestamp, negative-live-marker and no-open-picker controls. OCR
remains unscaled PSM11 with no character substitution, case repair or alternate
parameter retry. The ambiguous legacy row/token OCR is retained diagnostically
in events. Wrong page, missing/duplicate marker, wrong neighbor, stale capture,
wrong baseline ownership and still-open picker cannot satisfy this gate. Pure
regressions include delayed live paint and failure to establish a live baseline.
The old fixture bytes remain tested for provenance; both live entry points now
report `fixture_version: bounded-pixel-v2`. Full E03 exclusions still apply.

## Historical first-stage specification (superseded observer)

This first fixture stage launches the installed, uninstrumented production Ember
binary through the existing scale-one X11 desktop harness and drives the real
Block Search picker through its actual Ctrl+Shift+G binding. Before typing,
exact unique Cmd, Out and Latest controls must coexist in one observation. The
generic terminal Find bar (Ctrl+Shift+F) cannot satisfy that surface check. Query
clipboard readback remains required. An alternate OCR layout may analyze the
same screenshot for exact query text; it never retries input or uses fuzzy text.
It does not invoke application/controller functions. The E11 narrow
Wayland/IME handshake is not a prerequisite or imported dependency.

Run `python3 scripts/test-native-block-search.py` for pure fixture checks. On an
authorized native host with the dependencies of native-desktop-smoke.py, run:

```
python3 scripts/native-block-search.py --binary /path/to/ember --backend x11 --scenario raw --output /new/raw-artifacts
python3 scripts/native-block-search.py --binary /path/to/ember --backend x11 --scenario snapshot --output /new/snapshot-artifacts
```

Each scenario uses an isolated HOME, a fresh app process, public OSC 133 bash
integration and actual foreground PTY output. No private shell guards or app
search budgets are modified. Scrollback is configured to 160000 retained lines.
Output generation requires a real PTY with at least 100 columns and emits fixed
96-character lines in batches no larger than 128 rows. Exclusive manifests record
exact bytes, lines, SHA256, shell/TTY identity and monotonic generator timestamps.
The driver recomputes the expected bounded stream digest and compares all manifest
counts/mode/record and shell PID/TTY against its initial real-shell identity.
A separate real shell sentinel confirms each generator returned.

The raw scenario emits 100000 lines (9700000 bytes) in one command, then flushes
its endpoint into scrollback. This is genuinely truncated by the existing 256 KiB
per-command capture limit. Actual Find field copy/readback proves keyboard input
ownership. The driver chooses Output scope, starts the visible retained-output
action, attempts Return while loading, requires the exact stable raw-hit identity and Stop control in the same
observation before Return, then verifies that identity remains while loading. It
stops and pointer-activates that same hit after a fresh stopped-state observation
as the positive control. A separate explicit start/Stop/Restart flow then waits
for completed-record status and pointer-activates the early row again. The final viewport must contain both its exact unique token and an
adjacent unqueried row after the picker has closed. Merely finding the token in
the query or result row is not successful final navigation.

The snapshot scenario emits 64 separately completed commands, 2000 lines each
(194000 bytes per record). The total 12416000 bytes is above the 8 MiB newest
search-source window and below the real 16 MiB captured-output retention cap.
It searches for the earliest record through the real Earlier history action and
requires the same final viewport proof. It does not replace complete snapshots
with raw scanning.

Every screenshot and OCR table is retained. Phrase actions require exact unique
contiguous words on a single OCR line. Ambiguity, missing controls, a naturally
completed scan before the intended Stop boundary, or unreadable OCR fail closed;
there is no coordinate fallback or action retry. Native observations are slower
than compositor turns and do not deterministically force every race boundary.
The existing actual-source adversarial tests remain separate evidence.

The sampled app PID RSS/HWM/thread series is descriptive only. HWM spans the
whole process lifetime, RSS includes terminal/graphics/allocator state, and the
10 ms sampler can miss short peaks. No allocation budget or input p95 claim is
made from this fixture. Input helpers intentionally include setup delays, and
OCR timestamps are observation timestamps, not event/presentation latency.

This is an initial correctness gate, not full E03 acceptance. Still required:
final keyboard/AccessKit activation; natural-limit Continue
and the 4096-byte/hit-cap cases; pane/query/filter/scope/Latest/close/reset/resize/
eviction/alternate-screen races; continuous PTY contention; all replacement-path
peak accounting; and native input/presentation measurements. Result JSON lists
these exclusions even on success. No native pass exists until this exact built
binary runs on a supported host, and no publication/completion is implied.

The surface ownership check accepts only ASCII case variation for the four
Scope/Cmd/Out/Latest control labels, as observed in the saved real X11 pixels.
Each label must be unique; Scope/Cmd/Out must form the ordered same-height row
and Latest must be below it within its horizontal span. Generic Find and
misplaced or duplicate labels fail. Query, result and stable-anchor text remain
exact and case-sensitive; this is not fuzzy text matching or a coordinate click.

Out scope targeting uses a fresh full-screen observation and the same complete
surface validator. It clicks the center of that observation's unique normalized
Out rectangle once, with no cached or guessed coordinates. Control rectangles
must be nonempty and inside the fixture's fixed 1280x900 full-screen capture.
Invalid or duplicate surface controls produce no click. Other query/result
clicks retain their exact text matcher.
