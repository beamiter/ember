# Native Vim9 IDE

Ember combines a real Vim9 process with native egui workbench controls. Vim
owns text editing, modes, internal splits, registers, undo, completion, syntax
and plugins. Keyboard input goes directly to its PTY. Native buttons use the
shared `jterm_core::vim9` typed channel.

## Start

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

Vim 9 requires `+vim9script`, `+channel`, `+job`, `+timers` and `+quickfix`.
Use `JTERM_VIM9_BIN` to select an executable. The bridge is embedded in the
shared core and prepared automatically. Your existing Vim configuration loads
normally. Vim startup errors remain visible in its terminal grid.

`--ide` starts Vim directly, without first starting a shell, and does not
restore or overwrite the saved terminal layout. Editors opened in an ordinary
workspace coexist with shell tabs and are excluded from shell replay. Save
your files in Vim before exiting; terminal snapshots do not save editor text.

## Controls

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
Snapshots refresh every 100 ms while an editor is open.

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

Quick Open indexes the current project's files in the background using
`rg --files`, including hidden files while honoring ignore files and excluding
`.git`. The index is bounded; scan errors and the file limit are visible.
Type a filename or path fragment, select with Up/Down, and press Enter to open.
The filename itself is preserved, including spaces. **Refresh files** picks
up files added since the previous scan. Cancelled dialogs, navigation keys and
confirmation Enter remain owned by the native UI instead of being sent to
the Vim PTY. File ranking runs only when the query or file set changes.

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

The editing surface uses the existing native GPU terminal renderer. Vim
renders its completion menus and internal windows. Native controls cover
files, buffers, saving, search, builds/tests, quickfix, output and supported
code actions. Debug-adapter UI, a dedicated Git editor and remote editor
bridges are future extensions. Remote Files retain their existing workflow;
this bridge opens local files. Non-UTF8 paths are rejected with a diagnostic.

## Native acceptance test

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
