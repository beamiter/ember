# Fixed paired cumulative startup regression plan

This is a review candidate. Publishing is not authorization to dispatch.
The workflow is dispatch-only and produces no shipped app change.

## Immutable inputs and endpoint

The manifest fixes baseline `1c43338c0292f40fd294cb89d243bae356d64965`, candidate
`9607c1cd4c4f623ed383a7f1a7d7ebb07a22b5e7`, and desktop-driver source
`e4c292f10463e52fe7e74e808ca10fc4ab27729c`. Control is the future reviewed workflow
commit (`github.sha`), distinct from the measured candidate; record and review
its full SHA before dispatch. Never report a floating branch as that identity.

The baseline-to-current-candidate comparison includes 29 commits. It measures
cumulative startup regression, not E09-only causal overhead. The historical
E09 candidate was `60e70c4a...`; its unsuccessful full-startup gate remains so.
A direct-work pass cannot substitute for that original startup requirement.

The unchanged temporary overlay starts at the first statement of main and ends
in the first post_rendering branch BEFORE window.set_visible(true). This excludes
OS process launch, compositor scanout, actual visible-window readiness and
shell-prompt readiness. The diagnostic for exactly 64 restored sessions must
precede the marker; observe 64 live Bash processes on 64 distinct PTYs. Screenshots
are correctness evidence, never a clock. Source/lockfile/overlay/binary identities
and all raw results must be retained. Neither temporary binary is published.

## Sampling, statistics and stopping rules

Each backend runs on a separate hosted job. On that same runner, compare both
binaries using exactly five warmup pairs then 240 measured adjacent pairs.
Alternate AB/BA: measured order counts are 120 each. Use the same synthetic
version-4 fixture of 64 sessions with an existing cwd, normal mode and Bash.
Do not use real user session files. Do not pool X11 and Wayland results.

For each pair, delta = candidate minus baseline. Both the upper endpoint of the
fixed-seed (0), 10,000-resample two-sided 95% bootstrap CI for mean paired delta
AND nearest-rank paired-delta p95 must be strictly below 50ms. Exactly 50 fails.
This is not p95(candidate) minus p95(baseline). Both backends must pass.

Any identity/hash mismatch, dirty checkout, unequal lockfile or overlay dry-run
failure stops before sampling. Any app launch, marker, PTY, screenshot, liveness
or cleanup failure stops that backend, including in warmup. Failed samples are
not retried or discarded. Cancellation, timeout or fewer than 240 complete pairs
is incomplete evidence, never a pass. A failed gate remains
inconclusive-or-over-budget. Do not rerun until green, select a best run, trim
outliers or increase samples after looking. Any later experiment must record
its reason and differences while preserving prior failures.

The fixed 240 uses old X11 paired standard deviation 266.49ms. For planning only,
true mean delta 0, boundary 50, 97.5% upper bound and 80% power give
((1.96 + 0.8416) * 266.49 / 50)^2 ~= 223 pairs; choose 240 before sampling.
Approximate mean half-width at 240 is 33.7ms for X11 and 17.7ms for Wayland.
This assumes independent similarly variable pairs, not guaranteed bootstrap
power. Drift/serial dependence weakens inference. Old X11 had 16/30 deltas >=50ms
and Wayland 8/30; more samples do not remove that p95 problem or guarantee a pass.

Prior evidence: run 38947870827, artifact 12068123487. About 8.7 minutes for
70 pairs across two backends plus 23 minutes cold build suggests roughly
30.5 minutes sampling per 245-pair matrix job plus build. This is an estimate;
the 90-minute limit remains and cannot justify reducing samples after timeout.

## Dependency closure and implementation isolation

The runner imports only driver-control/scripts/native-desktop-smoke.py from the
old immutable driver checkout, not the new driver in control. Its Python imports
are standard library; its sibling reference is native-pty-capture.py, retained
and hash-checked even though the benchmark's Bash profile does not use rawcheck.
Wayland consumes EMBER_POINTER_HELPER compiled only from the pinned native-pointer.c
and wlr-virtual-pointer-unstable-v1.xml. All four hashes are in the manifest and
workflow. System binaries/libraries come from official Ubuntu packages; record
actual package, CPU, runner image, kernel and compiler versions. They are observed,
not bit-for-bit frozen across future runner images.

The legacy benchmark-session-startup.py remains byte-for-byte unchanged because
the direct-work workflow also pins it. The new runner hash-checks then imports
that exact legacy file for statistics, cleanup and watchdog handling; sample
supervisors/helpers execute that pinned legacy file directly. This avoids copying
or modifying the safety-critical helper functions. Tests assert hash and callable
identity. It does not call legacy.run or its historical identity gate.

The only copied logic is the outer fixed-plan orchestration. It validates each
sample before adding a pair and enforces exact final count. The unchanged overlay
only alters temporary main/eframe checkouts; no recovery UI or production Rust
source is changed by this candidate.

## Local review checks

Run python3 scripts/test-session-startup-benchmark.py. These pure/static tests
never launch a desktop, app, AF_UNIX or DBus service. Check YAML, each shell block
and embedded Python. Verify exact Git blob identities for both source refs, run
overlay dry-run/application/reversal on both and check endpoint placement.
Hosted compilation and sampling remain unexecuted until separately authorized.
