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
