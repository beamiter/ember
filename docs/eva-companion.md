# Shared Eva companion

Shared Eva is enabled by default. JWM starts or reuses `jcompaniond` for the same local graphical session; terminals only connect. Set `eva_companion_enabled = false` and restart the window to disable it. Explicit saved values remain authoritative. The mode is fixed for each window lifetime, and a missing coordinator never revives the legacy organism. `EVA_COMPANION_SOCKET` selects an externally managed socket; JWM does not auto-start a coordinator for that override.


Enable **Appearance → Shared Eva companion (save and restart)**. The persisted
key is `eva_companion_enabled = true` (default true). The legacy local organism
setting is retained but its rendering and animated preview are replaced while
shared mode is active. Mode changes take effect only on window restart, not an
unsafe in-place fallback. Disable legacy pets in older windows first.

Start `jcompaniond` using the coordinated release bundle. It must share the same
UID and `XDG_SESSION_ID`. `EVA_COMPANION_SOCKET` overrides the default
`$XDG_RUNTIME_DIR/jcompanion/eva.sock`. `EVA_COMPANION_MODE=shared` is an optional
launch override, not the only settings interface.

The Eva text dock or **Alt+Shift+E** opens the keyboard-accessible card. The card
selects all eight skins and Full/Calm/Static motion, and offers Follow me, Stay
here, Hide and Recall. Appearance is persisted by the coordinator and follows
the identity across supported hosts. The source-sketch preview is static and
has no independent life/timer. This shortcut does not replace Eva's existing
voice shortcut, and the terminal does not open a microphone.

The body uses the existing renderer's proven blank block/composer-padding
surface, after its selection/IME/alternate-screen/scroll guards. Glyphs are
native egui monospace, shaped and cached per frame identity, with shared atlas
colors. Compact text or no body is used when measured native bounds do not fit.
No PTY bytes, document height or CanvasMotion semantics are changed.

One worker and one host exist per GUI window, not per split. Only focus, safe
geometry, input flags and opaque task sequence/status metadata are sent. Command
text, cwd and output are not transmitted. Remote shell observation is excluded.
Disconnect suppresses the body and never restores a local pet. Queue submission
and a Glow swap no longer produce retirement receipts. On supported Vulkan with opt-in JWM/X11,
the renderer attaches a present ID, requires `VK_KHR_present_wait` success for
that exact ID, then publishes the even extended X11 counter. Only matching
`_NET_WM_FRAME_DRAWN` plus known `_NET_WM_FRAME_TIMINGS` from the current compositor
can deliver the original UI pass to the host gate. JWM requires
`JWM_X11_FRAME_SYNC=1`. Unsupported backends/compositors, timeout, zero/unknown
timing, stale counters and owner changes provide no receipt. The optional driver
wait is bounded at 100ms per requested evidence frame; no elapsed time is proof.
This is native publication plus scene-swap evidence, not physical scanout or an
authenticated `CompositorPresented` certificate. Strict compositor-proof mode
still rejects this host. Glow/Wayland retirement support and latency acceptance
remain unfinished. Body/dialogue metadata is captured for the submitted UI pass;
a delayed receipt cannot be relabelled as the current pass or grant.

Validation: cargo-check and focused pure gate tests are separate from native
visual evidence. Real IPC/display fault injection and hardware/IME/audio timing
must be run in an environment permitting the required local sockets and display;
no software test here certifies those physical properties.

## Context, dialogue and memory card

The card registers an opaque current pane/task context. Replies remain bound to
the original context; late answers are labelled rather than inserted into a new
pane. Text lives only in the separate GUI entry. Selected terminal text is read
only after **Review selected terminal text**, bounded to 4096 UTF-8 bytes, then
shown verbatim for a separate **Approve once** click. No eligible local selection
means nothing is sent. SSH/mosh/telnet sources are excluded. Approved evidence is
single-use, expires server-side, and is invalidated on context change.

Memory inspection, edit drafts, deletion and scoped clearing all use coordinator
review records. The GUI shows exact changes and digest before a deliberate
Approve button. Task scope is the default; global scope is explicit. Additional
global retrieval for dialogue has a separate checkbox and resets when context
changes. Project scope is not fabricated from cwd: this host currently exposes
verified task/global scopes until an existing project identity is bound.
Quiet hours and night appearance are coordinator-persisted; high-contrast glyphs
are a local viewing option. Operation errors are visible, nonfatal, and never
implicitly revoke a body's residence.

## Focused headless validation

`cargo test --manifest-path tools/eva-render-harness/Cargo.toml` compiles the exact
production `src/eva_glyphs.rs` with egui and the same pure atlas re-export. It
checks all eight skins × thirteen poses through actual native font shaping and
egui tessellation, color retention, measured bounds, protected input geometry,
zero extra document height, and no static/hidden/reduced-motion animation wake.
This is renderer-level CPU evidence, not a display/GPU/IME or migration test.
The matching jterm_core `tools/companion-host-harness` tests its exact production
adapter without unrelated terminal test modules.

## Healthy close and execution identity

Ordinary window close first checks every live pane. On Linux an interactive
root must still match its pinned shell executable and process start time, and
the terminal must report a fresh empty prompt. An `exec` command can retain the
shell's PID/PGID/SID; a builtin can also retain its executable. Neither is idle
merely because the root owns the foreground. Submitted or unfinished input also
keeps the window open. A shell without command-boundary reporting requires an
explicit shell exit or tab close; process names and prompt-like output alone do
not establish idle. These observations are not authenticated shell attestation.

An admitted close withdraws this host (not global Hide), presents a cleared
frame when it owned a body, and waits for exact durable retirement/withdrawal
acknowledgements. It does not infer proof from timeout or disconnect. An explicit
force-close control warns that retirement remains unknown; existing unsaved Vim
editor protection still takes precedence. In-flight grants cannot race a silent
nonowner exit. A missing daemon may keep graceful close pending.

Fallback task IDs include the live window host UUID, persisted pane UUID and
command sequence. A restored pane whose terminal counter resets therefore cannot
alias an unrelated earlier task's memory scope. Actual globally unique execution
IDs should be preferred when supplied by an authoritative task producer.

Archived task/project scopes can also be discovered and managed after their
original GUI owner has closed. TaskScopes lists only opaque scope/project/owner
metadata, sixteen at a time. Choose a listed scope or enter an exact scope ID,
review its management-only capability/digest, then approve. This enables
inspection/edit/deletion/clear of that exact archive; it does not adopt the task
or authorize model retrieval. Scope capabilities reset on context/reconnect, and
list replies must match both the original context and selected scope.

## Reproducible dependency release

Shared coordinator/atlas source is pinned to public `beamiter/jcompanion`
commit `9aa569540091524d637a7115b64c587b9177a55b` (protocol 1).
Release manifests and Cargo.lock use immutable Git sources; Cargo acquires them
for source builds without an unversioned sibling companion checkout. Use
`cargo build --locked` and preserve existing user configuration during upgrades.
Coordinator installation and JWM session startup are documented in jcompanion's
`docs/deployment.md`; native application installation does not silently enable it.
Focused render/unit fixtures are not physical display, voice, or live API acceptance.

Shared terminal core is pinned to `beamiter/jterm_core` commit `974ccad203375032fe87706caecab39ad516c238`; normal and dev/fixture dependency identities are aligned.

Final immutable-pin validation: `cargo check --tests --locked --offline` passed against public core `974ccad203375032fe87706caecab39ad516c238` and companion `9aa569540091524d637a7115b64c587b9177a55b`; the focused renderer harness passed **4/4**, covering 104 skin/pose shape fixtures. Native frame submission is still not physical scanout evidence.

## Local native continuation, 2026-10-30

The native smoke exposed a real geometry mismatch: structural canvas padding
was capped at 88px, while the shared 40×22 atlas needs about 242px at the native
11px line height. Even a tall empty canvas therefore rendered Compact text.
The renderer now prefers existing structural padding over a small active raw
card and offers its actual available height. It still requires proven empty
padding above the composer; no terminal cell, projection row, document height
or CanvasMotion rule is changed. Short or protected surfaces retain the
existing Compact/Hidden behavior.

The strengthened padding regression failed before this correction and passed
after it. All six focused active-companion geometry/guard tests pass. The
broader UI suite reports 77 pass / 2 fail; a separate clean `c21dbf6` worktree
reproduces the same two failures (glass panel origin and old 42px footer
expectations). Those baseline assertions are recorded as failures, not waived
or presented as a passing full UI suite.

The current binary passed a real Ember↔Forge round trip, four-second SIGSTOP
blocking and coordinator SIGKILL/restart recovery in private Xvfb/DBus with
JWM's opt-in `JWM_X11_FRAME_SYNC=1` GLX compositor. A real jsh printf task ran
through the native input and produced a verified private marker and visible
Succeeded result. Screenshots show the actual shared body before and after
transfer. The test uses private home/XDG directories and executable snapshots.
See sibling `jcompanion/docs/evidence/native-jwm-ember-forge-2026-10-30/` for
the report, checksums and retained PNGs, and `jwm/docs/x11-frame-feedback.md`
for the feedback semantics and limits. This is sampled native X11 evidence;
Glow, physical scanout, Wayland, IME, audio/API and performance remain separate.


## Native publication receipts, 2026-10-31

The previous source-text test only checked that a callback followed native
submission. It could not detect the real two-Ember-body failure in the family
matrix. That test is replaced by production-source receipt/scope tests and real
native handoffs; no image detector or transfer criterion was relaxed.

`tools/native-feedback-harness` compiles the exact vendored X11 receipt tracker
and Vulkan observation scope without a GUI/device. Frost also compiles the
actual snapshot observer. Run `cargo test --locked --manifest-path
tools/native-feedback-harness/Cargo.toml` and its matching strict Clippy command.
CI runs these independently. They cover exact issued counters/original serials,
unknown timing, replay/future receipts, bounded storage, nested/failed/panicked
presentation scopes and, for Frost, captured metadata and canceled/replaced
window registrations. They do not manufacture native presentation evidence.

The pinned WGPU HAL source is vendored locally with its original license and an
`EVA-PATCH.md` describing the narrow extension. Public shared-core/companion Git
pins remain unchanged; this introduces no unversioned sibling dependency.
Only the owner-bound `_JWM_FRAME_SYNC_OWNER` handshake enables this prototype;
other compositors remain unsupported. Keep the coordinator's stronger
`CompositorPresented` mode and full release acceptance separate.

An evidence frame stays the sole native output until its matching drawn/timing
pair completes. Eframe/iced defer redraw before consuming queued draw input,
texture deltas or swapchain images; this prevents fresh counters from starving
the earlier scene acknowledgement. A complete unknown pair releases pacing for
a fresh retry without a receipt. Only a deferred draw or an unknown retry wakes
redraw. Resize invalidates pending evidence, including resize back to the same
size. No timer promotes unknown state to proof.


## Compositor authority lifecycle, 2026-10-31

A pending scene receipt must not freeze native UI after the compositor disappears.
The X11 adapter now subscribes to real XFixes compositor-selection changes before
reading ownership, and to changes of the root JWM authority marker. Owner loss,
replacement and numerical owner-XID reuse cancel pending frames and old resize
control values, preserve counter monotonicity, and request a deferred draw. These
notifications never produce a presentation or retirement certificate. Synthetic
client events are ignored. The optional XFixes library stays alive until the Xlib
display closes; without this authority subscription native evidence is unavailable.
The selection protocol is defined in the [XFixes specification](https://xorg.freedesktop.org/archive/current/doc/fixesproto/fixesproto.txt).

The production-source harness adds owner-loss, marker-ready, stale-receipt and
owner-XID reuse regressions: Ember has six tests and Frost eight (including the
snapshot observer). The transport shim refuses real authority wiring; only native
acceptance exercises the Xlib extension decoder and actual server notifications.

`jcompanion/tests/native_lifecycle.py` opens two actual GUI processes in its private
Xvfb/DBus/home session. It freezes JWM after a genuine clearance publication,
clears that session's actual compositor selection, and verifies UI/native backing
pixel progress without new keyboard input or resize. It also verifies that the
exact old token remains blocked and no new scene receipt is accepted. A real
replacement JWM must report both managed client PIDs before focus/visible-body
round trips resume. Native backing PNG pixel hashes and root samples are retained;
these are recovery observations, not physical scanout or performance measurements.


## Native mapping generations, 2026-10-31

Real same-size unmap/remap exposed a gap in the previous authority-only tracker:
a round trip could succeed while a pre-unmap counter was still accepted. The
strict native fixture now requires that exact counter to remain rejected after
remap and reacquisition; it does not replace this with an image-only success.

Server-origin MapNotify/UnmapNotify invalidate the native flight, partial receipts
and old resize-control state before IME filtering or application callbacks. Counter
values remain monotonic. Unmap does not synthesize proof or wake hidden painting;
map requests one new draw. Producer begin/finish also check actual X11 VIEWABLE
state, so an unmapped backing cannot publish an even evidence counter. A fresh
mapped native publication and scene receipt are required to finish retirement.

`tools/native-feedback-harness` now has seven Ember / nine Frost tests, including
same-size mapping changes with a native flight and partial receipts. Actual
`jcompanion/tests/native_surface_lifecycle.py` uses two GUI processes, private WM
floating rules, a genuinely published clearance with JWM paused, real unmap/map,
unchanged native geometry and original live GUI identity. The resize case also
shrinks/restores the unmapped backing before remapping. Separate cases exercise
normal WM_DELETE_WINDOW close and fail-closed owner SIGKILL. Crash blocking is not
automatic recovery: trusted surface-destruction certification remains missing.
These are scoped native acceptance cases, not physical scanout, IME or performance
measurements; machine reports preserve individual outcomes and binary hashes.
