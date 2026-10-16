# Owned Block Search AT-SPI discovery

This hosted-only probe uses the unchanged installed release binary, real X11 and
PTY, Ctrl+Shift+G, real query typing and exact clipboard copy readback. It does not
run the 100k search or establish E03 correctness/performance acceptance.

Every child gets a fresh explicit environment allowlist. Host DISPLAY, Wayland,
AT_SPI_BUS_ADDRESS, D-Bus address, XAUTHORITY, Python/preload hooks, credentials
and backend overrides are discarded. The fixture creates its own HOME/runtime,
private session D-Bus process group and Xvfb. The private bus alone has its public
ScreenReaderEnabled property enabled. No host accessibility setting is changed.
Official Ubuntu dbus, python3-dbus and at-spi2-core supply the public protocol.

The application root must resolve on the private accessibility bus to the exact
spawned process PID. Foreign application content is never read or recorded. Child
references must stay on that unique bus owner. The probe records public roles,
interfaces, actions, text, states and screen extents for at most 512 nodes, with a
20s soft traversal deadline and 3s individual RPC timeout; its parent subprocess has a
30s timeout. Text nodes over 4096 characters and desktop application lists over 32
entries fail. The fixture has only synthetic shell/editor content.

Discovery success requires three unique application-wide scope labels and exactly
one PID-owned editable Entry (AT-SPI role 79, Editable state bit 7, Text interface)
whose value is E03DISCOVERY, independent of the clipboard value already checked.
AccessKit maps TextInput to Entry; this does not require EditableText mutation
support. The artifact preserves the explicit application root, parent/child edges,
owner, PID, and validated editor_path. Common picker ancestry and focus acceptance
remain explicitly unverified, even when this discovery predicate passes.
The original screenshot, X11 PID/window metadata, binary digest and public tree
remain evidence. This stage does not yet dispatch public Focus or Action methods,
validate their runtime response, or replace the existing OCR Find fixture. Missing
registration, ambiguity or unsupported interfaces fail rather than falling back.

The next reviewed stage may use public Focus and observed component rectangles,
with response proven by clipboard, selected state and stable result identity.
Ctrl+Shift+G while already open closes the picker and is not a focus shortcut.
Latest moves with dynamic toolbar labels and cannot be bounded by Out's right edge.
No query OCR coordinate, private application state, or synthetic controller call
is used by this discovery probe. Remaining native E03 gates stay open.

Cleanup always attempts the tracked Smoke teardown, even if private-bus group
signaling races with process exit or fails. Non-exit signaling errors and Smoke
teardown errors are retained together. Signaling the leader's process group does
not establish activated-service group membership or exit; hosted evidence must
verify that separately. No local native execution or cleanup acceptance is claimed.
