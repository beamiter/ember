# E03 rendered-body failure diagnostics

This does not change the native OCR oracle, its 30-second timeout, the source
window or memory budgets, or E03 acceptance status. A missing exact final phrase
still fails. No O/0 substitution, alternate OCR settings, synthetic controller
pass or accessibility-only pass is permitted.

## Why collect a second kind of evidence?

Official run 39012357553, artifact 12093902146, at commit
8689b2b1a5bcd82a56571740bb495fe50a9cc6eb has archive SHA256
8d467558e1c4285ad37c06f5faec9c212f90c587a0bbda0049f92b9b7ae21604.
Its block-search-snapshot/0004-final-output-navigation screenshot visibly shows the early
record E03SNAPSHOT00 ROW000016, followed by rows 17 and 18. The corresponding
TSV records ROWO00017 (letter O), which the exact ROW000017 oracle correctly
rejects. This is evidence of an observer false negative; it is not grounds to
silently mark that native run successful.

## Public row semantics

The production renderer can expose conservative read-only Label nodes for
fully clipped-in raw rows from the very same final grid submitted to GPU/CPU
painting. These nodes carry stable raw row identity, viewport grid revision and frame
number in their description. They are recreated each frame and never take focus
or accept actions. There is no cache of body text and no scrollback traversal.
The work and temporary text are bounded by the visible grid; each row is at most
1024 ASCII bytes. No text collection is done when AccessKit is inactive.

The initial implementation deliberately withholds rows with non-default style,
colors, hyperlinks, wide characters or non-printable/non-ASCII characters. It
withholds the whole body when terminal interaction is blocked (including the
search picker/modal), a popup is open, in Chat or alternate screen, when Kitty
placements exist, with non-opaque presentation, with terminal Find open, during smooth or pending
scrolling, or with IME preedit. The cursor row is withheld. Collection happens
after the renderer's own chrome, popup actions and foreground paint submission.
Padding, metadata, collapsed summaries and offscreen/partly clipped rows are not
reported. Password input that was not echoed into the grid is never a source.
This is a narrow accessibility surface, not a complete screen-reader solution.

These are render semantics, **not compositor pixel proof**. They do not establish
GPU execution, presentation timing, glyph distinguishability, contrast, or the
absence of external window occlusion. The parent app can subsequently paint
status toasts, tooltips, recovery/debug panels and other overlays. Those layers
are outside this renderer-level diagnostic; nodes are explicitly described as
"Terminal submitted raw row", never final-visible pixel evidence. Never accept them alone as final output
navigation. The original screenshots and exact OCR remain authoritative for this
gate pending separately reviewed native evidence.

## Failed-gate collection

Only after final OCR exhausts its original deadline, the synthetic native fixture
captures `failed-body-before.png`, a PID-owned connected AT-SPI tree, then
`failed-body-after.png`. It checks the app PID, original bus owner and X11 window
ownership and records both screenshot hashes. Description fields identify
renderer rows. This does not assert that the two screenshots are equal or that
the tree was atomically captured with either screenshot. Collection failures are
recorded separately and cannot mask or change the original failed verdict.

The generic closed-picker tree mode retains all root/edge/PID/bus-owner checks;
all existing picker callers still require their exact editor and scope labels.
This collection is confined to the isolated synthetic native harness, not an
application logger or an export of the user's history.

## Verification boundary

Headless egui tests cover read-only row nodes, exact text, frame metadata,
partial/full clipping, non-ASCII/control rejection and old-frame removal. Pure
Python tests preserve the failed OCR verdict, reject foreign diagnostic owners,
and preserve tree ownership/connection checks when the picker is absent.
A production-renderer regression also exercises the actual CPU renderer for
offscreen/old-frame, blocked input, alternate screen, Chat, opacity, smooth and
nonfinite scroll, preedit, popup, Find and Kitty placement cases. This regression
has been added but cannot be executed here without the pinned jterm_core Git
dependency. Full application compile and native behavior require existing hosted CI.
These focused tests do not count as rendered-body acceptance, full E03 memory
accounting, contention/race coverage or fixed-machine interaction p95 <100ms.
