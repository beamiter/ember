# Native Wayland geometry gate: stage one

Six isolated Sway cases use the same installed optimized Ember binary in Normal
mode. A fixed 2400x1800 physical output runs at actual compositor scales 1, 1.5,
and 2. Each scale tests an ordinary borderless tiled window and a borderless
floating window whose actual rectangle must be exactly (30,40,720,720) logical
units. Failure to achieve the requested geometry, including minimum-size
constraints, fails the case. Only bounded setup geometry queries may repeat;
resize commands and application input are never replayed.

The fixture reads public get_outputs/get_tree replies and requires one active
normal-transform output, exact physical mode and logical extent, and one focused
Ember surface with the owned process PID, node identity, border and floating
state. Captures are bracketed by fresh output/tree queries. Actual PNG dimensions
must equal the physical output dimensions. The complete geometry is compared
again immediately before the pointer request and after its acknowledged release.
A layout change fails, and a detected stale capture sends no input. These public
queries do not provide an atomic lock against a compositor change between the
last query and input; the post-click query detects persistent changes.

The Settings Remote tab is identified by exact case-sensitive OCR and the same
row as unique Appearance/Advanced tab anchors. OCR pixel rectangles are converted
with exact rational scale and measured output origin; the entire target must fit
inside the owned surface. The integer center is rounded down and then checked
strictly inside the word. No clamping, guessed coordinates, fuzzy OCR, or
hard-coded pointer layout extents are used. One real public virtual-pointer
click must make the unique same-row ordered Remote Hosts heading visible.
The fixture never configures a host, saves settings, invokes SSH, or connects to
a remote system. It closes Settings with the existing shortcut and requires an
exactly-once shell sentinel on the same Bash PID/TTY and byte-identical config.

Screenshots, literal TSVs, geometry before/after captures and clicks, exact
pointer coordinates/extents, configuration, shell transcript, app binary hash
and failure logs are retained. Existing desktop, clipboard ownership, recovery,
private-bus and composing-Return gates remain unchanged.

This stage validates native geometry and one measurable UI navigation positive
control. It does not complete E11 or E05. Block/Chat geometry, terminal selection
and copying, clipboard/IME at these scales, live mode switching, multi-output,
rotated output and X11 DPI are separate pending acceptance stages. Source tests
cover all six requested layouts, offsets, boundaries, rational rounding,
clipped words, unsupported output arrangements, stale capture rejection and
exactly-once pointer dispatch. Hosted runtime is required before any native
geometry coverage is claimed.

## Serialized setup after protocol diagnosis

A retained failing run showed pending IPC720x720 while the client still had an
older configure in flight. At scale2 it acknowledged720x720 but received a newer
full-size configure before committing720 pixels. Its advertised minimum was2x1
and maximum was unrestricted. This supports an overlapping setup transaction
cause; it does not establish an application minimum-size refusal.

The setup now issues border, floating, resize and move commands once each,
separated by bounded owned-client and IPC handshakes. The exact app_id maps the toplevel
to its xdg surface and wl_surface. A new toplevel configure, matching xdg serial
acknowledgment, matching window geometry and subsequent surface commit must be
observed. New pending configures invalidate old readiness; unrelated surfaces,
old serials, partial log lines and repeated render commits cannot satisfy a new
size-changing command's barrier. Destruction, ID release/rebinding or app_id
mutation of the selected chain fails closed; recreation is not supported. A
position-only move deliberately retains the completed resize handshake and
requires two fresh IPC samples at the requested position, because moving alone
need not emit a new size configure. The same geometry/protocol state agrees across two
read-only IPC samples. IPC alone can expose pending state, so the client commit
evidence is required alongside it; these observations are not an atomic lock.
All setup stages share the original30-second budget, with no repeated resize or
input. Exact720x720 and final stale-layout checks remain unchanged. Protocol
logs and synthetic before/after config copies/hashes stay in the artifacts.

### Persistent geometry in configure evidence

The owned client's `set_window_geometry` is double-buffered surface state, not
configure-local state. The parser applies pending geometry only at the owned
`wl_surface.commit` and retains it across later configure events. A same-size
configure therefore still requires its own matching acknowledgement and a later
commit, but does not require the client to redundantly resend geometry. New
configure events invalidate readiness; size mismatch, unacknowledged serials,
wrong surfaces, partial lines, and destroyed/rebound objects still fail closed.
Geometry before an acknowledgement is permitted, but is never treated as applied
before a surface commit. The public protocol contract is documented in
[xdg_surface.set_window_geometry](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/stable/xdg-shell/xdg-shell.xml).

Hosted run 38967339848 exposed this parser defect in all three narrow cases:
serial 10 set 1200×600; serial 11 repeated that size and was acknowledged without
another geometry request, then the owned surface committed. These traces prove
only the floating-stage size, not the later 720×720 resize or navigation result.
The successor retains the one-time setup commands and shared 30-second budget.
