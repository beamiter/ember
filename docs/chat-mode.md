# Chat mode

Block and Chat now share a session-owned, continuous pixel canvas. Precision
scrolling, animated wheel movement, fixed composer geometry, and anchored
streaming updates are described in [Continuous transcript canvas](transcript-canvas.md).

Chat is an opt-in terminal presentation alongside Normal and Block. It uses the
same shell session and raw terminal cells. It does not add an AI conversation or
a separate command executor.

## Enable or leave Chat

Choose **Settings → Terminal mode → Chat**, or set:

```toml
terminal_mode = "chat"
```

Choose `normal` or `block` to switch back. An explicit `terminal_mode` overrides
`block_mode`; omitting it preserves the previous `block_mode` setting and default.
This is a whole-window preference shared by all its panes. Switching layouts
does not restart the existing PTYs or discard queued native input. Independent
per-pane mode choices are not currently provided.

Submitted commands appear as right-aligned capsules, with output on a full-width
left-aligned surface. At a verified prompt, the shell's actual input cells stay
in a bottom composer while history can scroll independently. The optional ASCII
companion uses only verified blank composer space.

## Input and fallback behavior

- Run uses the existing terminal Enter encoder and is available only for a
  supported single-byte encoding. Enhanced multi-byte input remains available
  from the normal keyboard; optional buttons never truncate escape sequences.
- Controls capture the native foreground process group on pointer-down and
  recheck it with the command lifecycle at the writer boundary. Run additionally
  requires the real shell to own that foreground, even if an application emits
  shell-like OSC prompts.
- A control writes exactly one byte immediately through the idle writer's
  synchronization gate. Pending UI input/output, writer queued/in-flight bytes,
  protocol/mouse barriers, foreground changes, kernel backpressure or a closed
  writer reject it without queuing or automatic retry. Only a successful write
  arms Run's prompt-acknowledgment latch or triggers accepted-input animation.
- Interrupt sends Ctrl+C. The UI waits for the shell's completion marker before
  showing a completed command or exit code. A rejected old Interrupt cannot be
  delivered later to a different command.
- Keyboard input remains shell-owned, including multiline continuation prompts.
- `cd`, exported variables, Tab completion, history, IME and password entry keep
  their existing terminal paths. The dock does not resize the shell's PTY.
- An IME composition belongs to its originating terminal instance. Changing
  panes/tabs, opening an input-owning panel, or losing window focus cancels it;
  late commit text is discarded rather than inserted into either terminal.
  A pointer press conservatively cancels the remaining IME events in that batch,
  including same-pane clicks and opening a panel; already preceding commits
  keep their original order. Ordinary scrolling in the current pane preserves
  composition; scrolling over another pane can change the input owner.
  After cancellation, the first candidate-confirming Enter or commit may be
  discarded, so start the composition again if needed. Held Enter repeats stay
  protected through the native key tracker; a fresh Enter after a real release
  works even if the IME sends no final cancellation event.
- Alternate-screen applications, main-screen mouse/Kitty graphics takeovers,
  untrusted command boundaries and cursor addressing above an active output
  boundary use native geometry. Takeover is recorded at parser operations, even
  within one batch or in a hidden pane, and survives later mouse/cursor resets.
  Shell-reported completion and a subsequent safe prompt release it; A/B alone
  do not.
- Very long commands keep full width. Oversized multiline prompts and completion
  menus retain the scrolling terminal rather than being clipped into the dock.
- Capsule padding, separators and the footer are not terminal text. Copy, search,
  links and hit testing use original raw-cell positions. A drag crossing from
  history to the independently positioned composer stops at that boundary.

## Regression and acceptance checks

Use the repository's normal Linux dependencies and supported Rust toolchain.
The regular CI checks include formatting, Clippy, full tests and release builds.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo build --workspace --release --all-features --locked
```

The real Bash PTY acceptance test is explicit because it starts a local
interactive shell. It checks one unchanged shell process, full PTY dimensions,
working-directory/environment persistence, Tab completion, multiline input,
hidden input and Ctrl+C with a real exit status of 130:

```sh
cargo test --locked --test chat_mode_pty -- --ignored --nocapture
```

The ordinary `chat_` tests cover config compatibility, cell provenance, capsule
paint/hit alignment, scroll/dock independence, stale Run gestures, keyboard
encoding, large output/eviction and native mouse/graphics/fullscreen fallback.
The organism integration test also exercises the actual CPU egui paint path.
Its optional mesh/font-atlas export is an offscreen fixture, not live native GUI
acceptance:

```sh
EMBER_CHAT_RENDER_DUMP=/tmp/ember-chat-frame.json cargo test --locked --bin ember \
  chat_native_composer_exposes_safe_ascii_surface_and_offscreen_fixture
```

For live GUI acceptance, also check scrolling during editing, Run/Interrupt
across rapid prompt changes, completion menus, IME, selection/copy/search,
normal/block/chat switching, pane/tab changes, high-DPI resizing, a password
prompt, an alternate-screen application and a Kitty graphics application. Do
not treat an offscreen render or PTY-only pass as verification of the platform
window, GPU compositor, focus integration or accessibility.

The dependency-free IME routing check extracts the production raw-input and
writer methods, and runs the ownership reducer and native Enter tracker against
small event/transport stubs. The negative variant restores the old unguarded
writer and must fail. This does not validate an actual OS IME or GUI backend:

```sh
python scripts/test-ime-ownership.py --check-regression
```

Native IME acceptance still needs X11/Wayland testing: compose in pane A, switch
to B (and back to A), confirm the old candidate, and verify neither PTY receives
it. Repeat with a modal, window blur, pane closure, empty preedit before Commit,
and candidate Enter press/release followed by a fresh Enter in the same frame.
