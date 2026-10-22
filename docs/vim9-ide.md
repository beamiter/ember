# Native Vim9 IDE

## Simple plugin workbench

With the updated [beamiter/vimrc](https://github.com/beamiter/vimrc) loaded,
**Ctrl+Shift+P**, then **`>simple`**, exposes the configuration's Simple plugin
actions in the native command picker. Choose files, buffers, grep, symbols,
rename, code actions, Git status/diff/current hunk, Markdown preview, key help,
or the Simple workbench. Availability follows the plugins and current buffer;
an unavailable action explains what it needs.

The picker also shows the Simple configuration's cached Git branch/change
counts, diagnostic counts, remote workspace label and large-file mode when
available. The same context appears in the optional native status bar. No
extra panel is enabled, and the native buffer retains all space when the
picker is closed. Context collection does not start Git, LSP, or remote jobs.

Vim and the Simple plugins continue to own the action's editing behavior and
popups. Ordinary supported completion menus use the native paint path; other
plugin popups and preview views can still use the live compatibility display.
This integration does not turn browser Markdown previews into embedded web
views. The standard Vim configuration also remains usable outside these
terminals.

With the tool and diagnostic panels hidden, **F8** and **Shift+F8** reach Vim's
SimpleTerminal toggle/new mappings. Enabling the native diagnostic panel gives
those keys to its previous/next diagnostic actions.


Ember opens a real Vim9 process with a native egui buffer surface. Optional workbench controls
are hidden by default so the editing surface owns the available space. Vim
owns text editing, modes, internal splits, registers, undo, completion, syntax
and plugins. Keyboard input goes directly to its PTY. Native buttons use the
shared `jterm_core::vim9` typed channel.

The buffer itself is a native egui text surface. Vim supplies buffer text,
logical positions and GUI highlight attributes over the authenticated channel;
Ember shapes and draws those text runs directly. `guifg`, `guibg` and `guisp`
retain exact RGB values even with `notermguicolors` or conflicting `ctermfg`.
Bold uses a real bold face, italic uses native text styling, and underline,
undercurl, strikethrough, cursor and Visual selection are GUI decorations.
The native path does not repaint the buffer from ANSI cells or sample colors
from the terminal palette.

When the optional toolbar is enabled, **GUI buffer · RGB** marks the supported buffer view. Vim's command and status
rows and split separators remain in the compatibility surface between buffer
rectangles. Horizontal, vertical and mixed Vim splits paint every supported
window through the same native RGB path, including inactive windows. Each
window has its own clipping rectangle so italic overhang cannot cover a split
separator or another buffer. Complex views (folds, diff, complex/scrolling completion,
conceal and unsupported shaping) temporarily use **Compatible view**; hover
the label for the runtime's reason. A disconnected bridge or a resized frame
never leaves an old native frame over the terminal. The logical cell geometry
is retained for correct Vim mouse/IME positions; proportional text layout,
arbitrary line heights are not yet implemented. Ordinary fully visible
completion menus use native GUI paint; complex lists retain compatibility.

Native completion menus have a subtle shadow and an inside border that stays
one physical pixel wide when zoomed. Their Vim-owned position, row backgrounds
and text colors remain exact, including when a menu crosses editor splits.
Decoration colors inherit `Normal.guisp` when no explicit special color is set.

Native paint rectangles, wide-character carets and the OS IME candidate anchor
now share the same coordinate mapping. Fractional font metrics and an editor
inside an offset split no longer send the candidate window through a separately
rounded terminal-cell rectangle. Pixel coordinates remain clipped to the owning
buffer window. The shared core also resolves screen cells and buffer byte
positions to revision-bound anchors, providing the coordinate foundation for
future floating documentation, Markdown/image previews and color controls.
Those richer preview widgets are not implemented by this change.

Ctrl+wheel and font-size actions over an editor adjust that editor's font
without changing shell tabs, workbench/sidebar sizes or the saved config.
Missing named font families are rejected by both regular and bold font
resolution, allowing an actual installed monospace fallback to supply matching
advances instead of substituting an unrelated proportional face.

## Start

In an Ember local shell, type the normal Vim command:

```sh
vim src/main.rs
vim +42 'file with spaces.rs'
vim -p first.rs second.rs
```

The shell-expanded arguments, working directory and existing Vim configuration
are preserved. A native editor tab opens in the same Ember window and the
invoking shell waits until Vim exits, then receives the editor's exit status. The shell's command line
is not parsed or rewritten by the GUI. This also means shell quoting, globs and
Vim startup options retain their normal meaning.

Automatic entry is installed only in new local interactive shell environments.
It does not edit your shell files. Explicit command/Agent sessions, remote SSH
commands and noninteractive Vim invocations retain their normal execution.
Aliases or shell profiles that replace `vim`/`PATH` can deliberately bypass the
launcher. Use `JTERM_VIM9_AUTO_GUI=0 vim file` or the absolute Vim executable for
a one-off terminal session; `[vim9] auto_gui = false` disables injection for
new shell sessions. `nvim` remains separate.

Explicit editor launches remain available:

```sh
ember --ide /path/to/project
ember --ide /path/to/project/src/main.rs
ember --ide
```

The last form uses the current directory. A file argument opens that file and
uses the nearest ancestor with a project marker (`.git`, `Cargo.toml`,
`package.json`, `pyproject.toml`, `CMakeLists.txt` or `Makefile`) as its root,
falling back to the file's parent. The **Vim9** title-bar button opens a new
editor at the active terminal's local working directory. The command palette
also has **Vim9 IDE: Open Project**; custom keybindings can target `ide:open`.

The title-bar launcher does not retain keyboard focus. Space and Enter continue
to reach the shell or Vim after opening or switching editor tabs.

Vim 9 requires `+vim9script`, `+channel`, `+job`, `+timers` and `+quickfix`.
For explicit editor launches, use `JTERM_VIM9_BIN` to select an executable.
Shell-launched Vim uses the executable resolved from the shell's `PATH`.
The bridge is embedded in the
shared core and prepared automatically. Your existing Vim configuration loads
normally. Vim startup errors remain visible in its terminal grid.

`--ide` starts Vim directly, without first starting a shell, and does not
restore or overwrite the saved terminal layout. Editors opened in an ordinary
workspace coexist with shell tabs and are excluded from shell replay. Save
your files in Vim before exiting; terminal snapshots do not save editor text.

## Focused buffer defaults

Existing configuration files automatically get the following settings:

```toml
[vim9]
auto_gui = true
native_buffer = true
smooth_scroll = true
toolbar = false
buffer_tabs = false
status_bar = false
project_panel = false
tool_panel = false
```

Set only the panels you want to `true` in `~/.config/ember/config.toml`.
`toolbar` controls the IDE action row, `buffer_tabs` the native buffer strip,
`status_bar` the extra editor metadata, `project_panel` automatic opening of the
Files sidebar, and `tool_panel` the file/search/build fields plus initial
Problems/output panel. The ordinary terminal sidebar remains under its existing
user controls. Vim's own splits, status line, commands, plugins and modified-file
protection continue to work when all optional panels are hidden.

The native buffer is independent of these panels. Set `native_buffer = false`
only to select the compatibility renderer. Ctrl+S, editor font zoom, Quick Open
and Go to line continue to work in the focused editor. Explicit Search/Build
commands open a temporary tool row; Escape from its controls or **Close tools**
hides it again. Search/build results appear only after the action is requested,
and their **Hide** button returns the space to the buffer. Errors remain visible
on demand even when the metadata/status panel is disabled.

## Native scrolling and text detail

Wheel input is coalesced once per frame and targets the Vim split under the
pointer. One wheel notch moves three lines. Trackpad pixel distances are divided
by the actual line height, preserving fractional movement independently in each
split. Fractional X11 line units also follow the device directly. Normal and
Insert mode scroll independently of native click/drag support, including with
`virtualedit=block` or custom primary-pointer mappings. Ctrl+wheel retains editor
font zoom. Input mixed with keyboard activity
keeps its ordinary terminal route so semantic requests cannot overtake text.

Fractions move the native text immediately, using Vim's prefetched source rows
at the split edges. Each split has at most one viewport command in flight;
further wheel motion accumulates locally until that exact command is
acknowledged. The acknowledged topline rebases the remaining fraction instead
of starting a second animation. Vim's file boundaries retire excess movement.
The renderer reads the frame and command completion set atomically, including
when an acknowledgement arrives between input handling and painting.

Small trackpad deltas move content by pixels, including fractional pixels in
layout, without waiting for a whole line. When input reaches the prefetched
row limit, an acknowledgement preserves the position already shown and the
remaining movement catches up on display frames. Pending acknowledgements also
use the display frame clock, so high-refresh displays are not limited by the
16 ms background snapshot poll. This frame loop stops after the request and
remaining motion settle or are cancelled; it adds no second touchpad momentum.

The text, clipped edge rows, selection highlights, caret and IME candidate
anchor use the same viewport translation. A pointer hit uses the frame,
geometry and offsets actually submitted for display, even if a newer frame has
already arrived. New wheel input also checks that the displayed split still
owns the same buffer generation; an unseen replacement buffer cannot inherit
the old picture's wheel gesture.

While a nonzero fraction is displayed, a plain left click and drag use source
line and UTF-8 byte positions when Vim advertises native pointer support.
Pointer commands have one request in flight and coalesce later motion, with
release kept behind the outstanding press/drag. This preserves positions in
wide and combining text and lets Vim own the real Visual selection. At rest,
clicks keep Vim's normal mouse behavior. Modified clicks, unsupported pointer
semantics, or a second nearby click on a fractional image first settle the
image and consume that press and its release. Retry after it settles. A native
first click is not part of Vim's legacy multi-click history; fractional
double/triple-click parity is not claimed. Dragging outside a supported split
retains the last valid source position; edge autoscroll and cross-split source
selection are not implemented.

Keyboard input, preedit, edits, zoom, layout changes, unsupported views, focus
or editor changes cancel the local fraction and any queued pointer target.
A cancelled native press still owns its physical release, so it cannot become
an unrelated terminal mouse report. Once a replacement frame has been drawn,
hidden editors and removed panes stop owning new clicks in their old area.
Set `[vim9] smooth_scroll = false` to disable discrete wheel easing. Precision
devices still move the buffer by pixels. Insert
mode uses a narrow caret, Replace mode an underline, and Normal mode the block
caret.

Undercurls use a continuous antialiased wave with physical-pixel sampling and
shared phase across adjoining syntax runs. Decoration thickness follows display
scale without changing Vim's logical hit-testing geometry.

## Controls

These controls are available when their corresponding panel is enabled or an
explicit command opens the temporary tool row.

| Control | Behavior |
| --- | --- |
| Files / Open | Opens a local file. Relative paths resolve in the project root. |
| Quick Open | Finds project files with fuzzy path matching in an asynchronous index. |
| Go to line | Jumps to a positive line number or `line:column`. |
| Back / Forward | Moves through the active Vim window's real jump list. |
| Buffer strip | Switches by real buffer ID; a dot marks modifications. |
| Buffer × / Close buffer | Closes a real Vim buffer; modified buffers are retained. |
| Windows | Creates side-by-side/stacked Vim windows and moves between listed buffers. |
| Save / Save all | Uses normal Vim write behavior, errors and hooks. |
| Find | Searches the project and populates real quickfix results. |
| Search options | Chooses smart/matching/ignored case, regex, whole-word matching and a file glob. |
| Stop search | Cancels the current search while retaining the existing quickfix list. |
| Build | Runs the entered command asynchronously; blank uses Vim `makeprg`. |
| Save + Build | Writes all modified buffers, then builds only if the writes succeed. |
| Stop build | Cancels the current build job. |
| Problems | Shows diagnostics, search results and recent stdout/stderr. |
| Previous / Next issue | Navigates Vim quickfix. |
| Close editor | Requests Vim exit with modified-buffer checks. |

Click a result to jump to its file, line and column. Build runs only after
clicking **Build**. Configure Vim `errorformat` for compiler-specific output.
Successful commands also retain recent output. Lists and output are bounded;
inspect a truncated quickfix list with `:copen` in Vim. Status shows mode,
file, modifications, line/column, pending actions, build state and errors.
Workbench metadata refreshes at 100 ms while controls or actions are active.
An idle focused buffer reduces that metadata repaint to 500 ms; buffer revision
notifications still wake the renderer immediately, and background editor tabs
do not add a periodic workbench repaint to shell tabs.

The status line also shows the buffer's line count, file type, encoding and
number of Vim windows. Cancelled builds are labelled explicitly. Build output
keeps its latest lines even for large jobs and shows how many lines were
omitted from the native panel.
If Vim is finishing a key sequence, the status line shows the bounded wait
before saving. Pending operators, completion and command lines still need to
be finished before a native action can change editor state.

### Keyboard workflow

These contextual shortcuts apply while the Vim editing grid owns input:

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+P` | Quick Open |
| `Ctrl+Shift+G` | Go to line or line:column |
| `Alt+Left` / `Alt+Right` | Previous / next Vim jump location |
| `Ctrl+S` / `Ctrl+Shift+S` | Save / Save all |
| `Ctrl+Shift+F` | Focus project search; Enter starts searching |
| `Ctrl+Shift+B` | Focus the build command; Enter starts the build |
| `F8` / `Shift+F8` | Next / previous quickfix entry |
| `Ctrl+Shift+PageUp` / `Ctrl+Shift+PageDown` | Previous / next listed buffer |
| `Ctrl+Shift+W` | Close the current buffer with modified-buffer protection |

Vim's `Ctrl+P`, `Ctrl+G`, `Ctrl+W`, `Ctrl+O` and `Ctrl+I` retain their normal meanings. Shell tabs
keep the existing palette, remote picker and pane-close shortcuts. Native
text fields own their input; Escape returns focus to Vim. Opening the build
field never starts a command. The ordinary **Build** button keeps its previous
behavior; writing buffers first requires the explicit **Save + Build** action.

Native keyboard actions preserve their order with Vim input, including an
Escape and Save received in the same input batch. Earlier input drains to the
PTY before the action is sent, and later text waits for that action's reply.
The pending action and text remain bound to their original editor session
when focus or tabs change.

Quick Open indexes the current project's files in the background using
`rg --files`, including hidden files while honoring ignore files and excluding
`.git`. The index is bounded; scan errors and the file limit are visible.
Type a filename or path fragment, select with Up/Down, and press Enter to open.
The filename itself is preserved, including spaces. **Refresh files** picks
up files added since the previous scan. Cancelled dialogs, navigation keys and
confirmation Enter remain owned by the native UI instead of being sent to
the Vim PTY. File ranking runs only when the query or file set changes.

Quick Open also accepts `main.rs:42` or `src/main.rs:42:8` to open the
selected fuzzy match at a line and optional Vim byte column. Use `:42` or
`:42:8` to jump in the current buffer, even while indexing is unavailable.
The picker shows the destination before confirmation; invalid or zero
coordinates keep it open with an explanation. An exact indexed filename
containing colons takes precedence over location syntax, so `notes:42` can
still open that literal file. Enter and clicking a result use the same validated
location. Vim's jump list and unsaved-buffer checks continue to apply.

Type `>` in Quick Open to search the editor command list. `>save`, `>split`,
`>definition` and `>hover` select existing typed Vim actions; no Ex or shell
text is executed. Commands show their shortcuts and explain unavailable
states, including missing language support or an already finished build or
search. Up/Down selects a command and Enter runs it after checking the current
Vim state again. An unavailable action keeps the picker open with an explanation.
Remove `>` to return to files; `:line` and `path:line:column` retain their
existing behavior. Use `./>filename` for a literal filename beginning with `>`.

In multi-window Vim layouts, the active buffer view has a subtle inner boundary
that dims when the editor loses focus. It remains fixed while the buffer scrolls
and stays inside the view, leaving Vim's separator/status rows unchanged.
Problem rows show Error/Warning/Info labels and project-relative locations;
hover shows the full location and message. A completed search with no results
has its own hint only while it owns the current quickfix list and no merged
diagnostics are present.

The keyboard actions above are also listed as **Vim9 IDE: …** in the command
palette and are available for custom bindings: `ide:quick_open`, `ide:goto`,
`ide:save`, `ide:save_all`, `ide:search`, `ide:build`, `ide:previous_buffer`,
`ide:next_buffer`, `ide:close_buffer`, `ide:next_problem` and
`ide:previous_problem`.

Back and Forward share Vim's per-window jump list with native file/line
navigation, diagnostic jumps and the user's Vim commands. They preserve Vim's
modified-buffer checks and display any refusal. Their buttons reflect available
history; the keyboard and command-palette actions resolve the jump inside Vim.
Custom bindings can use `ide:back`, `ide:forward` and `ide:cancel_search`.

When Vim enables `modifyOtherKeys`, shifted printable keys use their actual
OS text character and are sent once. This prevents an input such as `%` from
becoming `5%`. Control/Alt chords and Kitty key encoding retain their existing
paths, and keys without an accompanying text event keep their keycode fallback.

Project search and Quick Open require `rg` (ripgrep) on `PATH`. The result panel shows the
query, progress, match count and search errors. A failed search explicitly
marks retained quickfix entries from an earlier operation. Large searches
report omitted matches; narrow the query to retrieve other results.

The search field has its own wrapping row. **Search options** controls the next
explicit Find/Enter action: searches are literal with smart case by default.
Choose **Match case** or **Ignore case** explicitly, turn on **Regular
expression** and/or **Whole word**, and optionally enter one ripgrep file glob
such as `**/*.rs`, `!vendor/**` or `**/release notes.txt`. Spaces and commas stay
inside that single pattern. Changing an option does not start a search. The
results panel shows the options from the search that actually ran, its progress,
errors or cancellation. **Stop search** cancels the worker and keeps the prior
quickfix list. A quickfix source/title/current-entry indicator makes it clear
when a build or another Vim command has replaced the list; Vim location-list
diagnostics may also appear in the panel.

The **Code** menu uses available `LspGotoDefinition`, `LspHover`,
`LspShowReferences`, `LspFormat` and `LspRename` commands. Definition also
supports Vim tags/tagfunc. Configure your Vim language server plugin and
language server as usual. Missing capabilities disable their controls with
an explanation; the IDE does not install servers automatically.

## Lifecycle and scope

Native tab/pane close asks Vim to quit and keeps the PTY until child exit.
Save modified buffers, or explicitly use `:qa!` in Vim to discard them.
Window close waits for editor exit, so modified buffers retain the window.
Each workbench is bound to its stable terminal session ID; reordering or
closing other tabs cannot redirect an editor action to another PTY.

The native buffer uses egui's text pipeline; Vim still renders unsupported
complex completion menus and unsupported display options in the compatible view. Native controls cover
files, buffers, saving, search, builds/tests, quickfix, output and supported
code actions. Debug-adapter UI, a dedicated Git editor and remote editor
bridges are future extensions. Remote Files retain their existing workflow;
this bridge opens local files. Non-UTF8 paths are rejected with a diagnostic.

## Native acceptance test

The focused GUI-buffer check uses actual window pixels with `termguicolors`
disabled and intentionally conflicting GUI/terminal colors. It also exercises
editing, Ctrl+S, undo, native RGB pixels in vertical/mixed splits, editing in
another active Vim window, resizing those splits, and font zoom. It verifies
the explicit fallback reason when a real Vim horizontal scroll clips a CJK
glyph, recovery after Vim returns to column zero, and fallback/restoration
when an unsupported window-local display option is toggled.
The Save check queues Escape, Ctrl+S and a subsequent edit in one OS batch,
then verifies that Save was handled natively and persisted the text before
that later edit:

```sh
xvfb-run -a -s '-screen 0 1600x1000x24' python3 scripts/native-vim9-buffer-smoke.py \
  --binary target/debug/ember --app ember --output /tmp/ember-native-buffer
```

This check requires xdotool, ImageMagick and Pillow. Screenshots and the result
JSON remain in the chosen output directory. It is an X11/software-GPU check,
not a Wayland, physical-GPU or interactive IME certification.

The focused unit tests include direct fractional paint/overscan, caret and IME
geometry, acknowledgement rebasing, frozen source hit testing, wheel source
identity, and cancelled gesture ownership:

```sh
cargo test --lib --locked vim9_buffer::tests
```

The ignored real-Vim test opens a private PTY and bridge, performs a fractional
click and coalesced drag through CJK/combining text, checks Vim's actual Visual
selection and yank, and verifies that cancellation never replays a release:

```sh
cargo test --lib --locked \
  vim9_buffer::native_input_tests::real_vim_fractional_click_drag_yank_and_cancel_use_the_presented_source \
  -- --ignored --exact --nocapture
```

It requires Vim 9, local sockets and PTY access. This verifies the native input
controller against Vim; desktop trackpad hardware and OS candidate windows
still require interactive validation.

On Linux with Xvfb, libX11/libXtst, ImageMagick, Tesseract and Vim 9 installed:

```sh
python3 scripts/native-vim9-smoke.py \
  --binary target/debug/ember \
  --output /tmp/ember-vim9-smoke
```

The output directory must be new. The test uses private XDG directories and
a synthetic project. It types through XTest into the real Vim PTY, exercises
keyboard Quick Open with a space-containing filename, Go to line, save/save-all,
Back/Forward with a real Vim location oracle, regex/case/whole-word search with a
space-containing glob, search cancellation, dialog cancellation and the explicit build workflow, clicks rendered
Save/Build/Close controls, checks the saved files and visible build output,
verifies modified-buffer protection and graceful exit, and checks
that the saved shell layout is unchanged. Screenshots, OCR, logs and
`result.json` remain available. It uses WGPU's GLES backend by default;
set `WGPU_BACKEND` to choose another available backend. `JTERM_VIM9_BIN` also
applies to this test.


### Recovery validation — 2026-10-20

Pinned shared core: `c387ab33caff52a0bd4f759fb20a35aae2586cd0`.
Passed 15 focused tests (3 library + 12 binary) and the real X11/WGPU window smoke: RGB in all three panes, Unicode, edit/save/undo, resize, font zoom, compatibility fallback/recovery and normal quit.
These runs use Xvfb/software graphics; physical GPU, Wayland and OS IME candidate windows are not certified.

### Direct input validation — 2026-10-21

Pinned shared core: `9dc44d8bb521b4d8ad9435e2bd6e09aecdf090b0`.
The 18 focused native-buffer library tests and the real-Vim PTY selection test
pass. All-target/all-feature `cargo check` and the CI Clippy command with
`-D warnings` pass against that locked Git revision. The CI tests job now runs
the real-Vim regression explicitly after its ordinary test suite.

The PTY test exercises the frontend controller and Vim's real selection/yank;
it does not certify a desktop trackpad, hardware GPU, Wayland compositor or OS
IME candidate window. Those interactive environments remain separate checks.


## Native plugin highlights

Single-line `matchadd()` / `:match` patterns and byte-based `matchaddpos()`
positions now use native GUI RGB paint, including the built-in `matchparen`
plugin. Match priorities, equal-priority insertion order, search precedence,
Visual selection and independent split-window ownership follow Vim. Plugin
matches update and disappear without editing the buffer or emitting a special
invalidation hook. Syntax, properties, text, undo and input remain Vim-owned.

Multiline/context-dependent/zero-width regexes and conceal matches retain an
explicit compatibility reason. Native paint is bounded to 64 matches and 2048
positions per window and 512 match/search ranges per source line; exceeding a
budget shows the real Vim compatibility view instead of partial decoration.

### Plugin validation — 2026-10-21

Pinned shared core: `feae27f7cd965332253a81723095f75850241e81`.
The 18 native-buffer library tests pass (one opt-in real-input test omitted).
The locked development build and Xvfb/software-WGPU buffer smoke pass,
including 43 exact plugin-regex RGB pixels and 37 positional CJK RGB pixels
with `notermguicolors`, removal without editing, same-batch Escape/Save/input,
split edit/save/undo, resize, zoom and compatibility recovery. Screenshots were
inspected. Artifacts: `/tmp/ember-vim9-plugin-feae27f-v2` on the validation host.


## Continuous wheel scrolling — 2026-10-21

Pinned shared core: `2b00c1d3d92f4f5555969d6b62a410866d4acb34`.
Supported Normal-mode buffer views now turn each three-line wheel notch into
a 100 ms continuous input transition. New notches retain their own completion
deadlines; reversing cancels the old direction's unplayed tail. The GUI frame
clock advances motion even without new Vim render messages. Trackpad pixels
continue following OS input directly. Source-checked acknowledgements rebase
paint, hit testing and caret/IME together; a captured press freezes future
wheel animation. Editing, preedit, focus changes, zoom and changed layout
retain their cancellation rules. Turning smooth scrolling off keeps the
existing compatibility behavior.

Render protocol 4 prefetches eight real source rows on each edge (protocol 3
hosts retain two). This gives ordinary notches and short bursts text to expose
while Vim handles the request. File boundaries, unsupported views and missing
margin coverage remain bounded by the actual Vim source.

The shared runtime passes 93 real-Vim regressions. The core Rust Vim9 suite
passes 77 regular checks plus its explicit real-PTY round trip; deterministic
wheel tests cover 60/120/240 Hz timelines, exact distance, acknowledgement
continuity, reversal, file edges and pointer capture.

The vendored Winit 0.30.13 patch ignores legacy X11 wheel-button releases,
which otherwise turn one press/release pair into two notches. See the
[upstream package provenance and patch](../vendor/winit/JTERM_PATCH.md).
The locked build, all-target/all-feature Clippy with `-D warnings`, and full
Xvfb/software-GPU buffer smoke pass. The smoke injects an actual complete X11
wheel notch, observes Vim's topline change from 50 to 53, and captures
intermediate native GUI pixels before checking save/undo, splits, resize,
font zoom and compatibility recovery. PNG encoding is deferred until after
XCB pixel sampling to keep screenshot I/O out of the gesture.

Observed highlight Y positions: 438 → 437 → 383 → 359 pixels. These sparse samples verify actual intermediate positions, not a frame-rate guarantee. Artifacts: `/tmp/ember-vim9-wheel-2b00c1d-v4`.


## Vim-owned native cursor — 2026-10-21

Shared core: `2fa91124b7890acb97b2d628c57b84e92d9b6d38`. The native buffer
now follows Vim's `guicursor` shape, percentage, GUI RGB highlight and blink
timings. Normal/Visual, exclusive Visual, operator-pending, Insert and Replace
have independent settings. Ordered `a:` overlays and linked highlight groups
are preserved; the second highlight applies when language mappings are active
in Insert/Replace. An unspecified group reverses the actual syntax/selection
colors; an explicitly cleared group retains its configured appearance.

A solid block repaints the source glyph in the cursor foreground so Latin,
CJK and combining text remain readable. Thin bars change only cursor paint;
source hit testing and the IME anchor retain their full character rectangle.
Cursor-only updates retain the existing text-shaping cache. Unfocused cursors
use steady outlines. Blink timing follows `blinkwait`, then `blinkoff`/`blinkon`;
any zero value disables blinking. Input, source and mode changes restart the
wait, while unrelated render revisions do not.

For a steady block in Normal/Visual, a 25% insertion bar and a 20% replacement
underline, put this in the usual vimrc:

```vim
highlight Cursor guifg=#20242c guibg=#d8dee9
highlight InsertCursor guifg=#20242c guibg=#73a2e1
set guicursor=n-v:block-Cursor,i:ver25-InsertCursor,r:hor20-Cursor,a:blinkon0
```

Existing frames without `appearance` retain their compatibility cursor.
Command-line, popup and other unsupported views still use Vim's live terminal
surface. Native showmatch timing is not inferred from terminal output.

Validation: locked build, all-target/all-feature Clippy (`-D warnings`), focused
native-buffer tests and the full Xvfb/software-GPU smoke pass. The window test
measured block 12×26, CJK 24×26, insertion 3×26
and replacement 12×8 pixel rectangles, with 34 exact cursor-foreground pixels
under the solid block. XCB samples alternate visible/hidden cursor pixels.
Existing plugin RGB, wheel, edit/save/undo, split, resize, font-zoom and fallback
checks remain passing. Artifacts: `/tmp/ember-vim9-cursor-2fa9112-v3`.
The shared core passes 80 Rust Vim9 checks plus the explicit real-PTY round
trip and 99 real-Vim runtime regressions. These are X11/software-renderer
checks, not a physical-display or Wayland performance certification.


## Native Insert completion — 2026-10-21

Pinned core: `20a1c694792f5737c8b92b32ef02e40f6e2c08d9` (render protocol 5).
Ordinary Insert completion menus now paint above the native GUI buffer, with
Vim's GUI `Pmenu`/`PmenuSel`, kind and extra-column colors. Word/abbreviation,
kind/menu labels, CJK and combining characters use the existing native glyph
path. The menu keeps Vim's screen geometry and padding and can cross split
separators without being clipped to one editing pane.

Vim still owns candidate generation and keyboard/mouse behavior. Ctrl-N/P
select, Ctrl-Y accepts and Ctrl-E cancels through the original PTY; the menu
never takes keyboard or IME focus. Its appearance cancels old buffer scroll
transforms, and completion mouse/wheel input bypasses semantic buffer actions.
Previously captured native gestures retain their cancelled physical tails.
Closing the menu clears the overlay and restores the same source buffer.

The list must fit fully without a scrollbar, use default column order and
contain unclipped ordinary labels. Long/filtered/fuzzy menus, separate info
popups, borders, opacity, custom `pumopt` and distinct `PmenuMatch[Sel]` styles
retain explicit compatibility reasons. Available per-item highlight metadata
is applied, but older Vim builds (including this host's 9.2.0858) omit those
fields from `complete_info()` and expose only standard menu highlight groups.
Hosts using render protocols 1–4 always retain their live compatibility menu.

Shared validation: 82 focused core Rust checks and the explicit real-PTY round
trip pass. The real-Vim runtime suite runs 106 checks: 105 pass, with one
per-item highlight test skipped because this Vim omits that optional metadata.
Menu GUI RGB, Unicode, selection, acceptance/cancellation, cross-split geometry,
row-delta closing and old-protocol/scrolled-menu compatibility are covered.

Ember passes 19 native-buffer checks plus the explicit real-PTY pointer/yank
regression, locked build, all-target/all-feature Clippy (`-D warnings`), fmt
and diff checks. The full Xvfb/software-GPU smoke passes menu normal/selected
RGB, CJK, Ctrl-N/P/Y/E, cross-split overlay and long-menu fallback/recovery,
plus all existing plugin/cursor/wheel/save/undo/split/resize/zoom checks. Its
Pmenu/PmenuSel backgrounds contain 22571/7018 exact GUI RGB pixels; both
buffer panes retain native RGB under the crossing menu. The real-PTY fixture
now waits for parsable yank JSON instead of observing a just-created file.
Artifacts: `/tmp/ember-vim9-completion-20a1c69-v2`. These checks use headless X11
and software rendering, not a physical-display or Wayland performance test.

## Buffer labels and pointer controls

With `[vim9] buffer_tabs = true`, buffer names use the shortest distinguishing
path suffix. For example, two `src/main.rs` files appear as
`client/src/main.rs` and `server/src/main.rs`; full paths remain in tooltips.
Unnamed or equivalent-path buffers retain distinct buffer IDs when needed.
Middle-click a buffer entry to close that exact buffer. Vim refuses unsaved
changes, just as with the existing close button.

Switching buffers reveals the current entry automatically. Refreshing its
modified marker does not pull the list away from a manually browsed position.

During fractional scrolling, cursor shapes retain their complete source-cell
geometry and are clipped at the viewport edge. A partially visible row no
longer compresses or relocates its horizontal caret. Mouse/IME coordinates
continue to use the visible clip.
Foreground recoloring and block-cursor inversion reuse the same native
shaped text; font and DPI changes still invalidate the toolkit's layout cache.

## Native column highlights

`set cursorcolumn` and `set colorcolumn=80,120` remain native in supported
buffer views. Signed color columns follow Vim's `textwidth`; each split keeps
its own guides. Syntax, search, Visual selection, tabs, wide characters and
end-of-line whitespace retain Vim's source and highlight rules. Background
decorations do not become source text or intercept source-position hits.
Block-cursor inversion uses the final foreground/background at the cursor
cell, including syntax over an underlying CursorLine or column background.

## Plugin scratch buffers and per-window palettes

Ordinary `nofile`, `nowrite` and `acwrite` buffers now support native text,
source-pointer gestures and precision scrolling without changing Vim's buffer
type, write callbacks or readonly/modifiable options. Terminal and prompt
buffers retain compatibility rendering.

A local `wincolor` keeps its own resolved Normal palette across text, empty
rows, gutters and scroll margins. Native focus/caret fallbacks use that split's
palette; changing the palette retires old scroll imagery. General
`winhighlight` remapping remains a compatibility case.

## Help and error-list windows

Help, quickfix and location lists now use native text painting for supported
plain views. QuickFixLine follows the list's current entry even when the text
cursor moves independently; global and location-list state remains separate.
These windows keep Vim's own mouse, wheel and keyboard processing, including
Enter, Ctrl-] and double-click activation. The host does not add pixel-scroll
animation to a window that explicitly delegates input to Vim. File/scratch
splits alongside it retain their normal precision scrolling.

Default help concealment and actual wrapped lines still require compatible
display. User options and readonly/nomodifiable settings are preserved.

A newly opened Vim editor inherits the current configured content font size.
Later font-setting changes continue to apply until that editor is explicitly
zoomed; a local zoom then keeps its own size. The same resolved value feeds
keyboard/wheel zoom and both single-pane and split-pane native painting.

Clicking the Vim body immediately confirms the caret, including a click at
its current position. This restarts the original `guicursor` blink wait;
steady cursors and the user's blink timings retain their existing behavior.

## Native visible whitespace

Common `listchars` markers retain native buffer painting. For example:

```vim
set list listchars=tab:»·,trail:·,nbsp:␣,eol:↲
```

Explicit two/three-glyph Tab markers, `space`, `lead`, `trail`, `nbsp` and
`eol` support single-cell symbols. Every marker keeps the original source
bytes for clicks, selection and saving; a line-end symbol is display-only.
Each Vim split retains its own options and GUI highlight colors.
Complex marker fields, default `^I` Tab geometry, active horizontal-edge
markers and wrapped EOL continuations retain compatibility rendering.

## Native inline hints and end-of-line annotations

SimpleCC CodeLens (`:SimpleCCCodeLens`) and virtual diagnostics can keep the
native buffer surface when their end-of-line text properties use the supported
`text_align: 'after'` layout. Their GUI colors, CJK and combining characters
use the same native text painter as source code, clipped to the owning split.
Property updates and removal repaint without changing the buffer or requiring
a source edit.

SimpleCC inline hints (`:SimpleCCInlayHints`) also keep native painting in
supported `nowrap` Normal, Insert and Replace views. The bridge supplies the
hint's exact display columns and shifts subsequent source runs without changing
their UTF-8 byte ranges. Unicode hints, Tab advances, split clipping, GUI colors,
font zoom and the caret/IME candidate position retain the same Vim geometry.
Hint removal restores the source layout without requiring a buffer edit.

Annotations carry no source byte range and remain outside saved text and undo.
Vim owns their lifetime and commands; use `:SimpleCCCodeLensRun` to execute a
lens supplied by the language server. In splits with inline hints, Vim retains
ordinary mouse hit testing while supported source-bound precision scrolling
remains available. The other splits retain their own input capabilities.
Above/below/right alignment, wrapped inline hints, Visual selections with
virtual text, control/bidirectional labels and unsupported clipping keep the
explicit compatibility view. No user display option is rewritten to force
native rendering. `:VimrcWorkbench` reports the authenticated inline capability
separately from native end-of-line text.

`inline_hints_shift_source_and_ime_then_clear_in_both_paint_paths` checks the
actual egui painter in both rendering paths, including multiple font geometries,
source-byte identity, caret/IME placement and removal without a source edit.
