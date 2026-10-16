# Live Settings transitions and IME lifecycle

This fixture extends the already-green live Normal → Block → Chat → Normal and
Find ownership baseline with genuine IME composition after each transition.
It uses one installed optimized Ember process, one stock bash shell and PTY,
one isolated headless Wayland compositor, and scale 1 at 1280×900. Existing
three initial-mode composition gates remain unchanged.

Each of four segments performs the existing same-mode clipboard positive
control and both Find paste chords with zero raw PTY bytes. A fresh input-method
helper/control directory then performs the existing strict preedit lifecycle:
owned same-client modifier roundtrip, zero preedit bytes, real composing Return,
exact UTF-8 Chinese commit with no CR, and exact OCR/pixel clearing. The helper
exits and its owned composition keyboard is reaped before one explicit sentinel
submission. All sentinels and collectors must retain the original shell PID/TTY.
Three actual Settings changes use rendered controls and persisted mode checks.

One long-lived seat keyboard stays alive across all segments. Each composition
keyboard retains its own keymap through its modifier/EOF/Return sequence and is
not replaced before commit/release. Failed setup never releases the EOF barrier;
cleanup stops/reaps that keyboard before disposing stdin. Helpers are never
replayed. Fresh artifact subdirectories share the same process/environment and
cleanup ownership rather than creating another app, compositor, or shell.

The aggregate transcript is exactly one initial identity sentinel plus one
post-composition submission per segment, compared as exact UTF-8 bytes with a
terminal LF on every line. CRLF, CR-only, missing final LF, extra blank lines,
duplicate/reordered records, and changed identities fail. Before/after snapshots, strict owned
geometry, raw bytes, protocol logs, helper exits, binary/helper hashes, and
per-segment results are retained. Pure tests validate sequencing and failure
cleanup; only hosted execution can establish native success.

Excluded from this stage: switching while preedit is active, X11 XIM, a full
IBus/Fcitx engine, fractional-scale composition/selection/copy, and the broader
E05 focus/task/image-only/cross-frame/latency matrix. This stage does not close
E11 or E05.

## Same-image Settings Save fallback

In hosted run 38974054847 the first three live IME segments completed, but
both original OCR passes missed the visibly present Save button on the final
Chat-to-Normal transition. Only the Wayland Settings mode-save lookup now permits
one additional pass after both original PSM11 and PSM6 return zero exact Save
matches. It applies ImageMagick `-colorspace Gray -negate` to the same saved PNG
and runs PSM11 once. Original and transformed images, TSVs, algorithm metadata,
and owned compositor geometry snapshots are retained. Both images must remain
1280×900; exactly one literal Save must lie fully within the focused Ember
rectangle. Ambiguity, absence, clipping, geometry change, or dimension mismatch
fails without a click. A successful lookup dispatches one click and retains the
existing persisted-mode and exact shell/PTY/transcript oracles. Original images
remain the UI acceptance evidence. This does not change the application style.
