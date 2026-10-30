# Compact workspace

Ember keeps the terminal font, configured cell padding, native input owner, and
sidebar visibility choices. The workspace now spends fewer pixels on chrome:

| Area | Previous | Compact default |
| --- | ---: | ---: |
| Top bar | 69 px | 42 px |
| Rail | 73 px | 48 px |
| Navigation default | 252 px | 216 px |
| Inspector default | 350 px | 288 px |
| Session header | 88 px | 38 px |
| Bottom toolbar | 98 px | 44 px |
| Main bottom margin | 46 px | 6 px |
| Toolbar controls | 54 px | 32 px |

The header places the session title and the existing New, Search, and Session
tools actions on one row. Terminal and Tools remain in the footer. Unavailable
attachment/model placeholders and the inert profile avatar no longer occupy
permanent controls. Native Run and Stop retain their existing ownership and IME
guards, including their reserved action slot in narrow panes.

Each panel uses one low contrast 1 px edge with an 8 px radius; controls use a
6 px radius and 16–18 px icons. The rail keeps 32 px pointer targets, header
icons keep at least 28 px targets, and the 6 px terminal scrollbar retains its
expanded pointer hit area. The main surface begins 4 px below the top bar.
The renderer keeps its configured padding; the duplicate 16/17 px terminal
insets and corresponding composer expansion were removed together.

Fixed chrome panels isolate their child widgets from egui's intrinsic minimum
sizes. A long Files action row or a large inspector label therefore cannot move
the panel's allocated edge and enlarge the real PTY beneath it. Files action
rows and inspector tabs wrap within their panel. The full Files inspector,
including its tabs, actions, and tree, uses one vertical scroll area so the tree
remains reachable when larger UI text fills the panel. The terminal receives the
actual center rectangle captured before the header is rendered, and the header
also keeps its intrinsic content inside that allocation.

Larger UI fonts grow the top bar, header, and toolbar before native geometry is
calculated, across the supported UI8–72 range. At UI32, their heights become
68, 62, and 70 px. Narrow toolbars use a 32 px Tools icon, and the top bar moves
Chat/Block into a presentation menu when both labels cannot fit. The optional
product name is measured against those controls before painting. Larger UI text
uses a short Search caption; its full purpose and shortcut remain in the tooltip.
Icon controls
retain their 28–32 px widths while their rows grow. This changes the available
native area consistently for Prompt and foreground programs; switching
between those states does not itself resize the PTY. Long real input continues
to use the whole native viewport when docking would clip it. Very short split panes omit footer
controls when their full hit areas cannot fit; native keyboard input remains
available.

The Rust geometry regressions compare actual renderer grid dimensions against
the previous layout with the same visible sidebars, font size, and padding.
A real egui panel regression also switches Context/Files allocation order with
resized sidebars and UI14/32/72 text, forces oversized child widgets, and verifies
that the actual terminal rectangle and native grid stay unchanged. Separate
tests cover adaptive control/input budgets, native origin stability,
real Run/Stop pointer clips, IME ownership, and sidebar resize behavior. The
Python glass observer registration follows the compact default tokens and keeps
the full input observation domain above the real controls. Its synthetic tests
do not establish live native behavior. The Settings popup palette registration
uses a fresh native capture with strict OCR selector provenance, exact edge
colors, source hashes, and unchanged rejection thresholds. Root verification
records executed Rust and native checks for the final binary.
