# Native reference workspace

The native egui shell binds `jterm_core::reference_shell`,
`reference_conversation` and `reference_icons`. The reference contract is in the
sibling core's `docs/reference-shell.md`. Geometry is logical pixels: 42 toolbar,
55 rail, 237 navigation, 306 inspector, 49 canvas header, and an 80-pixel minimum
composer with 28-pixel side insets and 11-pixel bottom inset. Below 1078 pixels the
inspector collapses; below 772 the navigation collapses. Actual terminal font
zoom and bounded multiline input can increase the input surface where required.
The shared chrome uses DejaVu Sans, an available substitute for the unidentified
reference font. Terminal text continues to use the configured terminal font. Headline weight 700
loads DejaVu Sans Bold explicitly (egui `strong()` changes color, not font weight);
normal body text uses the regular face. Shared a38 review metrics cover the
120-pixel model slot, soft controls, 24-pixel header actions, five Recent rows,
30-pixel Spaces rows, 49-pixel anchored status, and dual diff gutters.
Recent/Spaces section-to-row spacing and diff header groups use the fixed
6-pixel shared gap. Inspector queue rows retain the shared 24-pixel height and
2-pixel row gap; each row centers one title/elapsed baseline, with the full
detail available on hover. The queue heading has a separate 6-pixel bottom gap.

## Runtime and fixture boundaries

- Block and Chat use the same live PTY, native terminal cell renderer, fixed
  composer, focus and IME authority. Running, missing-integration and alternate-screen
  states reserve the same 80-pixel non-editable footer before native grid sizing,
  PTY resize, projection and rendering. Raw cells are never painted underneath it.
  The native viewport keeps its top/left origin and sole IME/input response.
  The footer explains foreground ownership; Stop reuses the existing validated
  Interrupt action and remains unavailable for native takeover or unknown owners.
  Block keeps real terminal output dark with
  left-aligned commands. Chat presents real commands as user capsules and real
  output under a Shell role. Output retains ANSI, native selection, copy, image,
  search and original raw-cell provenance. It is not parsed into invented agent
  progress. Existing projection caches, CanvasMotion, fractional scroll and
  stable reading anchors remain in use.
- Legacy `normal`, explicit Normal and `block_mode=false` remain readable and
  serialize without discarding old values; their effective presentation is
  Block. Public settings and toolbar expose only Block and Chat.
- Existing AI history and streamed partial messages use the same native
  proportional user/assistant message component. The existing AI input authority,
  consent and session behavior remain intact.
- Inspector title/status/provider/validation/session/current detail/queue/output
  map to the actual task manager. Missing data uses an explicit empty state.
  Structured agent steps and code diffs are not yet bound to a live agent event
  stream. Attach/Image/Code/Apply, sibling application launch and the shell's
  model selector are visibly unavailable rather than simulated operations.
- `JTERM_REFERENCE_FIXTURE=1` enables the typed **Visual acceptance fixture** in
  Chat and the inspector. The header and inspector identify sample data. Its
  six pills, four steps, code diff, inline terminal and five queue items exercise
  real native components. The fixture has a separate scroll view; it is not
  evidence that production retained-canvas behavior passes. The native shell
  composer remains real even with this explicit visual fixture enabled.
- Tools consumes the shared six-row dark popup contract. Terminal returns focus
  to the actual native input; Command palette and Search canvas open the existing
  controls. Attach file, Insert image and Insert code remain visibly unavailable.
  Each split terminal retains its own menu ID, while keyboard ownership aggregates
  all current/closing scopes. The shortcut badge
  displays the actual configured launcher binding (default Ctrl+Shift+P), rather
  than claiming the reference's Ctrl+K. PTY readline Ctrl+K is unchanged.
- The organism remains the existing single focused-session owner and animation
  model. Safe-area admission suppresses it while typing, composing,
  selecting, or when no sufficiently large proven empty region is available.
  In the fixed-dock canvas it can also use explicitly structural Padding rows
  between short history and the composer: these have no raw origin or document
  row, and every cell is verified empty. The same focused owner and one native
  draw call are reused. The visual fixture suppresses this live decoration.
  The shared reconstructed 64-pixel graphical contour replaces the native body's
  ASCII paint. Cubic paths are flattened and ear-clipped once, preserving the
  concave ear gap; strokes use the same paths. The rounded speech surface
  describes actual observed behavior. Its pointer-only collapse control reuses
  the existing expanded setting and never creates a keyboard/input owner.
  Product icons reuse the four shipped PNG identities, decoded once per context
  and painted into fixed 24/22-pixel slots.

## Input and projection invariants

The footer consists of explicit synthetic projection rows sufficient for its
28 logical pixels, based on the terminal line height. These rows have no raw
cell origin. Painting or hit-testing the send button must never borrow an
adjacent raw input row. Footer count participates in the projection cache key.
Tools registers with the existing block-chrome keyboard owner, including its
closing frame, so Escape and a same-batch Enter cannot leak into readline.
`JTERM_REFERENCE_CANVAS_TRACE=1` exposes a read-only native acceptance trace of
viewport size, scroll offset, follow intent, translation and raw top anchor. It
logs no terminal text or filesystem labels and does not mutate the projection,
scroll destination, or input state.
The actor accepts the two supported retained presentations; Block's Send/Stop
still uses the same native lifecycle/foreground/writer/IME barriers as Chat.
The existing pointer-down prompt identity, lifecycle checks, foreground process
check, writer readiness, IME guard and exact-byte authority still gate Send and
Interrupt. `chat_shared_composer_changes_history_alignment_without_moving_native_input`
checks raw anchors/cursor/grid state across both presentations.

The native preedit overlay now uses dark text on a pale chip, improving contrast
on both light canvas and dark output. It still displays compositor-owned preedit
and never writes those characters to the PTY.

## Reproduction and evidence

`scripts/reference-native-smoke.py` runs a copied release binary in an isolated
HOME/XDG/Xvfb session, sets a private configuration, types the shared fixture
through the real PTY and asserts an exactly-once shell sentinel. It captures
1356×704, 700×640, 800×640 and 1500×850 logical windows; `--scale` also verifies actual
1.25/1.5 DPR from the application, and `--details` captures Tools and scrolling. `--stream` requires the production canvas and
compares an unchanged history pixel region while real output continues; it
rejects a visual-fixture overlay as behavior evidence.
The X11 `--scale` option is application UI scaling, not compositor output scale.
Separate Sway evidence records actual output scale 1.25/1.5, physical output size,
logical app size, application DPR and Wayland buffer-scale observations.
`results.json` records git HEAD, dirty diff SHA256, binary SHA256, actual mode,
logical dimensions and DPR. It does not claim original-reference pixel approval.

The existing `native-ime-preedit-switch.py --reference-shell --renderer glow`
uses the actual Sway input-method protocol and exact UTF-8/CR/preedit quarantine
assertions. The old master footer no longer exists. Its replacement health proof
requires a focused PID-owned surface with stable geometry, a same-shell real PTY
readiness identity, and the immutable native toolbar's registered pixel palette.
No OCR rows are synthesized. Exact preedit-token OCR, owned geometry, immutable
screenshots and exact restoration pixels remain required. Legacy fixture mode
retains its original footer oracle. Settings tab/field locators use the existing
Save oracle's registered grayscale inversion for analysis only; original images
stay immutable and labels must remain exact, unique and inside the PID-owned
surface. Appearance/Font same-row anchors scope Advanced; mode menu targets are
scoped below the observed mode field to disambiguate identical toolbar labels.
The theme-backed preedit chip uses the primary dense OCR layout in both public
modes, registered before capture. Token analysis is an unchanged-pixel crop of
the entire owned central native surface (shared geometry), not a region inferred
from the expected token. Boxes map back to original pixels for exact restoration;
no fuzzy token matching or pixel tolerance is introduced.

Visual comparison to the original JPEG is owned by the coordinating task. This
implementation has not independently viewed the original pixels and does not
claim 1:1 acceptance. Final run results and screenshot paths are recorded in the
task evidence directory rather than fabricated as application state.
