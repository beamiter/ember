# Shared Eva companion

Enable **Appearance → Shared Eva companion (save and restart)**. The persisted
key is `eva_companion_enabled = true` (default false). The legacy local organism
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
Disconnect suppresses the body and never restores a local pet. Retirement proof
is emitted after the actual native renderer presents a cleared frame; successful
WGPU submission and Glow swap are not claims about arbitrary compositor scanout.
Strict recovery may legitimately remain blocked awaiting stronger proof.

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

Window close first withdraws this host (not global Hide), presents a cleared
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
Coordinator installation and opt-in startup are documented in jcompanion's
`docs/deployment.md`; native application installation does not silently enable it.
Focused render/unit fixtures are not physical display, voice, or live API acceptance.

Shared terminal core is pinned to `beamiter/jterm_core` commit `974ccad203375032fe87706caecab39ad516c238`; normal and dev/fixture dependency identities are aligned.

Final immutable-pin validation: `cargo check --tests --locked --offline` passed against public core `974ccad203375032fe87706caecab39ad516c238` and companion `9aa569540091524d637a7115b64c587b9177a55b`; the focused renderer harness passed **4/4**, covering 104 skin/pose shape fixtures. Native frame submission is still not physical scanout evidence.
