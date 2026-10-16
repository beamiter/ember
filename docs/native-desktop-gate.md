# Native desktop release-candidate gate

The existing CI release-build job stages its optimized all-features binary through
the production installer, then drives the installed executable under Xvfb/Openbox
and headless Sway. A failing desktop script fails that release-build job. No debug
substitute, application-message injection, input safety bypass, or user data is used.

The shell is stock `/bin/bash` with a synthetic login profile implementing the
public OSC 133 prompt lifecycle. Commands arrive through XTest or Wayland virtual
keyboard input; clipboard owners hold only generated fixture commands. Each
command writes a sentinel, shell PID and TTY to a real shell transcript. The
script checks exact order, exact-once execution and stable identity through live
Normal → Block → Chat → Normal settings changes. A split must create a different
shell and directional focus must return to the original identities.

Settings interaction uses OCR of actual rendered labels, desktop pointer events,
and readback of the ordinary saved configuration. Ambiguous or absent controls
fail closed. OCR is an interaction locator, not a screenshot similarity pass.
No production observability or test mode is added. The gate preserves screenshots,
OCR boxes, process/compositor logs, synthetic shell trace, transcript, installed
binary hash, package/font/source metadata and structured result on failure.

## Scope and remaining acceptance

This initial gate covers two compositor input paths, real mode switching,
clipboard command paste and split-pane keyboard focus on an installed release.
It is **not completion of roadmap E11 or E05**. Remaining required work includes:

- 1x/1.5x/2x plus narrow-window selection/copy and click geometry
- Actual IME preedit/commit, including Enter ownership and exactly-once submit
- Raw PTY-byte observation proving UI-owned clipboard events send zero bytes
- Cross-frame modifiers, focus loss, image-only and repeated clipboard events
- Measured long-clipboard input latency below the E05 100 ms bound

Software rendering evidence is not physical-GPU or multi-monitor certification.
Clipboard sentinels alone do not prove zero incidental PTY bytes. Each expansion
must add its own independent oracle and retain failures without weakening guards.

Local syntax/oracle checks do not count as a compositor run. Run the script only
on a supported display runner; do not work around sandbox socket restrictions.

## Window-manager shortcut conflicts

The X11 fixture preserves the installed Openbox configuration except its two
Ctrl+Alt+Left/Right desktop-switch bindings, which otherwise grab Ember's pane
focus shortcuts. Both original and effective XML configurations are artifacts;
the driver verifies those exact bindings before removing them. The same real
keyboard chords and strict Bash PID/TTY pane oracle remain in use. Users whose
window manager grabs these chords must resolve that desktop shortcut conflict;
this fixture does not certify behavior under a competing global grab.

## Find clipboard ownership stage

Each live mode runs a same-mode raw foreground PTY clipboard positive control,
then Find-field Ctrl+V and Ctrl+Shift+V tests. Each Find case must receive zero
PTY bytes, including its close operation. UI paste/select/copy is independently
verified with a distinct marker per case, clearing the field first, and replacing
the original clipboard with a distinct decoy before Copy
and requiring the copied marker to replace it. Byte files, expected values,
Bash parent PID/TTY, UI readback, and screenshots are retained. These checks
cover Find only; task/settings focus, cross-frame modifiers, focus loss, image
MIME, repeated paste, latency, scaling and IME remain separate requirements.

X11 readiness probes retry only within the original 30-second monotonic startup
budget. Each xdpyinfo call is capped by the remaining budget (at most three
seconds), timeout/exit diagnostics are retained, and an exited owned Xvfb fails
immediately. No application launch or input action is retried by this helper.
