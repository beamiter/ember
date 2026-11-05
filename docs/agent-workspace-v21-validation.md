# Workspace V2.1 stability validation

Baseline: Ember `c0b92ca963c3e3a9fcc6293cbe39f16628454c91`.
Shared release: `885324740c61e3b5c9c4549b4bd82100e1d54f7c`.
Date: 2026-11-05. Compiler: Rust 1.99.1, Linux cloud workspace.

## Failures reproduced from the baseline CI

[CI run 41795609083](https://github.com/beamiter/ember/actions/runs/41795609083)
failed Rustfmt, Clippy, tests, font groups, native frame receipts and the release
clipboard gate. The library suite actually ran 1,412 passing tests, three failing
tests and three ignored tests; compiling tests is not counted as running them.

- `wait_timeout` treated the PTY session drain's retryable `WouldBlock` as a fatal
  error. It now retains the unreaped ownership anchor and retries against the same
  deadline. Deterministic retry/deadline tests supplement the real PTY tests.
- Two UI assertions constructed an obsolete center margin/toolbar profile. The
  tests and production renderer now share the center/content geometry helpers,
  while independent layout, origin, clipping and grid-stability assertions remain.
- The library's immersive classifier was private but only consumed by the binary.
  It is now a documented public library export, without suppressing lint warnings.
- `.cargo/config.toml` silently replaced an immutable Git dependency with a sibling
  directory. This contaminated independent harnesses, changed their resolution and
  broke `--locked`; the receipt harness also had no sibling to load. Production
  now resolves the exact published core revision. The lockfile changed only its
  core source identity, not any dependency versions. An explicitly requested local
  candidate can still be selected with the documented command-line Cargo override.

## Checks executed locally

- `cargo fmt --all -- --check`: passed, including the baseline formatting defects.
- `python3 scripts/check-shared-core-pin.py --self-test`: five tests passed,
  including deliberately mismatched pins, missing Git source and hidden overrides.
- `python3 scripts/check-shared-core-pin.py`: manifest, dev dependency, lockfile and
  repository Cargo configuration agree on the exact shared release.
- `python3 tools/check-host-bundle.py`: 24 companion adapter files/checksums passed.
  The existing Eva delegation adapter is unchanged.
- `python3 scripts/test-files-follow.py`: 14 tests passed.
- `python3 font-group-harness/run.py`: production wiring checks and 23 Python
  regressions passed. Its Rust batches passed 14, 32 and 12 tests respectively;
  the latter two batches overlap, so these are not 58 distinct Rust tests.
- Native-feedback harness: seven tests, formatting and strict Clippy passed.
- Standalone clipboard harness: its previous lock failure is fixed; local native
  compilation is blocked by unavailable `xkbcommon.pc`. This is not a hotplug pass.

- `cargo test --locked --lib wait_timeout_ -- --test-threads=1`: six passed.
- `cargo test --locked --lib`: 1,415 passed, three failed, three ignored. Both
  previously failing UI tests and the PTY regression passed. All three remaining
  failures (`agent::diff` trusted Git tests and the recovery worktree test) reject
  the cloud sandbox's non-system-owned `/` ancestor. The security resolver was not
  weakened. A standard system-owned runner must recheck those preconditions.

- `cargo clippy --locked --all-targets -- -D warnings`: passed. This exposed a
  boxed result API adaptation and pre-existing binary-only lints masked by the
  original library lint; the fixes preserve behavior without suppressions.
- Full `--workspace --all-targets --all-features --no-fail-fast` execution also
  exposed three stale binary UI fixtures: compact Inspector font roles, measured
  product-name width, and a Chat projection that omitted production footer/bubble
  settings. These retain exact geometry, font scaling and safe-padding assertions.
  Re-running the complete command against `8853247` after these fixes executed
  1,415 passing library tests and 2,290 passing binary tests. Each target retained
  the same three trusted-Git environment failures described above; library and
  binary ignored counts were three and six. The separate block-search memory test
  passed. Ignored Chat PTY/GPU tests and unconfigured-provider fallbacks are not
  counted as native or provider acceptance.
- The built executable's host restart/draft smoke cannot create its session host
  in this restricted workspace (`Operation not permitted`). The identical isolated
  test is now a CI gate on the normal Linux runner, even if the main suite fails.

Native release and exact published-commit CI results remain pending. This document is an evidence scope record, not a
claim that V2.1 or V3 acceptance is complete.

## Verification boundaries

No physical keyboard, physical GPU, real Chinese IME candidate window, live model
provider, actual SSH target, or interactive Wayland/X11 acceptance was exercised
in this cloud record. Headless egui tests verify layout/input invariants, not those
hardware or native-environment claims. Ignored/skipped/blocked checks remain such.

The historical `packaging/jterm-core-src.tar.gz` is retained for provenance but is
not selected by production Cargo resolution. The materialization entrypoint uses
`cargo fetch --locked` for the production Git pin and creates no sibling directory.

## Native CI evidence and follow-up (2026-11-05)

The published stability revision's [CI run 41952783970](https://github.com/beamiter/ember/actions/runs/41952783970)
passed Rustfmt, strict Clippy, full tests (including the trusted-Git cases), host
restart/draft smoke, font groups, frame receipts and installer checks. Its native
release job failed. The preserved Wayland screenshot shows the PTY sentinel
command in an unsolicited API-chat composer; the Vim screenshot shows that same
chat hiding the running editor. Background catalog migration was opening its
legacy active chat after the host import acknowledgement, stealing Terminal focus.

Import now captures explicit panel visibility and the current selection generation.
Only an unchanged, explicitly visible chat may retain selection after migration.
Hidden catalog refresh cannot open it; closing or selecting another surface while
either import or load is pending invalidates navigation. The regression matrix
covers hidden/visible, edited, navigated, nonzero generation and post-import close.
The native gates remain enabled for exact-commit reruns, not counted as passing.
The Vim observer also uses the registered current dark canvas, with an independent
selected-mode paint witness and its unchanged real source-RGB inset assertions.

The dependency audit exposed a yanked `async-executor` 1.14.0. Version 1.14.1 fixes
the upstream Executor-drop/Waker use-after-free and is now pinned in the lockfile.
Three separate unmaintained advisories (`zbus_names`, `zvariant`, `zvariant_derive`)
have no patched version in the advisory database; their migration into zbus 6
requires the upstream native accessibility/settings dependency chain to update.
The audit remains strict and these advisories are not ignored or marked passed.

The recovery observer's separate old gray body/header palette was also stale.
The current explicit glass profile registers exact screenshot values, including
Color32 rounding, without admitting the legacy palette as a fallback. Reanalysis
of the preserved native artifact finds one recovery card in all six X11/Wayland
abnormal-startup images and no card in both normal-startup controls. Source
commit, run, artifact, image and SHA-256 are recorded in the observer metadata.
This is offline analysis of genuine captured pixels, not a rerun of recovery
input, recheck, export or IME behavior. Seven pure recovery regressions retain the
unique-body, attached-header and exact-warning requirements. The separate shell
Vim smoke now targets actual leftmost fixture-owned RGB ink instead of x=200 in
the workspace navigation; two pure tests reject missing or insufficient ink.

A further archived-frame check found that the header observer omitted the
38-pixel Eva control reserve although the product and fixtures enable Eva by
default. The observer now reads the same named reserve used by the unchanged
production layout, and a literal-coordinate regression independently matches the
captured frame. The old slot contained 260 exact blue pixels; the registered slot
contains 1,077 on Wayland and 1,093 on X11, under the original strict paint bounds.
Startup recovery also discards a screenshot spanning an initial compositor resize
and waits for a new stable frame within the existing 30-second deadline. The first
owned PID/window identity stays fixed across retries; identity/focus changes still
fail, and discarded frames cannot provide a paint witness.

The `9132070` native run passed both real X11 AT-SPI discovery and public Block
Search controls. Four of its six Wayland scale/layout cases passed measured
geometry, a real Remote-tab click, unchanged configuration and same-shell input.
The remaining two screenshots visibly contained the correct Settings UI, but raw
whole-screen OCR failed. Their observer now uses one preselected OCR mode on the
exact measured physical app crop after grayscale inversion, without resizing,
text repair or fallback. All 11 available before/after screenshots from the six
cases pass offline exact-label checks; this does not count as new pointer input.

The current opaque Settings menu also invalidated its historical translucent-edge
palette. The replacement profile requires one exact-color connected body, exact
98-pixel width, at least two selector heights and 80% body density, anchored below
and across the independently observed selector. Both genuine X11/Wayland captures
identify one exact Chat item; wrong palettes, dimensions, duplicate components,
low density and unowned/cross-crop OCR remain failures.

Early evidence from `03b6da4` confirms native Vim RGB, plugin paint, scrolling,
cursor and completion checks progressed beyond the previous startup failure.
Its first source row starts at y=120, making the old absolute `y < 120` assertion
stale. Source ink must now fit entirely inside an independently measured first
cursor cell, which must itself lie in the registered body's first row. A whole
extra IDE row still fails. The modal fixture's remaining shutdown timeout showed
Eva's visible close confirmation after clean Vim exit. Its updated exit gate
requires a normal VimLeavePre receipt, retirement of that exact process
incarnation, same-shell PID/TTY input before/after exit, and actual native window
closure (including the exact visible confirmation where required). Forced cleanup
cannot satisfy normal exit. These final native interactions remain pending CI.


The complete `03b6da4` run passed session recovery on both desktops and the
Block input-method-v2 compositor preedit/commit/Return gate. The first Block
segment of live-switch passed too; Chat and active-switch acceptance remain
incomplete. This is hosted protocol input, not physical IME/IBus/Fcitx coverage.

Two failed IME observations contain the exact whole-image token but the complete
body OCR omits it or joins its arrow. An independent paint witness now registers
all connected production preedit chips using the exact fill/ink colors and the
already registered font/line-height bounds. The observed exact token must lie
inside one such painted chip within the owned native body. No token-selected
crop, extra OCR, character repair or rectangular-layout-only witness is added.
The two archived `03b6da4` PNGs pass this offline check; new native acceptance
remains pending. Source PNG hashes are
`d91dc1cacb7b89a597745b42853c3b8797078602d9c5de77e31432fcb235393a` and
`52c57ead433c9c2eb147f6334c8d9ca8af14d390e18dd607bfe5818296511b3b`.

E03 reaches a visibly open Block Search window, but exact empty-entry AT-SPI
validation fails. Failed/partial discovery trees are now preserved separately
and explicitly marked unvalidated; they can never satisfy an acceptance gate.
The ownership, node/time bounds and exact editable-entry requirements stay intact
while the next native run collects the missing evidence.

`80920cf` early native evidence advances the complete Vim fixture through editing,
save/undo, scrolling/completion and clean Vim process retirement, followed by
same-shell PID/TTY input. Window closure then correctly refuses a deliberately
unintegrated shell of unknown activity. The fixture now types normal `exit` into
that already verified shell, requires its exact process incarnation to retire,
and only then requests window closure. Closing OCR has one 30-second total bound
and one CPU thread; no forced process cleanup can count as a pass.

The same native run genuinely switched X11 to Chat and passed its clipboard and
Find isolation, then reached the reverse menu with Chat visibly selected. The
selected row's exact production `(37,56,101)` fill now counts inside the same
connected `(16,24,37)` menu body, preserving the 80% painted-density threshold.
Wrong RGB values, disconnected/ambiguous bodies and inexact words still fail.
The archived reverse-menu PNG is recorded in the profile's provenance and passes
an offline exact Block-target observation; its new click remains pending CI.

E03's empty-entry ambiguity is now traced to the real `03b6da4` public tree:
the workspace sidebar and Block Search each expose an editable Entry. Both are
empty before the first query, but the old probe selected by text globally. The
probe now requires the unique named Block Search frame under the owned top-level
window, then its direct editable Entry with exact expected text. A sidebar entry
cannot shadow it or substitute for a missing/wrong picker entry. Duplicate
entries inside the picker still fail. The actual populated tree and a replay of
its empty-query state validate this hierarchy; fresh E03 native acceptance is
still pending. Discovery node/time limits and unvalidated diagnostic retention
are unchanged.

A later `80920cf` active-preedit PNG has the correct painted token but every
whole/body OCR pass either omits it or joins the arrow. The current glass profile
therefore uses one preselected PSM7 pass on every independently paint-registered
chip as its authoritative token geometry. Registration happens before token
lookup. Whole/canvas/body passes remain negative evidence: an exact token outside
one painted chip, duplicate exact tokens in any one pass, or a whole-image token
without an exact authoritative chip token still fails. No pass is retried, no
spelling is repaired, and crops are neither resampled nor chosen by OCR text.
Legacy cross-layout equality remains unchanged for the legacy profile.

Offline replay of six original preedit PNGs from `03b6da4` and `80920cf` observes
exactly one token each with this fixed profile. Both complete Block lifecycles
also preserve zero-token baseline/commit images and byte-exact pixel restoration
using the authoritative token rectangle. This is archived-image validation only;
current-master native compositor/PTY acceptance must still pass independently.


The actual `a62ef6f` native run passes complete installed-release interactions on
X11 and Wayland, full Vim editing and independent modal focus (including normal
application closure), both desktop recovery matrices, both Block and Chat
compositor preedit/commit/Return, and all six scale/geometry cases. Its two
mode-switch IME failures remain exact-token OCR misses; the fixed chip observer
above still needs a fresh native run before those gates can be claimed.

E03 now reaches the selected result and closes Block Search, but the old
closed-picker observer mistakes the workspace's persistent sidebar Entry for an
open query editor. The current glass profile identifies that one Entry only by
its direct owned top-level frame and containment to the left of the unique
public `Resize navigation sidebar` separator. A named picker, any scope control,
unknown/duplicate editor, missing separator or foreign tree still fails. No
query/body text chooses the exception or supplies a pixel success. Legacy editor
exclusion is unchanged. All four retained actual closed trees validate offline;
new native navigation, fresh target/neighbor pixels and all remaining E03 stages
still must run independently. A separate search archive exposes completed
public evidence while the later compositor gates continue.


The `f237e49` hosted run completes all three compositor IME gates, including
Block → Chat → Block live switching and Settings-boundary cancellation during
active preedit in both directions. Exact preedit pixels, zero PTY bytes during
composition, rejected stale commits, fresh composition and same-shell identity
all pass. This does not cover physical input devices or full IBus/Fcitx/XIM.

The `c8be357` hosted run passes bounded raw and snapshot E03 navigation, plus the
independent compatibility snapshot. Compatibility raw passes its first stopped
hit navigation, then a whole-frame Tesseract PSM11 process exceeds the existing
30-second budget during restart-flow observation. The failed run remains failed.
OCR subprocesses now explicitly use `OMP_THREAD_LIMIT=1`, already used by the
Vim close observer, to bound contention with the software-rendered desktop. The
application/compositor environment, image, PSM, exact predicates and 30-second
process timeout are unchanged; timeouts still fail immediately with no retry.
The execution policy is saved with native evidence. Offline replay of the
original timed-out PNG gives byte-identical TSV with and without this setting
on the cloud's newer Tesseract, but does not reproduce or prove the hosted
load cause. A fresh hosted run remains required.
