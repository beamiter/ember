# jterm2

jterm2 is a Linux terminal emulator written in Rust. It combines an egui
desktop shell with a WGPU text pipeline, a built-in VTE/ANSI parser, tabs,
split panes, searchable scrollback and Kitty protocol extensions.

The project is under active development. It is useful as a daily terminal for
testing, but compatibility with every TUI and escape sequence is not yet
claimed.

## Highlights

- WGPU terminal grid rendering with a CPU/Glow fallback
- Tabs, drag-to-reorder, rename, activity indicators and split-layout restore
- Nested horizontal and vertical splits, focused-pane zoom and one-command
  divider equalization; every split starts an independent shell session
- Per-pane status headers with working directory, git branch/dirty state and
  the running command, plus desktop notifications when a long command
  finishes unwatched (OSC 133)
- Unicode width handling, combining characters, ligatures and font fallback
- Full-scrollback search with auto-reveal navigation, bounded live refresh,
  selection-aware replace, and a continuous-grid
  [semantic command timeline](docs/jsh-semantic-executions.md) (OSC 133)
- Kitty graphics plus user-initiated MIME-aware paste events (OSC 5522)
- Bracketed paste sanitization, multiline paste confirmation and guarded
  clipboard-read protocols
- Clickable URLs, IP addresses and local paths
- Built-in/custom themes, live configuration reload and resilient configurable bindings
- Bounded PTY channels, parser-work adaptive budgets, viewport-only historical
  reflow and dirty-row GPU uploads
- Crash-safe atomic state writes, bounded session restore, corrupt-snapshot
  quarantine and hardened private lock/journal files

### Kitty graphics compatibility

jterm2 implements the core 7-bit Kitty graphics APC path for direct RGB,
RGBA and PNG transfers (`t=d`, `f=24/32/100`), chunking, queries, placement,
crop, z-order, deletion, cursor movement and ordinary main-screen scrollback.
Malformed, oversized and unsupported requests receive bounded protocol errors
instead of being accepted silently.

The structural half of the protocol is shared with the other jterm terminals
(`jterm_core::kitty_graphics`): control-data parsing, chunk reassembly across
`m=1` continuations, base64 decoding, raw-format length checks and the
pre-decode PNG header sniff, together with the memory caps those steps enforce
(64 MiB encoded, 64 MiB decoded, 16384 px per side, 16 KiB of control data).
Chunked uploads are keyed per image id with a separate anonymous slot, so an
upload for `i=1` is no longer destroyed by an unrelated single-shot transfer
for `i=2`. The image store, placements, deletion, the PNG decoder and the
protocol responder stay in jterm2, because a reply's dimensions and error text
come from whichever decoder produced them.

The following advanced parts of the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
are not currently implemented: file/temporary-file/shared-memory media, zlib,
animation, Unicode placeholders, relative placements and C1 APC. Horizontal
text reflow does not re-anchor images, margin clipping is cell-row based rather
than pixel-exact, and the project-specific alternate-screen text snapshot does
not include images.

## Platform and prerequisites

jterm2 currently targets Linux (X11 and Wayland). Building requires a current
Rust stable toolchain and the native window/graphics development packages used
by winit and WGPU.

On Ubuntu/Debian:

```bash
sudo apt-get install --no-install-recommends \
  pkg-config libfontconfig1-dev libwayland-dev libx11-dev libx11-xcb-dev \
  libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxcursor-dev libxi-dev libxrandr-dev libxkbcommon-dev \
  libegl1-mesa-dev libgl1-mesa-dev
```

Clipboard integration uses the first available backend:

- Wayland: `wl-copy` / `wl-paste` from `wl-clipboard`
- X11: `xclip` or `xsel`

The application still starts if none is installed, but host clipboard actions
will be unavailable.

## Build and run

```bash
cargo build
cargo run

# Optimized binary (thin LTO, one codegen unit, stripped symbols)
cargo build --release
./target/release/jterm2
```

Set `JTERM2_SHELL` to override shell detection for one launch:

```bash
JTERM2_SHELL=/bin/zsh cargo run --release
```

Bare shell names are resolved through `PATH`; relative paths such as
`./my-shell` remain explicit. Operational warnings are enabled by default.
Enable deeper diagnostics when needed:

```bash
RUST_LOG=jterm2=debug cargo run
```

## Configuration

The main configuration is `~/.config/jterm2/config.toml`. It is created after
settings are saved. Hand-edited values are validated on startup and hot reload;
unsafe dimensions and non-finite/out-of-range numeric values are normalized.

Example:

```toml
font_family = "JetBrains Mono Nerd Font"
font_size = 14.0
font_weight = 1.0
line_spacing = 1.0
padding = 2.0
font_backend = "fontdue"       # fontdue | ab_glyph
font_ligatures = true
subpixel_rendering = false

theme = "dark"
opacity = 1.0
ui_scale = 1.0                 # omit to follow native DPI
app_renderer = "wgpu"         # wgpu | glow (restart required)
gpu_rendering = true

scrollback_lines = 10000
scroll_speed = 3
restore_session = true
tab_bar_position = "top"      # top | sidebar
shell = "/bin/bash"            # optional; JTERM2_SHELL has priority
jsh_update_check = "daily"     # startup | daily | never

# Host clipboard policy. Reading is more sensitive than writing.
osc52_clipboard_write = true
osc52_clipboard_read = false
paste_confirm = true

# Desktop toast when a command that ran at least the threshold finishes while
# its pane is not being watched (window unfocused, or a hidden tab).
notify_long_blocks = true
notify_long_block_threshold_ms = 10000

# Git branch/dirty indicator in split-pane headers.
show_repo_strip = true
```

The Settings panel exposes the same clipboard and paste-confirmation policies
under **Advanced → Security**, including a way to re-enable confirmation after
choosing “Don't ask again” in the paste preview.

`session_history_file` may point at a custom session snapshot location. Other
state is stored beside the config:

- `session_history.json` — tabs, names and working directories
- `ui_history.json` — recent commands and search history
- `keybindings.toml` — user binding overrides
- `themes/*.toml` — custom themes

Only the first running jterm2 instance owns and updates the shared session
snapshot, preventing a secondary window from overwriting the primary state.
Restore is capped at 64 sessions and 4 MiB of snapshot data. Malformed or
oversized snapshots are moved to a timestamped `.corrupt-*` backup before a
fresh session is saved; if that backup cannot be created, persistence remains
disabled for the run instead of overwriting the original. The writer applies
the same bounds, tab names are shortened on a UTF-8 boundary, and a saved
working directory that no longer exists falls back to the default directory.

### Keybindings

Defaults include:

| Action | Binding |
| --- | --- |
| New session | `Ctrl+Shift+T` |
| Close focused pane and its session | `Ctrl+Shift+W` |
| Next / previous session | `Ctrl+Tab` / `Ctrl+Shift+Tab` (`Ctrl+PageDown` / `Ctrl+PageUp`) |
| Sessions 1–8 / last session | `Ctrl+1`…`Ctrl+8` / `Ctrl+9` |
| Last active session | `Ctrl+\`` |
| Copy / paste | `Ctrl+Shift+C` / `Ctrl+Shift+V` |
| Search | `Ctrl+Shift+F` |
| Find and replace selection | `Ctrl+Alt+R` |
| Command palette | `Ctrl+Shift+P` |
| Settings | `Ctrl+Shift+O` |
| Toggle sidebar | `Ctrl+\` |
| Left/right / top/bottom split | `Ctrl+Shift+E` / `Ctrl+Shift+D` |
| Focus pane by direction | `Ctrl+Alt+Arrow` |
| Resize pane divider | `Ctrl+Alt+Shift+Arrow` |
| Zoom / restore focused pane | `Ctrl+Shift+Enter` / `Ctrl+Shift+Z` |
| Equalize all pane dividers | Command palette: “Equalize Panes” |
| Font size increase / decrease / reset | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` |
| Help | `Ctrl+Shift+/` |
| Debug overlay | `F12` |

Bindings in `~/.config/jterm2/keybindings.toml` override defaults. The file is
a flat TOML table:

```toml
"ctrl+shift+t" = "session:new"
"ctrl+alt+right" = "pane:focus_right"
"ctrl+alt+left" = "pane:focus_left"
"ctrl+0" = "font:reset"
"ctrl+shift+w" = "none" # remove a default binding
```

Chord parsing is shared with the other jterm terminals (`jterm_core`):
modifier order and aliases (`Control`, `Option`, `Cmd`/`Win`/`Meta`, `Enter`,
`Esc`, `ArrowLeft`, `PageUp`/`Prior` and similar) are normalized, `F1`–`F24`
and `Space` are bindable, and the sidebar chord may be spelled either
`"ctrl+\\"` or `"ctrl+backslash"` — both parse, and the word form is the
canonical spelling so the file never needs escaping. Assigning `none`,
`false`, `disabled`, `unbind` or an empty string removes a default binding.
Invalid entries are reported and skipped without discarding the other valid
overrides.

The in-app help panel is generated from the active bindings, so it reflects
customizations. Copy, paste and keyboard font zoom use this same command path;
there are no separate hard-coded shortcuts that bypass user overrides.

Search results are capped at 20,000 matches to keep broad queries bounded.
When old scrollback must be reflowed after a width change, jterm2 keeps the
results and navigation but suppresses any historical highlight whose raw
coordinate cannot yet be mapped exactly, rather than painting the wrong cell.

## Installing and updating jsh

jterm2 prefers its companion shell [`jsh`](https://github.com/beamiter/jsh) and
falls back to bash only when it cannot find one. The palette command
**Install or update jsh** runs the installer in its own session: the session is
the progress UI, so it can be interrupted with Ctrl+C and it waits for Enter
before closing, instead of a failure flashing past.

The installer is embedded in the binary, so a machine that has never had jsh can
still bootstrap one. It verifies the download's checksum, swaps the binary in
with `rename(2)` — **shells that are already running keep the version they
started with; new sessions pick up the new one** — keeps the previous binary for
rollback, and reports when `PATH` resolves `jsh` to some other binary of the
same name rather than this shell.

When jsh is missing or a newer one is published, a dismissible row appears under
the tab bar with the same action. The check runs on a worker thread, never
installs anything by itself, and stays silent when it cannot reach the network.
`jsh_update_check = "daily"` (the default) reuses the installer's own cache
(`~/.cache/jsh/update-check.json`), so several jterms open at once still cost one
request a day; `"startup"` asks every launch and `"never"` disables the check.

## Security notes

- OSC 52 clipboard writes are enabled by default; clipboard reads are disabled
  unless `osc52_clipboard_read = true` is set explicitly.
- MIME-aware OSC 5522 data reads are authorized only by a short-lived,
  single-use token created by an actual user paste action. The token is scoped
  to the MIME types announced for that paste.
- Multiline or large text paste asks for confirmation by default.
- Carriage returns are normalized before that policy is evaluated, and UI
  commands place their submit key outside bracketed-paste markers.
- Embedded bracketed-paste terminators are removed before forwarding data.
- Session snapshots are size/count bounded before shells are restored. Invalid
  snapshots are preserved as side-by-side backups instead of silently replaced.
- Instance locks and execution journals reject symbolic links, hard links and
  non-regular files before mutation.
- Desktop notifications use a bounded worker and always reap or time out the
  external `notify-send` helper.
- Custom-theme names are restricted to one safe filename component; theme
  saves replace symlinks rather than following them outside the theme directory.
- Link targets are shown before opening and require `Ctrl+Click`.

Terminal output is untrusted input. Keep the read policy disabled unless a
workflow genuinely requires programmatic clipboard reads.

## Architecture

```text
PTY reader/writer threads
        │ bounded events
        ▼
TerminalState + parser ── dirty rows / snapshots ──► TerminalRenderer
        │                                               │
        │ responses                                     ▼
        └────────────────────────────────────── WGPU callbacks / Glow painter

TerminalApp coordinates tabs, panes, input, search, config and persistence.
```

Important modules:

- `src/terminal/` — grid, parser, modes, selection and scrollback
- `src/pty.rs`, `src/shell.rs` — PTY lifecycle and bounded background I/O
- `src/session_manager.rs`, `src/layout.rs` — tabs, restore and pane mapping
- `src/ui.rs`, `src/gpu/` — terminal layout, glyph atlas and rendering pipeline
- `src/app/` — input, UI coordination, config/session save and window behavior
- `src/config.rs`, `src/keybindings.rs`, `src/theme.rs` — customization

## Development and verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo build --workspace --release --all-features
```

Criterion benchmarks, including deep-scrollback resize/reflow with a forced
viewport cache miss, live in `benches/terminal_benchmark.rs`:

```bash
cargo bench
```

GitHub Actions runs formatting, Clippy, tests and a release build. Please add a
focused regression test for parser, input or persistence changes whenever the
behavior can be exercised without a desktop session.
