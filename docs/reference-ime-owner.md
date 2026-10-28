# Native composition remains inside its input owner

The published design had a live mode-switch defect: the complete native gate
passed in Block, then could not see the preedit after Settings switched the
same shell to Chat. This reproduced with both the fd86 and 2c0d binaries; it
is not attributed to the later content-font change. It is not certified as a
pre-design baseline limitation.

A private, removed metadata probe confirmed that the application received the
18-byte composition. The projected caret occupied y749–772, with only 8px
below it inside the old paint boundary. The old below-only branch required a
whole further line and painted nothing. The same sequence had enough space in
Block. The probe contains geometry and byte counts, not terminal or preedit
text, and is not included in the production source.

The renderer now places the actual preedit galley below the actual caret when
it fits, otherwise above, otherwise within that caret's line. Chat bounds come
from its existing body/composer cards; command headers and the shared tool row
are not input owners. The painted composition and platform candidate rectangle
use the same galley and character cursor. Long compositions scroll that cursor
within the owner clip. An owner shorter than the glyph height cannot display a
full line; this patch does not change PTY rows or allocate extra canvas space.

The application also previously discarded egui's `active_range_chars`. In the
locked egui 0.36.2 integration, winit byte boundaries have already been converted
to Unicode-character indices. The real event adapter now uses the valid range's
trailing edge, including zero and middle CJK positions. A missing range or a
malformed, reversed or out-of-range range retains the existing end-of-text
fallback. The range carries no direction. No commit bytes, Enter ownership,
focus authority, terminal cells, presentation rows or reading anchors change.

Actual paint/candidate tests cover the observed one-line owner, below/above/
inline placement, left/right edges, scales 1/1.25/1.5, CJK middle ranges and
empty-preedit dismissal. The event-to-model-to-paint test invokes the same
adapter called by the application, not a separate test event translator.
Native live-switch evidence verifies visibility, Unicode submission and input
ownership; it does not certify every physical IME's middle-selection behavior.

The clean production build has SHA256
`d6e64969e1776748b544d1df4d11e90af8df33450568fa094d15639d9255584d`
and its112-file source digest is
`ddb04880096e8dbf279e988e6ba67758089e54339449c2c1ad6062a9174b18f4`.
The frozen-source tests passed2189 binary tests (6 ignored),1372 library tests
(3 ignored), and all-target/all-feature Clippy with warnings denied.

The first unchanged full live-switch run still failed its exact OCR assertion.
The original Chat image now visibly contains the whole composition in its
23px input body; a separate offline diagnosis, using native terminal-color
components independent of the token text, observes exactly one token in that
body with PSM6/7. The same analysis sees zero tokens in before/commit controls
and the preserved fd86/2c0d failure images. This is evidence of the original
whole-canvas OCR missing the inline last row, not a passing native gate. The
original script and failure are retained; any observer migration is a separate
follow-up with the exact-token and PTY-byte assertions preserved.

The same production binary passed a physical GUI Stop check in each mode.
The test selected the actual native accent-colored Stop square from its pixels,
clicked it, verified the real foreground process received SIGINT and ended
before normal completion, then accepted further input in the same Bash/TTY.
Block/Chat interruption times were about0.176/0.184 seconds. These checks do not
substitute for terminal drag-selection, complete physical-IME coverage, or
visual 1:1 acceptance.

The unchanged same-binary raw and snapshot Search gates passed, as did Block's
composing-Return gate. Chat's composing-Return gate failed separately with
`Token layouts disagree on exact token geometry`: its whole-canvas PSM6 found
one exact token while the preselected sparse PSM11 found none in the same
immutable image. This is distinct from live-switch's expected-one/observed-zero
failure. Neither is silently counted as a pass; the production source is saved
separately from the subsequent observer work.
