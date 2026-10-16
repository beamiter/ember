# Bounded Block search: first real-app gate

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
