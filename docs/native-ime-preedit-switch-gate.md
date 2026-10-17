# Active preedit across live Settings mode switches

## Acceptance contract

This bounded fixture exercises Normal → Block → Chat → Normal in one Ember
process and the same shell/TTY at Wayland scale 1. It uses the public compositor
input-method-v2 protocol and real Settings keyboard/pointer actions. The visible
preedit is the established ASCII OCR token EMBERPREEDITMARKER; the composition
commit payload is fixed Chinese 你. This is a synthetic native IME protocol test,
not evidence about a particular Chinese input engine's candidate selection UI.

Opening Settings transfers keyboard ownership away from the terminal. The source
contract is cancellation of the terminal's composition, not preservation across
Settings and not implicit commitment on switching mode. No production change is
proposed. Source basis in the cd8b snapshot:

- src/app/input.rs: sync_terminal_ime_owner removes ownership while terminal input
  is blocked, clears origin preedit, and disables its local IME state.
- src/terminal_ime.rs: Ownership::sync changes Active to Cancelled; admit rejects
  the old Commit. New Opened (or Closed followed by fresh nonempty preedit) can
  establish a fresh lifecycle.
- src/ui.rs uses explicit IMEAllowed(true). The vendor's platform-output IME
  handling also controls native enablement; logical ownership cancellation does
  not imply a guaranteed native deactivate event.

For every transition:

1. Start a real raw foreground PTY collector. Observe zero bytes and the preedit
   token rendered inside the unique focused Ember surface before any switch.
2. Use Ctrl+Shift+O and actual Settings controls to persist the target mode. The
   observer never grabs, forwards, or replays keyboard events. The Settings
   opener is thus dispatched through the application's real ownership logic.
3. On return to the terminal, require the old token absent and zero PTY bytes.
   The app's owned surface identity/rectangle must match; text placement may
   differ between modes, so cross-mode pixel equality is deliberately not used.
4. Record each native done serial and activation state, the preedit origin
   serial, and challenge serial. If the original context stayed active, send its
   one late Chinese Commit only after switching and require no terminal bytes
   and no stale terminal preedit. If any native deactivation happened, abandon
   that composition, even if subsequently reactivated. Never replay old text
   into a new native activation.
5. Exit/reap the observer, then finish and drain the collector, requiring exactly
   zero bytes and the original shell PID/TTY. A subsequent fresh helper and real
   composing Return must produce exactly UTF-8 你, no CR, and one subsequent
   explicit command submission in that same shell. Exact four-line sentinel
   transcript checks final order, cardinality, and identity.

Settings text controls may legitimately own their own IME text. This contract
asserts no stale terminal dispatch or overlay; it makes no global claim that
all editors remain unchanged. The late-commit challenge here occurs after
Settings closes, so it specifically tests a surviving cancelled terminal context.
The two observed native lifecycle branches are reported distinctly. Neither
branch may be silently relabeled as preservation. A screenshot/oracle failure,
nonzero PTY byte, helper failure, identity mismatch, or cleanup failure is a
failure, not a skip.

## Hosted-only integration

The candidate is isolated. Copied existing scripts are dependency snapshots,
not publication changes. No real display, bus, socket, or helper was opened
locally. No Cargo build is needed. Local Python tests exercise mocks only.
The collector is a separate copy with a 90-second deadline to accommodate
Settings OCR; the ordinary 15-second collector is unchanged.

After independent review and root GO, add the accompanying workflow step after
existing completed-composition live-switch gate, rebased onto the then-current
workflow. Build the new C observer against the already generated input-method
protocol, and execute this stage with the same installed production binary.
Failure output must be uploaded by the existing always-run artifact step.
No publication is authorized by this document.

## Fresh-control initial health provenance

The active-preedit stage hands its immediately preceding successful transition's
post-challenge visual evidence to the following fresh composition control. This
is an optional adapter used only by this stage, not a change to ordinary initial
mode or completed-composition fixtures. It requires the transition passed its
final zero-byte collector and preserved shell identity. Source evidence must be
the exact expected transition directory's after-challenge PNG and matching JSON;
the PNG SHA256 and all saved visual fields must still match, with zero expected
and observed preedit tokens and an empty match list.

At the fresh control's initial absence snapshot, the same live application
object/PID and persisted destination mode are checked. The visual oracle then
requires the source's exact owned window identity/rectangle and anchor box in
the new snapshot. It may use the existing same-PNG PSM11 health fallback only
after zero PSM6 health anchors. Missing, duplicate, moved, stale, or mutated
provenance and preedit token presence still fail. The adapter is single-use for
this initial snapshot; later before/during/after comparisons use the fresh
cycle's own images and existing strict pixel/byte oracles. No input retry or
additional screenshot is introduced. A handoff artifact records source/current
PNG hashes, owner geometry, mode, shell identity, and selected health OCR layout.

This fixture repair does not retrospectively accept a failed hosted stage.

## Current63 post-commit health repair (pending hosted acceptance)

CI 38990885146 failed in fresh-after-chat-to-normal/ime-commit-without-cr.png,
after this fresh cycle's initial health and rendered preedit had succeeded.
Block-to-chat had completed. The third cycle had already committed exact UTF-8
without CR before its final absence snapshot failed the primary OCR health anchor.
It had not finished pixel restoration validation, final collector drain or the
submission sentinel. Do not describe this as an initial-handoff failure.

FreshVisual now requires ordered current-cycle labels, the identical view/app,
live PID and destination mode, and unchanged persisted initial PNG/JSON provenance.
Only the post-commit absence snapshot following successful rendered preedit may
opt into the existing same-PNG PSM11 health analysis. Presence layout is now preselected by the fresh destination mode as described
below; exact token counts and anchor position remain strict. No capture/input
retry or pixel restoration tolerance is introduced. ime-fresh-visual-phase.json
records the exact requested phase and failure for subsequent diagnosis.

Offline replay of the original immutable failed image passes the repaired oracle
and exact before/during/after pixel restoration. This does not retroactively pass
the failed native run; fresh hosted completion remains required.


## Fresh 5704 rendered-presence layout (pending hosted acceptance)

The new run failed in fresh-after-chat-to-normal/ime-preedit-zero-bytes.png.
Its yellow preedit marker is visibly present. Primary PSM6 merged the arrow/space
into the token word; same immutable PNG PSM11 separates the exact marker at
21,282,166,10. No post-commit frame exists for this fresh cycle.

FreshVisual chooses its presence layout at construction, before current-cycle
capture: PSM11 for destination normal, PSM6 for block/chat. The choice is recorded
in the handoff, never triggered by matching or failing an observed string. Only
ordered presence with verified current-cycle baseline may use alternate token
layout. Primary rows and exact health anchor still must be valid. Exact primary
tokens, if any, must agree with the selected layout's geometry; ambiguous,
outside or inconsistent tokens fail. Selected layout must contain exactly one
literal marker, without trimming, arrow removal, substitutions or fuzzy matching.
The screenshot hash and normal current-cycle ownership/provenance checks still
apply. Exact pixel restoration and PTY/CR checks are unchanged. Ordinary visual
callers keep the default PSM6 token policy.

Actual 5704 replay proves initial absence and rendered presence/pixel difference,
not post-commit restoration or native completion. Original63 three-frame replay
remains a separate regression check. Both native gates require fresh hosted proof.
