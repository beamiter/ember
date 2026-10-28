# Composer tool slot height

This is separate from the rich-message font ownership change. At the existing
Main14 default, nominal 28px tool buttons inherited a 29px generic interaction
minimum. Their bottom border was y685 instead of y684. The preceding binary
has the same default14 defect; the old Main12 visual fixture concealed it.

The tool factory now sets its local interaction minimum to the shared 28px
control token. Its empty background buttons explicitly use the same 10px
control role as their separately painted captions, so an inherited Main28–72
font cannot inflate an invisible text line. No global font, setting, slot,
gap, input owner, PTY or action authority changes.

The actual painted-rectangle regression first failed at Main14 (29 versus28).
The minimum-only attempt then failed at Main28 (36 versus28). Those failures
remain recorded; the final exact assertion passes at Main8,12,14,18,28,72.
The final suite passed 2183 binary tests with6 ignored,1366 library tests
with3 ignored, and all-target/all-feature Clippy with warnings denied.

Release binary SHA256:
`52944a020ce119388c37b561973325edd1bd37f149c5f7c032734c97a2c51cdd`.
Its112-file source digest is
`34372633df92e046fd0e52dca5d7d5eb9a5c6f3cd2a179a1a464e4a33c15ab75`.
All64 native Enter frames now have the original exact footer borders y657/684,
stable1356×704 geometry and one execution in each mode. Same-binary default14
Chat/Block captures, native diff Copy and four-line exact heredoc checks passed.
Raw/snapshot Search and IME are separate same-binary gates and are not inferred
from these pixels. The known live Settings-mode-switch preedit failure is an
independent follow-up; composing Return does not certify that transition.
