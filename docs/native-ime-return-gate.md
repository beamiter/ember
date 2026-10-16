# Native composing Return gate

The installed optimized binary runs on a new isolated Sway compositor for each
initial Normal, Block and Chat configuration. Existing desktop, Find ownership,
session recovery and private-bus motion gates remain unchanged.

An input-method-v2 client receives actual compositor keyboard-grab events. It
sends fixed synthetic preedit `EMBERPREEDITMARKER`; a real native Return press commits `你`, and
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
consumption with synthetic input, not a full IBus/Fcitx engine. The visual gate requires exact token absence before preedit, unique presence
during preedit, and absence after commit inside the same verified app rectangle,
plus exact restoration of the token-region pixels to the before image.
Initial-mode coverage is not live mode-switch coverage. X11 XIM, real-device IME
toggles, live switching, scaling and the remaining clipboard matrix stay open
acceptance items. No application event injection or production test mode
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

## Exact rendered-preedit evidence

Each screenshot is bracketed by Sway tree queries requiring exactly one Ember
surface, the owned application PID, current focus and an unchanged positive
rectangle contained in the output. The single output must have its expected
1280x900 geometry, normal transform and scale one; each actual screenshot must
also be exactly 1280x900. The rectangle and node identity match all three captures.
Tesseract psm6 reads each original full screenshot once. Empty or malformed word
evidence fails. Every capture requires one literal `master` status-bar health
anchor inside the owned surface and at the same bounding box. This fixture's
checkout supplies that branch label; a missing or changed label fails closed.
The anchor has been observed in the retained during/after images for all three
modes, but its new pre-composition baseline still requires hosted validation.

Matching is literal and case-sensitive: no punctuation trimming, substring,
fuzzy recognition, coordinate fallback or repeated input. Missing or duplicate
present-token rows fail. Any exact token row crossing or outside the owned
surface fails in every phase, including an expected-absent phase. OCR absence
alone never establishes clearing: the entire unique during-token bounding box
is cropped from all three original images into fixed 8-bit sRGB RGB bytes.
During pixels must differ from before, and after pixels must equal before
byte-for-byte. Crop bytes, region, hashes and equality results are retained.
Cursor blinking, animations or other repaint differences are not masked or
ignored; they fail this deliberately strict first-stage fixture for diagnosis.

Source screenshots, both geometry queries, output metadata, TSVs and match
results are retained. The preedit token never appears in shell input or displayed
commands; it is supplied only through compositor preedit. The committed Chinese
bytes and original zero-byte/no-CR oracles are unchanged. This fixture still
requires hosted runtime validation before claiming visual acceptance.
