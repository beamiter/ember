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
