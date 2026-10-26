# Native organism evolution

The ASCII companion uses the shared, deterministic organism model. All changes
below consume existing authoritative command events or presentation facts; none
inspect output contents, invoke an LLM, write terminal input, or create a worker.

## One companion per GUI

`OrganismHost` is owned once by `TerminalApp`, outside the split renderers.
`WindowLife`, `AmbientMind`, `CompanionVoice`, and optional `CompanionCare` live
there. Pane changes revoke old attention tickets and command identities, while
preserving the window's life, ambient mind, temperament, and story cursor.

`CompanionSurface::choose` selects Body, Compact, or Hidden. In Block/Chat there
is no permanent status-card sprite and no companion projection rows: only the
same-frame active-pane region can paint the body. Static uses this same surface
at frame zero. A narrow safe region can hold one compact glyph. Normal mode
uses the shared footer; unavailable owners no longer create a passive duplicate.
Inspection presents text about the body, including its latest reply.

The command palette binds Hello, Company, Story, Rest, Care Toggle and Care Snooze
to the session recorded when it opens. Admission revalidates that session after
the palette closes, so a stale action cannot migrate to another pane. Replies
are local authored text and preserve command/agent outcome state. The optional
care clock starts disabled (the preference is saved), uses the existing heartbeat, counts only foreground
local human activity, and waits for a quiet moment. It has no worker, new timer,
network service, or shell authority.

## Work continues between reactions

The ambient mind now receives `NativeOrganism::repo_vigil()`. A failed build or
test therefore settles into a watchful pose instead of immediately returning to
an unrelated idle animation. Several failures produce a quieter, lower posture.
Recovery remains visible until an observed successful push closes the work loop.
Missing exit status and successful unrelated commands cannot close that loop.
Exhaustion still permits rest; the shared physiology retains priority over a
guard pose.

The retained facts are volatile and scoped to the currently observed owner and
exact working-directory string. This change does not discover Git roots, restore
old work after a pane handoff, or infer what happened in an unobserved pane.
Changed idle-prompt directory metadata invalidates the previous loop before a
new command runs. A separate bounded companion directory follows OSC 7, the
first accepted command Start and accepted completion-directory metadata in wire
order. An older directory report cannot overwrite a fresher Start even when
both arrive in one parse batch; a later report can retire the old work scope.
History and the general working-directory cache retain their existing meaning.
When exhaustion wins, the status describes rest and hover inspection lists
unfinished work separately.

## Native presentation

Block and Chat bodies now have their own painted-pose transition state. The
classic footer's paint cannot seed a native body's transition. Only a sprite
that fits completely and is submitted to the painter commits a new endpoint.
Owner/source/geometry changes, accepted input, hidden or unfitting surfaces and
Calm/Static motion retire stale transitions. An active greeting keeps its own
presentation priority. Dormant owners retain their slower existing cadence.

The body measures one fixed three-row, twelve-column envelope, then lays out only
the final sprite. This removes the previous extra layout of the unmodified base
pose. A future sprite outside that envelope fails closed. A missing or unfitting
body contributes no animation wake; the window keeps its existing host
scheduling policy without a second status body.

Shared weighted animation clips supply breathing, blinking, grooming, greeting,
walking and celebration poses through the existing sprite API. Full uses 100 ms
ticks; Calm and Static use frame zero. No independent animation clock is added.

Settled work poses retain the theme's semantic color with softer borders than
immediate reactions. Compact readouts name the actual behavior. Hover inspection
on the sole body explains the final context and describes the simulated
creature qualitatively, without command text, paths, percentages or assessments
of the user. Inspection creates no greeting and does not modify life state.

Live body and footer colors preserve the shared reaction's tone. A quiet
acknowledgment after a long ordinary command or repeated clean builds can keep
its small celebration pose without turning into a bright success color. That
tone applies only to the matching, unexpired, non-running live reaction; new
work, greetings, ambient poses and previews retain their own color semantics.

## Verification scope

`src/organism_ui/single_companion_tests.rs` checks actual egui submissions across
Normal/Block/Chat and Full/Calm/Static, split handoff, compact fit, conversation
admission, and one optional care clock.

`src/organism_ui/evolution_tests.rs` exercises real parser lifecycle markers,
failure/recovery/push continuity, unknown outcomes, context boundaries, CPU paint
submission, transition cancellation, complete fit and wake suppression, and
side-effect-free inspection. The existing organism fixtures also cover attention,
input retreat, clipping, explicit motion, preview isolation and ownership.
Focused color regressions cover real long-command and habituated-build reactions,
hold expiry, new work, preview independence and greeting priority.

These tests submit and inspect egui shapes. They do not establish native desktop
input routing or GPU presentation quality; those remain covered by the project's
separate native motion and rendering gates.
