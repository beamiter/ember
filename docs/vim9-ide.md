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
| Buffer strip | Switches by real buffer ID; a dot marks modifications. |
| Save / Save all | Uses normal Vim write behavior, errors and hooks. |
| Find | Searches the project and populates real quickfix results. |
| Build | Runs the entered command asynchronously; blank uses Vim `makeprg`. |
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

Project search requires `rg` (ripgrep) on `PATH`. The result panel shows the
query, progress, match count and search errors. A failed search explicitly
marks retained quickfix entries from an earlier operation. Large searches
report omitted matches; narrow the query to retrieve other results.

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
a synthetic project. It types through XTest into the real Vim PTY, clicks
rendered Save/Build/Close controls, checks the saved file and visible build
output, verifies modified-buffer protection and graceful exit, and checks
that the saved shell layout is unchanged. Screenshots, OCR, logs and
`result.json` remain available. It uses WGPU's GLES backend by default;
set `WGPU_BACKEND` to choose another available backend. `JTERM_VIM9_BIN` also
applies to this test.
