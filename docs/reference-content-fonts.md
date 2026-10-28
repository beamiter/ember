# Content font ownership

The reference message renderer previously used fixed FontIds, so changing Main
did not change its heading, body, metadata, task, code or gutter glyphs. Actual
egui TextShapes and native pixels confirmed this independently of saved settings.

Rich message roles now scale from their owning Body setting relative to the
existing `Config::DEFAULT_FONT_SIZE` of 14. The central transcript owns Main;
the independent AI Chats window owns UI. At the existing Main14/UI14 default,
the shared headline/body/code hierarchy remains 20/12/10. The settings default,
legacy normalization, global zoom and display scale are unchanged.

Pill, task and diff row geometry grows with its actual text. Large line-number
gutters use measured glyph widths. Component-local interaction minima follow
the semantic body role rather than inheriting Main's generic control height;
the default task rows retain the shared 25px token. The inspector keeps its
existing, independent UI typography.

The old acceptance fixture explicitly selected Main12/UI14. New reference
captures use the actual Main14/UI14 default; old images retain their nondefault
provenance. An intermediate BODY12-normalized build (`13df0379...`) and its
measurements remain separate evidence and are not the final default contract.

## Validation checkpoint

Candidate binary SHA256:
`2c0d587fff0e7feb7b72fe5f93193df2b6c096c370a44fc2346b96685b95f0ae`.
Its 112 Rust/Cargo source-file digest is
`87cef5214fd3c0250e9b23e8081adfb79b807ce6b71d1725d3317377bbb94945`
(a content digest, not a Git commit). It uses the normal system release toolchain.

- 2182 binary tests passed, 6 ignored; all-target/all-feature Clippy passed with
  warnings denied. Actual TextShape tests cover 100%, 125% and 150% display
  scale, owner sizes 8–24, independent sibling ownership and larger diff gutters.
- Native default14 heading glyph `One` measures 43×15px; Main15 measures
  46×15px. Before the fix, Main14 and Main15 both measured 43×15px.
- The new default14 message region is pixel-identical to the old explicitly
  configured Main12 reference region. The full windows differ by 1412 pixels
  in the composer because their actual native input settings differ. This is
  not a claim of identical configurations. At the same Main14 or Main15,
  composer pixels are unchanged by the message-role patch.
- Native AI Chats keeps its glyph mask unchanged across Main14→15. UI14→15
  changes its sample user glyph from 42×9 to 45×9px and restores the original
  mask on the inverse operation. No model request was sent.
- Native diff Copy returns the exact displayed code, omitting removed lines.
  Both modes preserve the real shell through four-line heredoc input and exact
  payload execution. All 64 Enter frames retain the window geometry; each
  command executes once.
- The strict raw Search gate passed on this candidate. Snapshot, composing
  Return and live Settings-mode-switch preedit are tracked separately until
  their exact candidate runs reach a terminal result.

The original Enter footer-border assertion still fails at this checkpoint:
Main14's generic 29px button minimum makes a nominal 28px tool extend to y685
instead of y684. The same Main14 defect exists in the preceding binary; the old
Main12 fixture concealed it. Its local tool-factory repair is a separate patch,
and this checkpoint does not claim the footer gate passed. Existing physical
terminal-drag and software-only WGPU limitations also remain separate. None of
these checks certify final reference fidelity or authorize a new publication.
