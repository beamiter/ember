# ember

ember is a Linux terminal emulator written in Rust. It combines an egui
desktop shell with a WGPU text pipeline, a built-in VTE/ANSI parser, tabs,
split panes, searchable scrollback and Kitty protocol extensions.

The project is under active development. It is useful as a daily terminal for
testing, but compatibility with every TUI and escape sequence is not yet
claimed.

## Highlights

- WGPU terminal grid rendering with a CPU/Glow fallback
- Tabs, drag-to-reorder, rename, activity indicators and split-layout restore
- Drag a single-pane tab to a target pane edge to merge it as a split, or drag
  a split-pane header back to the tab bar to make it a normal tab. A short tab
  hover previews the destination; cancelled drops never restart or clone a PTY
- Nested horizontal and vertical splits, focused-pane zoom and one-command
  divider equalization; every split starts an independent shell session
- Per-pane status headers with working directory, git branch/dirty state and
  the running command, plus desktop notifications when a long command
  finishes unwatched (OSC 133)
- Unicode width handling, combining characters, ligatures and font fallback
- Full-scrollback search with auto-reveal navigation, bounded live refresh,
  selection-aware replace, and a continuous-grid
  [semantic command timeline](docs/jsh-semantic-executions.md) (OSC 133)
- Failed semantic commands expose Fix, Explain, Retry, and Create Agent Task
  actions. Agent tasks retain the selected command's stable source identity,
  bounded C..D rendered-output snapshot and reported cwd. Experimental Codex
  tasks can run through the structured app-server protocol in a descriptor-
  pinned isolated worktree, show agent output and exact display-and-deny
  approval snapshots, continue with bounded review feedback on the same loaded
  provider thread,
  and open a bounded native Git diff review surface. A finished task can rerun
  its exact source command as a separate validation terminal and retain the
  passed / failed / needs-review result on the task card
- Opt-in [command correction](#command-correction) for narrowly classified
  failures: one review card, never an automatic run, verified against this
  host's APT index or executable PATH where it can be, and an AI fallback only
  where command-context sharing is consented to. The engine is
  `jterm_core::command_correction`, shared with the sibling terminals
- [Workflows](#workflows): saved command templates with named parameters, in
  TOML or YAML, opened with `Ctrl+Shift+M`. The rendered command is *inserted*
  at the prompt and never run for you. Discovery, both parsers, validation and
  the template engine are `jterm_core::workflows`, shared with the sibling
  terminals, so one workflow file means the same thing in all four
- Kitty graphics plus user-initiated MIME-aware paste events (OSC 5522)
- Bracketed paste sanitization, multiline paste confirmation and guarded
  clipboard-read protocols
- End-to-end OSC 8 hyperlinks plus detected URLs, IP addresses and local paths;
  hyperlink metadata survives scrollback, selection and reflow while unsafe or
  oversized targets remain inert
- A bounded-worker, lazy file sidebar whose scans and pagination never block
  the UI thread; stale slow-directory results are discarded after navigation.
  It browses SSH hosts and Docker containers natively (no sshfs), with
  right-click file operations that work the same locally and remotely
- Built-in/custom themes, live configuration reload and resilient configurable bindings
- Bounded PTY channels, parser-work adaptive budgets, viewport-only historical
  reflow and dirty-row GPU uploads
- Crash-safe atomic state writes, bounded session restore, corrupt-snapshot
  quarantine and hardened private lock/journal files

### Kitty graphics compatibility

ember implements the core 7-bit Kitty graphics APC path for direct RGB,
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
protocol responder stay in ember, because a reply's dimensions and error text
come from whichever decoder produced them.

The following advanced parts of the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
are not currently implemented: file/temporary-file/shared-memory media, zlib,
animation, Unicode placeholders, relative placements and C1 APC. Horizontal
text reflow does not re-anchor images, margin clipping is cell-row based rather
than pixel-exact, and the project-specific alternate-screen text snapshot does
not include images.

## Platform and prerequisites

ember currently targets Linux (X11 and Wayland). Building requires a current
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

The experimental native Codex task path additionally requires the currently
audited `codex-cli 0.147.0`, a ChatGPT login from `codex login`, a running
per-user systemd manager, and unified cgroup v2 with `cgroup.kill`.
If native startup fails and its containment is fully stopped, the task card
offers an explicit continuation through the ordinary terminal CLI fallback.

## Build and run

```bash
cargo build
cargo run

# Optimized binary (thin LTO, one codegen unit, stripped symbols)
cargo build --release
./target/release/ember
```

Set `EMBER_SHELL` to override shell detection for one launch:

```bash
EMBER_SHELL=/bin/zsh cargo run --release
```

Bare shell names are resolved through `PATH`; relative paths such as
`./my-shell` remain explicit. Operational warnings are enabled by default.
Enable deeper diagnostics when needed:

```bash
RUST_LOG=ember=debug cargo run
```

## Install with desktop integration

```bash
./scripts/install.sh              # build, then install binary + launcher entry
./scripts/install.sh --binary /path/to/ember  # install a prebuilt binary, skip Cargo
./scripts/install.sh --data-dir /opt/ember/share  # choose the XDG data base
./scripts/install.sh --dry-run    # print every command without changing files
./scripts/install.sh --no-desktop # binary only
./scripts/uninstall.sh            # remove both; configuration is preserved
```

`--binary` suits release archives, CI artifacts, and distro staging: the
installer never invokes the Rust toolchain, yet still installs the binary,
desktop entry, AppStream metadata, and icons through the same tested paths.
The input must be a readable, non-symlink regular file. This path requires
Linux with `/proc/self/fd` mounted and GNU `stat`; the installer fails with a
diagnostic when descriptor pinning is unavailable. Bash does not provide an
atomic no-follow open here. After the file is successfully opened and the
pathname and descriptor are verified to identify the same inode, a later
pathname replacement cannot change the inode copied through that descriptor.
The installed binary mode is always `0755`. A private temporary file is created
beside the destination and GNU `mv -T` replaces it atomically; copy failures or
exit before rename clean up that uncommitted temp and retain the previous
binary. Rename is the binary commit point; a later resource failure does not
roll it back. This requires GNU coreutils `mktemp`/`mv` in addition to the
prerequisites above. It composes
with `--prefix`, `--bin-dir`, `--data-dir`, `--no-desktop`, and `DESTDIR`.
Zero-byte prebuilt files are rejected before the old target changes. Desktop,
AppStream, SVG, and PNG sources are all preflighted before build/write; public
assets use mode-correct same-directory temps and atomic rename too. Under a
non-root packaging `DESTDIR`, repeated separators and lexical `.` components
are collapsed before every existing component from `/` through the staging
root is checked for symlinks. A disguised root link is therefore rejected
before either an install write or uninstall removal, while normal host prefixes
keep their usual symlink behavior. This is an existing-state preflight, not a
guarantee against a component replaced concurrently after the check.

Install and uninstall derive their targets from the same runtime paths: the
binary defaults to `PREFIX/bin/ember`, and the desktop entry and icons live
under `${XDG_DATA_HOME:-PREFIX/share}`. Re-running the installer updates the
same targets. `--bin-dir` overrides only the binary directory; `--data-dir`
overrides the shared-data base (and takes precedence over `XDG_DATA_HOME`).
Pass the same overrides when uninstalling later. `DESTDIR` merely prepends a
packaging root to these absolute runtime paths — the desktop entry's `Exec=`
still points at the runtime path without `DESTDIR`.
Those absolute runtime paths may contain spaces, Unicode and `.` components;
empty values, control characters, and lexical `..` components are rejected.
Only the `DESTDIR` spelling is lexically normalized as described above.

This installs into `~/.local` by default (override with
`--prefix`/`--bin-dir`/`--data-dir`, `XDG_DATA_HOME`, and `DESTDIR` for
packaging):

| File | Path |
| --- | --- |
| Binary | `~/.local/bin/ember` |
| Launcher entry | `~/.local/share/applications/io.github.beamiter.ember.desktop` |
| Icons | `~/.local/share/icons/hicolor/{scalable,128x128,256x256}/apps/io.github.beamiter.ember.*` |
| AppStream metadata | `~/.local/share/metainfo/io.github.beamiter.ember.metainfo.xml` |

That is what makes ember appear in the GNOME/KDE application list with its own
icon, ready to pin. Three details decide whether it shows up at all, and the
installer handles each:

- `Exec=`/`TryExec=` are rewritten to the binary's absolute path (system
  prefixes such as `/usr` keep the relocatable bare name). A desktop session
  fixes its `PATH` at login, so `TryExec=ember` fails and hides the entry
  **completely** when `~/.local/bin` is not on that `PATH`.
- `update-desktop-database` and `gtk-update-icon-cache` are refreshed after
  install and uninstall; a stale icon cache shadows newly installed icons.
  `DESTDIR` builds skip the refresh and leave it to the package manager.
- `StartupWMClass` is `io.github.beamiter.ember`, matching the window's real
  `WM_CLASS`. egui hands the app id to winit as its Wayland window name, which
  also becomes the X11 general class, so both display servers agree — without
  it the shell shows an unbranded window that cannot be pinned.

The window also carries its own icon: `data/io.github.beamiter.ember-128.png`
is embedded in the binary and handed to winit at startup, so `_NET_WM_ICON` is
set even for a bare `cargo run` or a session where the entry is not installed.
The launcher entry covers only windows the shell can match to it.

Verify with `desktop-file-validate <entry>` and `gtk-launch
io.github.beamiter.ember`.

## Configuration

The main configuration is `~/.config/ember/config.toml`. It is created after
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
shell = "/bin/bash"            # optional; EMBER_SHELL has priority
jsh_update_check = "daily"     # startup | daily | never

# AI is off by default. Semantic command context includes the exact command,
# working directory, and captured output. A direct loopback Ollama endpoint may
# attach it locally (an inherited HTTP proxy disables that exemption).
# Anthropic, OpenAI-compatible, and remote Ollama providers require this
# explicit cloud-sharing opt-in before Ember sends it.
ai_enabled = false
ai_share_command_context = false

# Review-first correction card for a narrowly classified failed command
# (command not found, unknown subcommand/option, an APT package name, or a
# correction the failed tool printed itself). Requires ai_enabled as well.
# Locally verified corrections — the host's APT index, the executable PATH, or
# the tool's own suggestion — need nothing else and never leave the machine.
# The AI fallback additionally requires ai_share_command_context, because its
# payload is exactly the failed command, the working directory and a bounded
# sample of that command's output. Nothing is ever executed without an explicit
# review action on the card.
command_correction_enabled = false

# Experimental local task dashboard. Independent from cloud-context consent.
experimental_task_sidebar = false

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

# Family-wide bottom status bar: working directory, git branch, last command
# status/duration, grid size and tab position. Same key in every jterm.
bottom_bar = true

# Command-card chrome (Warp-style): theme-relative cards, a colored outcome
# stripe and a status badge per OSC 133 command block. Running blocks show a
# compact live elapsed-time badge when it fits without covering terminal text.
# A badge that does not fit steps down through shorter spellings (dropping the
# finish clock, then the lifecycle qualifier, then the duration, finally to the
# bare outcome glyph) and then through smaller font sizes, so a small font or
# tight line_spacing no longer drops the outcome — and the exit code — whole.
# When a long block's own prompt row scrolls above the viewport the badge
# retries on that block's first visible row. Hovering a failed or
# unknown-status card keeps its outcome color and only brightens it; hover
# never repaints a failure in the neutral wash.
# Block Mode reserves an 8px card gutter before column zero (which can reduce
# a pane by one column). Compact only tightens visual chrome and never changes
# the PTY/cell geometry relative to non-compact Block Mode.
# Turning block mode off also clears/disables whole-block selection, so arrows
# and Enter retain their ordinary terminal meaning.
block_mode = true
block_compact = false
```

Completed records use `jterm_core::block_contract` only after Ember has merged
OSC 133 metadata with its bounded screen reconstruction. Card badges,
failure markers/navigation, and the Commands sidebar consequently agree that a
blank/background record carrying a nonzero raw status is not a failed command,
while a real command with no reported status remains unknown rather than
success.

Completion provenance is tracked separately from exit outcome. A matching
OSC 133 `C`/`D` lifecycle is healthy; a command whose `D` is lost is closed at
the next prompt boundary with an `inferred` badge, no invented exit code,
duration, or finish time. Malformed or mismatched ids, plus retained/recent
stale or duplicate execution ids, cannot close a different live block.
Inferred events may release a locally correlated Agent wait, but are excluded
from desktop completion notifications
and the durable execution journal. Context menus explain degraded lifecycles,
while Markdown and JSON copies expose completion provenance explicitly.

Whole-block interaction applies only to completed records. A plain click on a
command header selects that card; `Shift+Click` on any finished row extends a
contiguous range and `Ctrl+Shift+Click` toggles one card. Plain output clicks,
drags, double-clicks, and triple-clicks remain native terminal text selection,
and text selection takes precedence over whole-card copying. Right-click uses
the pressed card as a stable target and exposes selection-aware copy/reinput,
**Run Again** / **Retry**, search, bookmark, Agent, JSON-copy, navigation, and
**Collapse Output** actions. Re-execution is offered only for a single block
whose exact command the shell reported (`command_exact`, not shortened);
reconstructed or truncated commands are display-only and never authorize a
run, and a range keeps Reinput as its batch path because running a reviewed
list is not the same as reviewing a run. Prompt readiness, bracketed paste,
pending input and multiline refusals are reported by the shared replay guard
through the status line, exactly as the Commands sidebar's "Run again" does.
Collapsing a finished block replaces only its projected output rows with one
expandable summary row; the raw terminal history, search index, captured output,
and PTY bytes remain unchanged. A normal text selection made while output is
collapsed follows stable retained raw-cell identities across compatible
projection rebuilds, so background output no longer erases the highlight.
Column selections, width changes, effective-collapse changes, evicted endpoints,
trimmed live-grid trailing blanks, and ambiguous reflow still discard it
deliberately instead of silently selecting different bytes. Per-card filtering,
deletion, and file export
are shown as unavailable because Ember's history is one continuous terminal
grid; pretending to delete only metadata would leave the visible terminal bytes
behind.

`Ctrl+Shift+G` opens Block Search 4.4. `Aa` selects case-sensitive matching,
`.*` selects Rust-regex matching, and `W` requires Unicode whole-word matches;
`Ctrl+I` / `Ctrl+R` / `Ctrl+W` toggle the same controls without leaving the
query. `All / Cmd / Out` restricts matching to all text, commands, or output;
`Ctrl+O` cycles the scope, which is applied before the 500-hit cap. Invalid
expressions stay visible as query errors and cannot activate an older result.
`All / Failed / Slow / Bookmarked / Background` chips also browse their
category with an empty query. Results report the current position and support
wrapping `↑/↓`, `Home/End`, and ten-row `PageUp/PageDown` navigation while
keeping the virtual list aligned. `Enter` reveals and closes; `Shift+Enter`
reveals, keeps the picker focused, and advances only after the record is
revalidated. Even when a result row itself owns keyboard focus, those keys are
accepted only by the picker input pass; the row render path accepts a real
primary-pointer click, standard focused-button `Space`, or a targeted AccessKit
Click, preventing Shift+Enter from revealing twice and then closing without
discarding normal button keyboard behavior. An evicted result stays open,
refreshes, and reports the stale target instead of silently stepping.
Each virtual result row has its own `☆` / `★` bookmark button. Clicking the
star never jumps or closes the picker; exact `Ctrl+Shift+B` toggles the
highlighted hit instead. Both paths validate the current pane, completed-record
generation, hit identity, and live record before resolving the terminal-owned
monotonic sequence. This keeps every command/output hit for one record in sync
and prevents a later PTY-reused record id from inheriting a bookmark. Holding
the chord toggles at most once even if Ctrl or Shift is released before B;
ordinary Shift+B repeats still type normally. If `block:search` itself is
remapped to Ctrl+Shift+B, its close/repeat behavior retains priority over the
picker-local bookmark action. Under the Bookmarked filter,
removing the current record retains the nearest surviving result. Bookmarks are
pane-local and process-lifetime only: session close clears them, while real
`command_records` retirement prunes them behind a deque-version gate. Output
snapshot or scrollback eviction alone never removes one. Accessible result rows
report selected state; every bookmark button is one real focusable AccessKit
control whose name includes its result position and bounded command/output
context as well as its pressed state and action. Tab or AccessKit focus on a
star also selects that row. Keyboard and assistive-tech activation keeps focus
on the same stable star (or the nearest surviving star after a Bookmarked
re-filter), and falls back to the query only when no result remains. Every stale
activation path, including `Ctrl+Shift+B`, recovers that focus after its
anchor-preserving refresh; successful pointer activation retains the established
query-refocus behavior. The picker distinguishes no retained bookmarks from
bookmarks with no indexed command/output text in the chosen scope, independently
of whether the current non-empty query matched. Empty-query metadata browsing
never invents command or output text: `All` / `Out` use the first nonblank
retained output line when no real command is available, and `Cmd` produces no
hit for a commandless record.
Reopening restores the last valid query, matching controls, scope, and
metadata filter for this process only; it is never written to config or a
session snapshot. `Ctrl+U` clears only the query; **Reset** or `Ctrl+Shift+U`
restores every intent control to defaults. Invalid text above 4 KiB is not
remembered, and pointer activation of a control returns focus to the query.
Source
extraction is newest-first with an 8 MiB retained ceiling; the finished
original/lowercase index has a 16 MiB retained ceiling, counting its Vec
allocation, record ids, and every String capacity. Omitted history is reported
as `older blocks not indexed`. Rebuild releases the old cache before extraction,
so it never holds old+source+new indexes together. The lazy iterator must still
materialize the first rejected source (up to the 256 KiB per-record output cap),
and cache admission temporarily constructs one lowercase candidate before
rejecting it; these short-lived candidates are outside the retained ceilings.
Ember currently rebuilds this bounded index synchronously on the UI thread only
when the completed-record version changes; ordinary query/filter edits rescan
the existing cache. A 4 KiB query boundary (including whitespace) and a 2 MiB
regex compiler limit bound pasted expressions. Search hits retain Unicode-scalar
spans, reveal the physical soft-wrap row containing the match when possible
(expanding only the proven owning collapse), and are
revalidated against the pane plus oldest/newest record sequence before Enter,
so deque rotation cannot retarget an old hit. A refresh caused only by a
finalized-record change — a background command completing while the picker is
open — keeps the highlight on the same `(record, line)` row instead of
snapping back to the first result, so Enter cannot fire at a block the user
never chose; editing the query or flipping a case/regex/filter control is a
new intent and deliberately restarts at the top. `Shift+Enter` reveals the
current hit, advances to the next, and keeps the picker open; plain `Enter`
retains reveal-and-close behavior. The result list uses fixed-height virtual
rows: only the visible rows are materialized while keyboard, wheel and scrollbar
navigation retain the full result extent. Keyboard navigation cannot be stolen
by a stationary hover, and a background record refresh that preserves the exact
highlight also preserves the pointer user's current scroll position. Block
Search 3.9 explicitly treats same-length oldest/newest record rotation as a
version change, so count-stable retention can never leave stale results active.
If retention removes the highlighted hit, the nearest surviving old rank is
selected instead of jumping to the first row; a changed query or filter still
starts from the top as a new intent. The visible **Refresh** button and a plain,
non-repeated `F5` share one immediate rebuild path while retaining query and
selection identity; the currently configured `block:search` binding takes
priority if it is remapped onto F5, while other modified F5 chords are not
reinterpreted as refresh. An invalid expression keeps its last valid results
instead of forcing a rebuild.
If a completed record arrives while that expression is invalid, the same-pane
version rebuild is deferred until the intent becomes valid; switching panes
still releases the old pane's cache and result identities immediately.
Matching and refresh controls use their own compact row, leaving the query
editable at the picker's minimum width. When Tab, Shift+Tab, or an assistive
technology moves focus to one of those controls, Enter activates that control
even if the focus move and Enter arrive in the same low-frame-rate input batch;
it cannot be mistaken for a result jump.
Closing the picker with Escape or the current `block:search` binding also owns
the rest of that input frame, so a queued F5 or Enter cannot repopulate or
activate the released results. Holding the opening `block:search` chord does not
flash the picker closed: repeat edges from that physical press are consumed,
and a new non-repeat press still closes it normally.
An empty pane distinguishes “no completed blocks yet” from a shell that has
never reported OSC 133 and points the latter to the jsh installer.

Select a failed row in the **Commands** sidebar (or use its context menu) to
start a fresh Agent task with **Fix**, **Explain**, or **Create task**. The task
never resumes an unrelated saved transcript, remains bound to the source
terminal even when another tab is focused, and refuses to replace an Agent
command that is still running. The failed row's **Retry** action continues to
use the guarded semantic replay path. **Review Diff** opens an egui surface
containing bounded `git status --short` and tracked `git diff HEAD`
output for the current working tree; it may include pre-existing changes.
Untracked paths are listed but their contents are not read into the diff view.
Retry and Agent command execution currently require the recorded cwd to match
an independently observed local shell-process cwd; SSH/tmux-style wrappers
fail closed until Ember has an explicit remote execution backend.

The experimental **Tasks** dashboard can be enabled under **Settings → AI &
Agent** (or with `experimental_task_sidebar = true`). It tracks provider,
normalized lifecycle state, source-command provenance, an isolated worktree,
and either an attached Agent PTY or native provider stream by stable identity
rather than tab position. **Start Codex** first enters a cancellable background
**Preparing** state: registered-worktree verification, descriptor pinning,
launcher trust checks, prompt construction, and the private Codex home never
block the UI thread. Only a matching, still-current task generation under the
current sharing and redaction policy may select the native runtime after
preparation; cancelled, stale, or consent-revoked results destroy their
directory capabilities, credential buffers, and temporary home without
spawning Codex. The provider worker re-proves the descriptor-pinned Git
identity and re-resolves the trusted Codex/Node launch chain immediately before
spawn, so a queued preparation cannot authorize later path or branch changes.
**Start Codex** explicitly selects one native session for the task:
after both AI and command-context sharing are enabled, Ember sends a bounded,
optionally redacted user prompt over Codex app-server's newline-delimited
protocol and consumes structured turn, command, diff, approval, and completion
events. The source cwd is mapped to the same repository-relative directory in
the worktree. Both the root and nested cwd are opened as directory capabilities,
not re-resolved pathname strings.

Completing a turn leaves that same app-server process and provider thread
parked at a live **Ready for review** point. From the task card, the user can
send bounded review feedback to start another sequential turn on the loaded
thread, or choose **Finish Codex**. Overlapping turns and duplicate follow-up or
finish actions are rejected atomically. A live session is capped at 32 turns so
every completed provider turn identity remains remembered and can never regain
authority through bounded-history eviction. Every later turn reuses the same pinned
cwd, writable root, disabled network/environment settings, private Codex home,
and non-accepting approval policy. A new turn invalidates any older validation
result. Validation remains locked until **Finish Codex** has stopped the full
containment scope and reaped the provider. The native session itself is still
single-use: once stopped, it cannot be resumed or replaced with a second native
session for that task; cross-process `thread/resume` is not enabled.
The native response, command, and file cards keep the current/latest turn plus
an oldest-evicting, byte-budgeted history of compact completed-turn summaries.
Each history entry retains its Ember-local turn identity; follow-up turns also
retain the reviewed feedback that caused them, while approval authority remains
confined to the active turn. This is a bounded review aid rather than a durable
transcript; the Git diff remains cumulative from the task's immutable base commit.

Native Codex runs with approval policy `never`, hosted web search and tool
network access disabled, and `/tmp` excluded from tool writable roots. Its
tool-write capability is the descriptor-pinned worktree; the app-server keeps
its own transient state in a separate Ember-owned private directory. Account
default and remote execution environments are explicitly disabled for both
the thread and turn. Provider
transport to the configured Codex service still requires network access. If a
managed Codex policy raises an approval request, Ember retains and shows the
complete exact request but only allows **Deny**. Accepting either command or
file-change approval is deliberately disabled in this first native slice,
because an accepted action cannot yet be bound to Ember's pinned worktree and
process containment. **Open terminal Agent** remains the opaque compatibility fallback
and does not receive saved command/output context. Closing that PTY marks its
task cancelled; a successful child exit becomes **Ready for review** and keeps
the transcript available. Both paths diff against the commit captured when the
worktree was created, so Agent commits stay visible.

Ember stops the complete native cgroup, verifies it is empty, and reaps the
provider before publishing the terminal event that unlocks validation. An
out-of-scope guardian triggers `cgroup.kill` if Ember exits without its normal
shutdown path. Initial redaction covers only the attached command, relative
cwd, and captured terminal output; Codex may separately send worktree content
and tool output according to its provider behavior. Task metadata is currently
runtime-only; **Hide task** only hides that metadata and deliberately leaves the
active worktree untouched.

The native path does not load the user's Codex configuration, trust database,
MCP servers, hooks, plugins, apps, or refresh token. Ember creates a private
0700 `CODEX_HOME` with an empty config, passes only the current in-memory
ChatGPT access grant through app-server's login RPC, and attests the effective
config and its source layers before starting a thread. Project or managed
configuration that cannot satisfy that proof fails closed and leaves the
terminal compatibility continuation available. Tool subprocesses receive a
separate no-login environment: only a vetted absolute PATH (including safe
user-owned toolchain directories), the user's HOME, and basic locale/identity
variables; proxy credentials and the provider's private state are not exposed.
Because this relies on experimental app-server APIs, Ember currently gates the
native path to the audited `codex-cli 0.147.0` protocol identity.

The current Codex `workspaceWrite` policy confines writes but does not provide
a general host-file read boundary. Starting native Codex therefore also trusts
the installed Codex sandbox to read files available to it and potentially send
their contents to the configured provider. Ember removes shell-startup hooks,
user D-Bus access, and API-key environment variables from the provider child,
but this is not a substitute for a filesystem-read sandbox.

When an Agent process finishes successfully, **Run validation** executes the
same exact, non-truncated single-line command that created the task. Ember maps
the source repository subdirectory into the isolated worktree, canonicalizes
the target, and rejects missing paths or symbolic-link escapes before starting
a separate read-only-after-exit validation terminal. The task card records each
attempt as running, passed, failed, needs review (no authoritative exit
status), or cancelled. A failed validation does not masquerade as an Agent
runtime failure, and a passing validation still requires the explicit **Mark
complete** action after diff review. Validation cannot start until a native
Agent event stream has fully ended. A native task may run sequential turns only
while its original loaded session remains alive; after that session stops, the
terminal compatibility path is the explicit continuation after an unsuccessful
native session. If a later turn fails after an earlier review point, Ember
preserves that reviewable diff and offers either validation or explicit terminal
recovery after the provider is fully stopped. A direct Agent terminal or native terminal fallback that exits
unsuccessfully can be retried in the same isolated worktree; the old transcript
binding remains authoritative until a new PTY has spawned and atomically takes
its place, without changing runtime provenance or reopening native one-shot
authority. The validation shell uses non-login command mode so a login profile
cannot silently move execution out of the checked worktree directory. Ember
reuses the source terminal's captured shell identity, disables supported-shell
startup files, verifies that Git still registers the exact task worktree and
branch, then carries the verified cwd into the child through an open directory
descriptor and `fchdir` rather than resolving the path again.

The Settings panel exposes the same clipboard and paste-confirmation policies
under **Advanced → Security**, including a way to re-enable confirmation after
choosing “Don't ask again” in the paste preview.

`session_history_file` may point at a custom session snapshot location. Other
state is stored beside the config:

- `session_history.json` — tabs, names and working directories
- `ui_history.json` — recent commands and search history
- `keybindings.toml` — user binding overrides
- `themes/*.toml` — custom themes

Only the first running ember instance owns and updates the shared session
snapshot, preventing a secondary window from overwriting the primary state.
Restore is capped at 64 sessions and 4 MiB of snapshot data. Malformed or
oversized snapshots are moved to a timestamped `.corrupt-*` backup before a
fresh session is saved; if that backup cannot be created, persistence remains
disabled for the run instead of overwriting the original. The writer applies
the same bounds, tab names are shortened on a UTF-8 boundary, and a saved
working directory that no longer exists falls back to the default directory.

### Command correction

Off by default: it needs both `ai_enabled` and `command_correction_enabled`
(**Settings → AI & Agent → Offer corrections for failed commands**). With them
on, a failed command that Ember can classify *narrowly* — `command not found`,
an unknown subcommand, an unknown option, an APT package name `apt` could not
locate, or a correction the failed tool printed itself — raises one review card
above the active session. An ordinary nonzero exit raises nothing.

The card is review-first, and nothing on it runs by itself. It pre-fills an
editable field with the proposal, takes keyboard focus only from a clean, idle
prompt and only within a bounded retry window, and its first frame cannot
consume a trailing Enter from the same input batch as approval. The primary
action reads **Insert for review** — the command is written to the prompt and
the user presses Enter — unless the proposal was verified against this host, is
still exactly as proposed, and is not destructive; only then does it read **Run
verified command**. Any edit downgrades it back to insert-only on the same
frame. A destructive draft (`rm -rf …` and friends) is now labelled
`⚠ destructive` beside the field; it was always offered, but previously in the
same chrome as `git status`. Escape, **Dismiss** and the window ✕ close the card
and cancel the request behind it.

**Verified locally, or suggested by a model.** Two evidence classes reach the
card and it says which:

- *Verified* — the replacement exists in this host's APT package index or on its
  executable PATH. Nothing leaves the machine, and this is the only class that
  can offer direct execution.
- *Unverified* — a correction the failed target printed, or the AI fallback.
  These are always insert-only.

The AI fallback runs only when `ai_share_command_context = true` (or the
provider is a directly configured loopback Ollama endpoint, with no inherited
HTTP proxy), because its payload is exactly the failed command, the working
directory and a bounded sample of that command's output. With the switch off —
its default — no AI correction is requested and none appears, while the
locally verified ones keep working. Ember has gated the fallback this way since
the feature landed; as of 2026-08-29 all four jterm terminals do, which is what
the shared engine's `ContextSharing` state now enforces at compile time.

**What a proposal may not be.** A candidate is refused, not shown, when it adds
shell control syntax the original did not have, adds `sudo`/`doas`/`su`, adds
`ssh`/`mosh`/`scp`/`sftp`, is unchanged, is not one printable line, exceeds
16 KiB, or — new on 2026-08-29 — hands a pipeline stage to a shell or
interpreter the original did not. That last rule is the reason a failed
`curl … | head -20` can no longer be "corrected" into `curl … | sh`: the older
check only asked whether a `|` was *present*, and the original already had one.
The rule compares the set of interpreters each pipeline stage runs, so `|  sh`,
`| /bin/sh`, `| zsh`, `| busybox sh` and `| xargs -n1 sh -c` are all refused,
while `ls | gerp foo` → `ls | grep foo` is still offered.

**Automatic helpers.** Gathering evidence runs two programs for you, without
asking: `bash --noprofile --norc -lc 'compgen -c | LC_ALL=C sort -u'` to
enumerate command names, and `apt-cache pkgnames` to check a package name.
Those are the only two programs this surface may ever execute. Both are
resolved through the family's shared trust predicate, which also checks every
directory on the way to them: a binary that is group- or world-writable, or
owned by neither root nor you, is not a system helper, wherever on `PATH` it
sits. On a host where neither passes that check, PATH-verified corrections
still work — command names then come from a read-only directory walk of the
same `PATH`, which executes nothing — but APT-verified package corrections
disappear, because nothing else can answer that question. That is the intended
trade: the alternative was executing the binary. Running Ember as root no
longer disables every helper (see **Security notes**).

**Untrusted completions raise no card.** A command block whose end Ember had to
infer — a later prompt forced it shut and the OSC 133 end mark never arrived —
carries stale scrollback and a guessed status, so it is now skipped here, as it
already was by the execution journal, the Agent panel and the long-command
toast. Cards that used to appear after an interrupted or force-closed block will
stop appearing.

Since 2026-08-29 the whole engine half of this feature — classification, token
extraction, ranking, the safety gate, the prompt, the reply parser, the helper
trust predicate, the probe layer and the request epoch machine — lives in
`jterm_core::command_correction` and is shared verbatim with anvil, forge and
frost. Ember keeps only the card, its focus and arming rules, and the effect the
app applies to the PTY.

### Workflows

A workflow is a saved command template with named parameters — the jterm
family's shared "parameterised snippet" format. `Ctrl+Shift+M` opens the
picker; fuzzy search matches name, description and tags. Enter on a result
opens the parameter dialog, or — for a template that declares no arguments —
goes straight to the prompt. **Insert command** (or Enter in the dialog) writes
the rendered command to the prompt and stops. Nothing is executed for you, the
same review-first rule the correction card and the history picker follow, and
insertion takes the identical guarded path: a read-only task terminal, an
alternate screen, a prompt that is not ready or not empty, or pending input all
refuse it. Escape closes either surface. The library is re-read each time the
picker opens, so a file added on disk appears the next time you press
`Ctrl+Shift+M`.

Workflow files are TOML or YAML (`.toml`, `.yaml`, `.yml`) and are read, in
precedence order, from:

1. `~/.config/ember/workflows/`
2. every entry of `$EMBER_WORKFLOW_DIR` — a `:`-separated list that *adds* to
   the standard locations rather than replacing them
3. `~/.local/share/ember/workflows/`
4. `<dir>/ember/workflows/` for each `$XDG_DATA_DIRS` entry
5. the checked-out `scripts/workflows/` when Ember runs from a source tree

Workflow *names* are unique across the whole path and the first occurrence
wins, so a file in `~/.config/ember/workflows` shadows an installed example of
the same name. The picker lists the library alphabetically by name. A file that
does not parse is skipped without hiding the rest; Ember logs its path and
reason and shows a bounded one-line toast naming the first rejected file. The
toast repeats only when the set of rejected paths changes, so reopening the
picker does not nag while fixing or newly breaking a file remains visible. Each
file is opened with `O_NOFOLLOW` and read under a size cap, so a symlinked or
oversized workflow file is refused rather than followed.

```yaml
name: "Kill process on port"
description: "Find and kill whatever is listening on a TCP port"
command: "lsof -ti tcp:{{port}} | xargs -r kill -{{signal}}"
tags: [net, debug]
args:
  - name: port
    description: "TCP port"
    default: "3000"
  - name: signal
    description: "Signal to send (TERM/KILL/HUP)"
    default: "TERM"
```

`{name}` and `{{name}}` both substitute a declared argument, and placeholder
names are trimmed, so `{{ port }}` binds exactly like `{{port}}`. A `{{…}}`
that matches no argument is the literal-brace escape and emits single braces,
mirroring `format!` — `awk '{{print $1}}'` renders `awk '{print $1}'`.

**An argument that declares no `default` can no longer be left blank** (changed
2026-08-29). Ember used to pre-fill every parameter field with the empty
string, so `kill -9 {{pid}}` submitted with an untouched **Pid** field rendered
`kill -9 ` and put *that* at the prompt. Submitting it now reports
`missing values: pid`, the dialog stays open with the error, and the row is
marked `*` — with `* needs a value` in the dialog footer — before Enter is ever
pressed. Whitespace-only counts as blank.

Declaring a default is how a file says that an empty value is meaningful there.
`default: ""` still renders empty, and clearing a field that *has* a default is
a deliberate empty value rather than a fallback to the default. The one bundled
example this changes is `docker-tail-logs.yaml`: its `container` argument
declared `default: ""` while plainly meaning "required", which would have
inserted `docker logs -f --tail 100 `. That empty default is gone, so
`container` must now be filled.

Two further changes in the same direction:

- A declared argument name with leading or trailing whitespace (`name: "pid "`)
  is now rejected at load. It used to load clean and then bind nothing:
  `{{ pid }}` rendered as the literal `{ pid }`, the dialog considered the form
  complete, and whatever was typed in that field was dropped on the way to the
  prompt.
- `{{` and `}}` now nest, so an unterminated `{{` survives a later placeholder
  closing a pair. `awk '{{print $1}' {{log}} | sort -u` used to render as
  `awk '{print $1}' access.log | sort -u` — a different, executable awk program
  — because the scan for the closing braces ran to the end of the template.
  Nested JSON bodies such as `-d '{{"a":{{"b":1}}}}'` are unaffected.

Since 2026-08-29 discovery, the bounded reader, both parsers, validation and
the template engine live in `jterm_core::workflows` and are shared verbatim
with anvil, forge and frost — the four terminals read the same files out of the
same directories, so a difference in what one of them accepted was a difference
in what a user's file *meant* depending on which terminal opened it. Ember
keeps its search-path policy, its alphabetical load order and the egui overlay.

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
| Window opacity increase / decrease | `Ctrl+Alt+=` / `Ctrl+Alt+-` |
| Select all completed command blocks | `Ctrl+Shift+A` |
| Reinput selected block commands (without running) | `Ctrl+Shift+I` |
| Select previous / next command block | `Ctrl+Shift+[` / `Ctrl+Shift+]` (also bound as `{` / `}`, since layouts report the shifted bracket either way) |
| Jump to the oldest failed block | `Ctrl+Shift+X` |
| Previous / next failed block | `Ctrl+Shift+,` / `Ctrl+Shift+.` |
| Toggle bookmark on the active completed block (or selected Block Search hit) | `Ctrl+Shift+B` |
| Previous / next bookmarked block (wrapping) | `Ctrl+,` / `Ctrl+.` |
| Search/filter completed command blocks | `Ctrl+Shift+G` |
| Toggle Agent panel (per-command approval) | `Ctrl+Alt+G` |
| Toggle AI chats library (read-only; nothing runs) | `Ctrl+Shift+Alt+A` |
| Workflow picker (parameterised command templates) | `Ctrl+Shift+M` |
| Help | `Ctrl+Shift+/` |
| Debug overlay | `F12` |

Block selection is context-sensitive. `Ctrl+Up` starts at the newest block;
once a block is selected, plain `Up` / `Down` collapses and moves the active
edge, while `Shift+Up` / `Shift+Down` expands or contracts the anchored range
and reports the resulting block count. `Enter` reinputs every selected real
command in terminal order as editable bracketed-paste text, background-output
blocks are skipped, and `Escape` clears the range. Enter continues to the child
unchanged while a command or alternate-screen program owns the PTY.

Selection movement is deliberately minimal — it keeps the viewport where the
reader left it whenever the target block header is already on screen. Clicking
a row in the **Commands** sidebar stays an explicit "take me there" gesture and
still re-pins the block at the top.

Stepping newer past the newest block still exits block selection so the next
`Ctrl+Down` scrolls again, but a multi-block range collapses onto the newest
block first — one stray step can no longer discard a whole range — and the
exit is announced instead of happening silently. Selection movement no longer
re-pins the viewport. Because these context keys
are not bindable commands, they are also listed verbatim in the `Ctrl+Shift+/`
help panel, and every unbound command there (and in the command palette) now
shows the `block:*`-style id to write in `keybindings.toml` instead of the
word “unbound”.

Bindings in `~/.config/ember/keybindings.toml` override defaults. The file is
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

The file is capped at 256 KiB and opened through the same descriptor-based
persistence boundary as Ember's snapshots. A missing file still selects the
defaults; on Unix, a symlink, FIFO/device, multiply linked file, foreign-owned
file, or group/world-writable file is rejected without following or blocking on
the entry. Unsafe and oversized files produce a startup diagnostic and leave
the built-in bindings active.

The in-app help panel is generated from the active bindings, so it reflects
customizations. Copy, paste and keyboard font zoom use this same command path;
there are no separate hard-coded shortcuts that bypass user overrides.

Search results are capped at 20,000 matches to keep broad queries bounded.
When old scrollback must be reflowed after a width change, ember keeps the
results and navigation but suppresses any historical highlight whose raw
coordinate cannot yet be mapped exactly, rather than painting the wrong cell.
Block Mode chrome no longer shares that suppression: the identity viewport now
carries exact per-display-row provenance, so cards, stripes and badges survive
scrolling back over ordinary soft-wrapped output instead of disappearing
exactly when history is being read. Painting and every pointer hit test choose
their chrome through one rule, and both chrome sources agree on card
ownership — which rows belong to which block, and therefore which card a click
selects — including the awkward row a command shares with the next prompt when
its output ended without a newline. The newest input card's six-row floor stays where
it belongs — in the block model's line-id space, shared with the raw path —
and the projected path resolves that end through the viewport rather than
re-deriving a floor in display rows, which collapsed rows and reflow make
meaningless. An end that does not resolve means the card is genuinely clipped,
so it covers the rows it owns and declines to draw a finished bottom edge
rather than guessing one. Three sweeps guard this: a differential comparing
both chrome sources (card ownership, plus the live card's bottom edge) across
a grid of widths, heights, output shapes, screen repaints and scroll offsets
wherever both can answer; an invariant on collapsed projections, where only
one source answers, that the newest card never extends past its own content;
and a top-clipped repro for a running command that repaints the screen.
Restoring chrome here also closes a mouse-routing leak: with no chrome the
pointer path treated the whole grid as application-owned, so mouse events went
to the child program while the user was reading scrollback. One decorative difference is deliberate: a card whose successor's
prompt begins mid-row keeps its closed bottom edge on the raw path but not on
the projected one, which refuses to draw a boundary through output it cannot
prove has ended. That shifts such a card's painted bottom by one gap; row
ownership, and therefore which card a click selects, is identical either way.

## Shell integration (OSC 133)

Block Mode, prompt navigation, semantic command history, completion badges and
failed-command actions require the shell to report command boundaries with
OSC 133. Ember prefers `jsh`, which emits those marks natively. A fallback shell
without integration still works as an ordinary terminal, but it cannot produce
command blocks; block actions and Block Search say so explicitly and point to
**Install or update jsh** in the command palette instead of presenting an empty
query as if nothing matched.

A custom bash/zsh integration needs four marks: `A` before the prompt, `B` after
the prompt, `C` when execution begins, and `D;<exit>` when it ends. Optional
command/cwd/id fields improve replay and lifecycle diagnostics, but they never
authorize execution unless Ember captured them exactly.

## Installing and updating jsh

ember prefers its companion shell [`jsh`](https://github.com/beamiter/jsh) and
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

## Remote hosts and containers

Ctrl+Shift+S opens the remote host picker. An entry in the config file names an
ssh destination or a running container, and choosing one opens it in a new
session:

```toml
[[remote_hosts]]
name = "devbox"
host = "dev.example.com"
user = "yj"
deploy = "persist"
ssh_args = ["-p", "22"]

[[remote_hosts]]
name = "myubuntu"
host = "myubuntu"      # a running container's name
docker = true
deploy = "persist"
```

These two are also what a config file with no `remote_hosts` key starts with:
the two mistakes the grammar cannot forgive are invisible in an empty list —
the port belongs in `ssh_args`, never as `host = "box:22"`, and the login
belongs in `user`, never as `host = "root@box"`. An explicit list wins,
`remote_hosts = []` included, so hosts deleted in the panel stay deleted.

The Remote tab of the Settings panel adds, edits and removes these entries
without editing the file by hand; the less common fields (`ssh_args`,
`session`, `remote_shell`, `deploy_artifact`) stay config-file only and survive
the panel untouched.

Invalid or temporarily incomplete entries are preserved when Settings saves,
so correcting a field never destroys the rest of that host. A single
application-level gate combines the shared connection grammar with bounded,
visually safe text and is rechecked by the picker, connection launcher, and
remote Files backend before any process is started. App-owned length, control,
and visual-format checks run before shared semantic validation, so an unknown
`deploy` draft cannot inject its raw, oversized, or direction-changing value
into an error shown by the UI. The first 128 entries may
be active; later entries still round-trip and remain editable but are shown as
unavailable, and Settings disables Add until the count is back below 128.
Picker and Settings render at most 256 rows at once (so entry 129 remains
visible with its inactive diagnosis); any further drafts stay byte-for-byte in
memory and on disk and the UI reports how many were omitted from that bounded
view. Save feedback separately counts invalid active drafts and retained
over-limit drafts. Runtime-only errors use a neutral bounded host label rather
than inventing an index.

`deploy = "off"` (the default) connects plainly and runs `remote_shell`
(default `jsh`) as found on the destination. `"persist"` and `"incognito"`
bring jsh along through the family's `jsh-remote.sh`: when the local jsh is a
static build — which a Linux install now is — it lends itself, so nothing is
fetched from anywhere and the far side runs exactly the version that sent it.
Persist keeps jsh's dot-files and a cached binary in the destination's `$HOME`
so the next connection skips the transfer; incognito sandboxes `$HOME` and
deletes it on exit — inside a container the sandbox lives in its tmpfs, so
`docker diff` stays empty. An entry the config grammar rejects is shown in the
picker with its reason rather than hidden.

The grammar, validation and argv are shared with the whole jterm family
(`jterm_core::jsh_remote::RemoteHostConfig`); typing `ssh host` or
`docker exec -it name bash` into a jsh prompt reaches the same machinery with
no configuration at all.

The **Files** sidebar browses these hosts natively: a location selector next
to the refresh button switches the tree between **Local** and every configured
`ssh:` / `docker:` entry. Remote listing runs the system `ssh` / `docker`
binaries feeding a small POSIX sh probe script (`sh -s -- <op> ...` on the far
side, script on stdin, arguments single-quote-escaped), the same philosophy as
the jsh-remote launcher — no sshfs, no agent, no extra dependencies, and the
same dotfile/sorting/truncation policy as local scans. Right-clicking a row
(or the current-directory header) offers **New File**, **New Folder**,
**Rename**, **Delete**, **Copy**, **Cut**, **Paste** and **Refresh**, executed
on a bounded background worker against either backend, and switching location
re-roots the tree at the remote home directory once the probe answers. Paste
also works across locations: remote→local **downloads**, local→remote
**uploads**, and remote→remote relays through a unique local temp file —
streamed in 64 KiB chunks with a 512 MiB cap (directories travel as tar
streams, regular files land via a write-then-rename partial file, never
overwriting an existing target, including a dangling symbolic link), with cut
becoming copy-then-delete and any partial success reported as such. While a
transfer runs, the sidebar shows a live busy row (正在下载/上传 … with
transferred bytes, and the total for
uploads) with a ✕ button that cancels it — the in-flight child is killed,
local partial files are cleaned up, and the outcome is reported as a neutral
已取消 rather than an error. The context menu also offers 复制路径, copying
the row's full path (plain, unprefixed for remote rows) to the system
clipboard. Root-level directories use `/` as their tar parent rather than an
empty `-C` operand. The v4 probe refuses directory collisions atomically (`untar
<dir> <name>` exits 17 before extracting) and answers `stat` for cheap remote
existence checks without opening FIFOs or other special leaves for a size read.
You can also drag files and folders from the OS file manager
straight onto the tree: dropping onto a row targets that directory (a file row
targets its parent, blank space the current root), a hover hint shows the
destination, drops are capped at 256 items and 512 MiB total, and the import
runs through the same copy/upload pipeline with progress and cancellation.
The **Hidden** header toggle opts into dot-prefixed entries for both local and
remote trees. Each change starts a fresh generation-stamped root scan, clears
row selection that may no longer be visible, and rejects results issued under
the previous visibility policy.
Rows support multi-select (ctrl+click toggles, shift+click extends a range in
visible order): Delete/Copy/Cut/复制路径 act on the whole selection (delete
asks once with a count and up to five names), batch paste iterates items with
per-item AlreadyExists refusal and a summary status (5 项中 2 项失败：…), and
batch cut deletes only successfully-copied sources. A 🔍 toggle in the Files
header opens a type-to-filter row that prunes the loaded tree client-side
(case-insensitive name substring, matches plus auto-expanded ancestors,
expansion state restored on clear, no new scans — identical for local and
remote listings).

Local Rename and downloaded-file publication use Linux
`renameat2(RENAME_NOREPLACE)`: a destination created by another process after
the friendly existence check wins intact instead of being overwritten by the
commit. On a kernel or filesystem without that atomic primitive, the operation
fails closed rather than falling back to a racy rename.
Transfer staging names are reserved owner-only with exclusive create before
their producer starts, so partial content is not published by a permissive
umask, and a planted hidden symlink is refused without touching its target or
leaving a child process to reap. The downloaded regular file retains that
owner-only mode when its staging inode is published. These private names have
a fixed-size basename independent of the transferred name, so a valid
filesystem-limit name remains transferable; occupied candidates are retried
without unlinking them or starting the producer, and cleanup verifies the
reserved inode before unlinking so a replaced candidate survives.
Downloaded directories are extracted into a private 0700 same-parent directory,
validated for one matching directory root, and only then published with the
same no-replace rename. A concurrently-created destination is never merged
with tar output or removed during cleanup.

Directory refresh is stale-while-revalidate: the last-good rows, expanded
subtrees and pagination remain usable while a new local or remote listing is
in flight. Surviving directories are reconciled in place by path and type;
refresh failures leave that snapshot visible with an inline **Retry** action;
F5 refreshes only when the pointer is over Files and a tree row owns actual
keyboard focus; a terminal, filter/path editor, or popup keeps the key. Every
directory request also carries a per-path revision and a
cancellation token, so a newer request retires queued work and kills the
process group of an older slow SSH/Docker probe instead of merely ignoring its
eventual result. Reconciliation removes vanished selections and revokes delayed
menu/dialog intents before they can act on a stale remote path. When a failed
refresh leaves a last-good snapshot visible, browsing, copying its displayed
path and Retry remain available, while filesystem actions wait for a successful
revalidation so a reused remote pathname cannot target a different object.

The second Remote Files evolution pass also hard-bounds scheduling: at most 64
directory requests may wait behind the two scan workers, and the serialized
filesystem-operation queue has the same 64-item ceiling. Root/navigation work
supersedes the old generation; visible Retry work may jump lazy expansion, but
a bounded burst rule always lets queued lazy work progress. Repeated requests
for one path are physically coalesced, and collapsing a still-loading directory
cancels and removes its invisible probe. The sidebar reports authoritative
pending/queued counts during bursts. Every successful directory snapshot keeps
a monotonic completion timestamp, so Refreshing/StaleError rows disclose the
age of the last-good data. Successful create/copy/rename/upload operations
refresh only their exact materialized parent and focus the confirmed destination
after reconciliation.

The third pass makes remote navigation transactional. Switching endpoints
stages both home discovery and the first root listing; entering a directory, a
breadcrumb, Home/Up, or Back/Forward first scans a generation-stamped
candidate while the current root, selection, expansion state, and
last-good rows remain untouched. Only the matching successful result commits;
a failure or out-of-order completion leaves the old authority/tree usable and
leaves history unchanged. Successful navigation keeps a 32-entry success-only
history and reuses up to eight authority-bound root snapshots before
reconciling the fresh listing. The header provides Back/Forward, clickable
breadcrumbs, and a Ctrl+L absolute-path editor; typed paths are UTF-8/length
bounded, lexically normalized, and reject relative, root-escaping, control, and
bidi-spoofing input. Remote snapshots older than 60 seconds are revalidated
stale-while-revalidate in a five-second, two-directory visible-work budget.
Retryable failures use a capped 1/2/4/8/16/30-second automatic cooldown
(explicit Retry can make one deliberate attempt), while non-retryable failures
do not loop in the background. The sidebar separates queue and execution
latency for the last authoritative scan, and cache entries affected by
filesystem operations are invalidated at their exact materialized directory.

Remote browsing is now independent of terminal input: double-click enters the
directory in the Remote tree, while **↑**/**Home** (or **Alt+Up**/**Alt+Home**
with a focused tree row and the Files panel hovered) navigate the
authority-bound remote root. These
actions never inject `cd` into an unrelated PTY. Remote home output is strict
UTF-8, single-line and absolute. Probe/OS failures shown in the tree are mapped
to stable retry-oriented classes; untrusted diagnostics are single-line,
credential-redacted, control/bidi-cleaned, and truncated on Unicode character
boundaries. With Files hovered, a tree row actually focused, and no
filter/path editor/menu owning focus, Arrow Up/Down move row focus, Left/Right
collapse/expand or enter a child, Enter navigates the focused directory, and
Ctrl+L opens the path editor; terminal/text/popup input is never captured.

The v4 remote list protocol applies the requested hidden-file policy and
`MAX_DIRECTORY_ENTRIES + 1` row ceiling on the far side, preserving an explicit
truncation signal without streaming an unbounded directory over SSH. Symlinks
are classified before directories and therefore never become expandable rows.
Untrusted list output must contain exact UTF-8 basenames; invalid encodings,
oversized operands, dangerous components and duplicate/colliding names are
dropped rather than lossy-decoded into a different operable path.

An interactive terminal that is already running a manually typed `ssh`
command can also move Files to that destination automatically. The authority
is the focused session's real foreground process argv read through `/proc`;
terminal text, titles, OSC command/cwd reports, and Ember-created managed
remote panes are never accepted as proof of an SSH connection. Direct
`ssh TARGET -p 22`, explicit `-S` / `ControlPath`, and the constrained
`jsh-remote.sh` launcher shape are recognized. Ember first runs the remote-home
probe in non-interactive BatchMode while leaving the current tree visible. A
failure therefore keeps the old location unchanged and offers a Retry action
in the toast plus a persistent **Retry SSH Files** control in the Files
header, with the reminder that a key, agent, or live ControlMaster socket is
required. Probe results are consumed only after the frame's Files and pane-
focus interactions, so a same-frame click cannot be overwritten. Exiting SSH
does not force Files back to Local.

Observed ControlPath material is execution-only: the stable saved/temporary
profile identity never contains the live socket. Every scan, operation,
clipboard source, transfer leg, drop, and terminal action freezes the matching
execution overlay. A uniquely matching saved profile is preferred; otherwise
the selector shows an independent `(temporary)` location that survives config
reordering. Saved and temporary forms of the same stable SSH transport are one
filesystem namespace, so Copy/Cut can use direct copy/rename and retain the
live socket instead of relaying. If Files is already on that namespace, an
identical overlay reveals it immediately; a new socket is staged and probed
first, then rebound in place without changing the current root, loaded rows,
or expansion state. A failed or stale upgrade retains both the old tree and
old socket. Long DSW-style labels use safe middle elision (for example
`root@dsw…aliyuncs.com`) while the complete bounded endpoint remains in the
location tooltip.
Observed ControlPath values are replayed only when absolute or in strict
`~/...` form; cwd-relative sockets such as `./cm` are rejected because Ember
cannot safely recover the original SSH process's working-directory semantics.

The Files header also makes the terminal boundary explicit. On **Local**, **Open
terminal here** creates a new interactive tab whose cwd is the current tree
root. On `ssh:` / `docker:`, **Connect terminal (profile default)** opens the
selected profile through the same validated remote launcher as Ctrl+Shift+S;
it intentionally starts in that profile's normal default directory rather than
pretending the independently browsed Files path belongs to the new PTY. A
process-observed temporary SSH location instead offers **Connect terminal (SSH
login)** and opens a plain interactive `ssh -t` login with its validated
connection arguments/live ControlPath, without deploying jsh or inventing a
remote command. A saved SSH location currently rebound to a live ControlPath
uses that same plain-login action and exact socket; only a saved location with
no live overlay uses its configured deploy/default behavior.

Remote Files locations are reconciled by complete profile identity when
Settings adds, removes, edits, or reorders entries. A uniquely moved profile
keeps its tree and file clipboard while its config index is remapped. If the old
profile is missing, changed, duplicated, invalid, or moved beyond the active
limit, Ember fails closed to **Local**, invalidates the old remote selection
and queued/in-flight tree authority, and reports the recovery. The clipboard
source is reconciled independently: a different exact/unique/valid profile is
remapped and retained (with its intent token), while only an unprovable source
is cleared.
A remote-home probe failure follows the same recovery instead of leaving an
empty, unusable remote tree selected.
File-menu actions and the New/Rename/Delete dialogs are stamped with both that
tree generation and its complete location identity. Changing roots or failing
over from an edited remote profile closes/rejects the old intent, so a retained
path cannot later execute against Local or a different host.

File Copy/Cut state also has an intent identity independent of its payload.
Every new Copy or Cut receives a fresh token (even when the selected paths are
identical), and a slow paste may clear or shrink the clipboard only if that
exact token is still current. Safe remote-profile reorders remap the payload
while preserving its token. File operations use a separate location-authority
generation from tree scans, so Refresh or a local cwd/root update cannot leave
a completed transfer's progress/Cancel row behind or suppress safe clipboard
settlement; actually leaving/replacing the backend still rejects all late UI
effects.

## Security notes

- OSC 52 clipboard writes and reads are disabled by default; each direction
  requires its corresponding option to be set to `true` explicitly.
- MIME-aware OSC 5522 data reads are authorized only by a short-lived,
  single-use token created by an actual user paste action. The token is scoped
  to the MIME types announced for that paste.
- Multiline or large text paste asks for confirmation by default.
- Carriage returns are normalized before that policy is evaluated, and UI
  commands place their submit key outside bracketed-paste markers.
- Embedded bracketed-paste terminators are removed before forwarding data.
- Session snapshots enforce size, count, field, layout-depth, and cumulative
  text budgets while decoding, before any shell is restored. Invalid snapshots
  are preserved as side-by-side backups instead of silently replaced.
- Instance locks and execution journals reject symbolic links, hard links and
  non-regular files before mutation.
- Desktop notifications use a bounded worker and always reap or time out the
  external `notify-send` helper.
- Custom-theme names are restricted to one safe filename component; theme
  saves replace symlinks rather than following them outside the theme directory.
- Link targets are shown before opening and require `Ctrl+Click`.
- The two helpers command correction may run automatically (`bash`,
  `apt-cache`) are resolved through `jterm_core::helper`'s trust predicate,
  which refuses a group/world-writable file, a file owned by a third user, and
  any such directory on the path to it. Ember's own predicate, replaced on
  2026-08-29, answered "trusted" for a binary owned by another account at mode
  0755: on a shared machine, a hostile `bash` placed earlier on `PATH` was
  spawned automatically by any failed command. Clamping the child's `PATH` was
  never a defence, because the helper was itself the hostile binary. The same
  predicate was wrong in the other direction under `sudo ember` or in a
  container, where it refused every root-owned system binary and silently
  produced no APT-verified corrections at all; both directions are fixed.
- A proposed correction may not hand a pipeline stage to a shell or interpreter
  the original command did not already feed. The previous check only asked
  whether a pipe character was *present*, so `curl … | head` could be
  "corrected" into `curl … | sh` with no new marker to detect.
- Every string the correction card renders — the model's reason, the failed
  command, and inline errors — is bounded and sanitised by the engine before it
  reaches the card, so a reply carrying a bidi override cannot reorder the text
  drawn beside a pre-filled, auto-focused command field. Destructive drafts are
  labelled rather than drawn in ordinary chrome.

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

`HistoryProjection` keeps an allocation-sharing identity fast path and provides
fail-closed raw/display origins for renderer consumers. In Block Mode, a
session-owned projection policy can splice finished output into synthetic
collapse summaries before the viewport is sliced, with independent projected
scroll anchoring and without mutating terminal history. Filter and Delete are
not implemented.

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
bash scripts/test-install-paths.sh
```

Criterion benchmarks, including deep-scrollback resize/reflow with a forced
viewport cache miss, live in `benches/terminal_benchmark.rs`:

```bash
cargo bench
```

GitHub Actions runs formatting, Clippy, tests and a release build. Please add a
focused regression test for parser, input or persistence changes whenever the
behavior can be exercised without a desktop session.

## License

ember is dual-licensed under **MIT OR Apache-2.0**; pick either at your option.
Full texts are in [`LICENSE-MIT`](LICENSE-MIT) and
[`LICENSE-APACHE`](LICENSE-APACHE). Contributions are accepted under the same
dual terms.
