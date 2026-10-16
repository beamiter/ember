# Native composing Return gate

The installed optimized binary runs on a new isolated Sway compositor for each
initial Normal, Block and Chat configuration. Existing desktop, Find ownership,
session recovery and private-bus motion gates remain unchanged.

An input-method-v2 client receives actual compositor keyboard-grab events. It
sends fixed synthetic preedit `ni`; a real native Return press commits `你`, and
the matching release ends the grab. It never forwards or replays keys. XKB
keymaps are read from protocol descriptors with a 1 MiB size bound, checked NUL
termination, read-only mappings and explicit descriptor cleanup. All protocol
waits share a 30-second monotonic deadline. Failure releases owned resources.

The foreground Bash child runs a bounded raw PTY collector. Its atomic live
snapshot must stay empty after the preedit request, then contain exactly the
UTF-8 commit and no CR after the composition Return. Final captured bytes and
shell PID/TTY are checked again after draining. A subsequent explicit shell
command must execute once on the same shell. Screenshots, helper protocol logs,
raw bytes, identity and app/helper SHA256 results are retained on failure too.
The installed binary is already compared against the release artifact before
these tests. The helper is compiled with strict warnings from checked-in source
and the official protocol XML; distro development packages provide dependencies.

This gate exercises genuine compositor input-method protocol and Return
consumption with synthetic input, not a full IBus/Fcitx engine. It retains
preedit screenshots but does not assert visual preedit rendering. Initial-mode
coverage is not live mode-switch coverage. X11 XIM, real-device IME toggles,
visual preedit, live switching, scaling and the remaining clipboard matrix stay
open acceptance items. No application event injection or production test mode
is used, and no user display or clipboard is accessed.

## Owned keyboard readiness and cancellation

The composition keyboard is one long-lived official wtype process. Its exact
argument vector is frozen by socket-free tests: 250 ms startup, public Shift
modifier mask 1 then 0, empty stdin barrier, exactly one Return, then a long
hold terminated by the driver. Startup sleep alone is not readiness. Only this
synthetic client's WAYLAND_DEBUG log is enabled; the parser requires the same
keyboard object's modifier press/release, its following sync request, and that
exact callback's done event. Partial lines, missing releases, wrong or stale
callbacks and an early key event do not satisfy readiness. Raw PTY bytes must
be empty both before and after readiness.

The stdin pipe receives no bytes. Its EOF is released once only after the
preedit marker and a fresh zero-byte check. The same keyboard stays alive
through commit and Return release. On failure before that point, cleanup
terminates and reaps the process before closing stdin, so cancellation cannot
accidentally release Return. The initial seat-presence keyboard is separate
and never sends its delayed key; both owned processes are tracked for cleanup.

This targets a proven missing-keymap fixture setup failure, consistent with
no active live keyboard after the previous short-lived wtype process exited.
The diagnostic artifact reported active text input, no protocol error, and no
keymap before any preedit or composition Return was sent. Sway 1.9's public
modifier handler selects the sending keyboard as active. No application
keyboard routing or ownership guard is changed.
