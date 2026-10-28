# Single-line native IME evidence

The final observer passed both native composing-Return modes and the complete
Settings Block→Chat→Block IME workflow on unchanged production binary d6e6.
All140 pure IME/Settings tests passed. This test-only change is separate from
product commit1df5c6a. The original
whole-canvas observer missed the now-visible composition in a23px Chat input
body. Another original attempt found the exact word with PSM6 while the
preselected sparse PSM11 found none. Both failures remain recorded; they are
not retroactively marked as passes.

The reference Return and live-switch fixtures already use Main16/UI16 at
scale1. Their existing default line spacing1 is now written explicitly, and
all three fields are required before the extra observer is enabled. Each
snapshot records the real configuration path and its SHA256. The original
private132f geometry probe measured a23px native caret line. A separate CPU
measurement with the locked egui dependency found an18px control galley plus
its4px inset, a22px lower bound; it does not establish a19+4 derivation. The
renderer takes the maximum of its native atlas metric and that control bound.

Only registered single-line terminal-color planes16–23px high receive the
additional PSM6 pass. Both limits come from the fixed16pt fixture and its
measured23px line, not the target text. The whole image and whole canvas remain
observed. Larger terminal regions keep their legacy checks. Empty tight-body
OCR never independently proves absence; the original healthy owned canvas,
PID/focus, scale and immutable image checks are still required.

The exact RGB9,12,23 background may be split by a caret or pale composition.
Before width filtering, the observer joins only fragments with exactly equal
y and height. Every member's bounds and pixel count are retained. It then
requires width at least250px and the same16–23px height contract. A one-pixel
y or height difference does not join fragments. All eligible planes are
examined; words never select regions or fragments.

Exact token occurrences are combined without taking a maximum count. Across
passes only exactly equal word boxes deduplicate. Overlapping unequal boxes,
including one-pixel changes, still fail. Two occurrences from one pass, separate
bodies or a registered union remain two. A missing legacy sparse pass receives
an exemption only when an independent single-line pass witnesses the same
physical word with the exact same box. A word outside such a plane gets no
exemption. Fuzzy text and punctuation repair remain prohibited.

The unchanged lifecycle still requires one visible token during composition,
zero before/after, zero preedit PTY bytes, exact Unicode commit without CR,
consumed composing Return, one explicit later submission, exact pixel
restoration, and the same Bash/TTY. The full live-switch gate also retains its
real Find clipboard positive and zero-byte controls and Block→Chat→Block.

Offline replay passed nine immutable native images: current Block/Chat,
before/commit controls, the fragmented-background failure, and fd86/2c0d
negative images where the product had not painted the token. A derived fixture
containing two copies of actual native input-body pixels produced exactly two
occurrences and was rejected as a single composition. The rejected all-body
observer's171/172px discrepancy and the first single-component native failure
are retained. Product pixels, font sizes and layout were not adjusted for OCR.

The corrected fragment observer passed111 pure IME tests and both native
composing-Return modes on the unchanged d6e6 production binary. In the full
live-switch run, live0Block and live1Chat also completed all original protocol,
Find positive/negative and pixel-restoration checks. The overall run still
failed before live2: the separate legacy Settings-menu OCR missed the visibly
present Block option while switching Chat→Block. No final selection or third
composition was attempted. The full gate is therefore not reported as passing;
its menu failure and the earlier observer failures remain separate evidence.

A subsequent menu-only observer addresses that separate Settings blocker. It
uses the already observed selector and original popup admissible bounds, then
registers the native popup's two RGB60 vertical border runs within those bounds.
The same immutable Gray-inverted image and PSM11 are used for the tighter pass.
Exact name, uniqueness, original position constraints, actual Settings click,
Save, configuration change and PID/TTY checks remain authoritative. The failed
menu image passed offline replay; its adjacent no-popup selector image did not
register. Twelve menu negatives cover toolbar-only, duplicate names/rows,
one-pixel different boxes, wrong borders, ambiguity and owned-window bounds.

That full native rerun successfully switched Chat→Block and reached live2.
It then failed the conservative witness guard: whole-image PSM6 observed one
exact token in the real Block terminal, while the cropped canvas PSM6 observed
none and its multi-line input body did not qualify for the new thin-body pass.
This is retained as another incomplete full-gate result, not a product rendering
or submission failure. No broad exemption or count/box tolerance was added.


The final spatial-witness extension accepts a whole-image exact word only when
its whole box lies in the owned canvas and exactly one independently registered
native terminal plane. Multi-line planes use the same fixed native RGB and
component geometry; they receive no additional OCR pass. Header, inspector,
no-plane, wrong-color, crossing and ambiguous-plane cases fail. Actual word
counts remain a union, and existing cross-pass box disagreements still fail.
The full-image word is not called a successful cropped-canvas observation.

The final complete native run (same d6e6 product) passed all three segments,
with three clipboard positive controls, six Find zero-byte cases, four exact
sentinels, real Settings selection/Save twice and one unchanged Bash/TTY. Each
segment observed one exact preedit token, zero preedit bytes, UTF-8 e4bda0 with
no CR, one explicit later submission, and exact before/after pixel restoration.
Both Return gates were rerun with the final test sources and also passed.
This closes the recorded protocol/observer gates, not physical-IME coverage,
switching while preedit is already active, terminal drag selection or 1:1 design
acceptance. All earlier failed runs remain preserved as historical evidence.
