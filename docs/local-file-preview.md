# Local file preview (initial Ember slice)

Ctrl + primary click on a detected local file reference opens a read-only right sidebar candidate. Because terminal output can come from a remote proxy even when its foreground process is local, this first slice **does not automatically read the file**. The user selects **Preview local file** after inspecting the candidate and local base directory. Known SSH and non-interactive sessions are rejected. A foreground application with mouse reporting enabled retains the click; the host does not take it for preview.

The base directory is captured from the originating PTY foreground process at pointer press, not from terminal-controlled OSC 7 or the assistant process cwd. A delayed click is cancelled on session changes as before. A preview is shown only for its originating session. File reads and image decoding happen on one worker, so slow filesystems cannot create an unbounded thread queue.

Supported initial scope:

- Existing absolute, `./`, `../`, `~/` references and simple bare source references such as `src/App.tsx:12:3` and `README.md`.
- UTF-8 text including TSX, Markdown, HTML and SVG is displayed as inert selectable source. No JavaScript, shell, HTML/webview, Markdown commands, external opener or remote fetch is invoked.
- PNG raster preview only, matching the existing image dependency features. JPEG/WebP, Markdown rendering, syntax highlighting, line jumping, OSC 8 file URIs and quoted filenames containing spaces are not implemented in this slice.
- Exact filenames win over `:line[:column]` suffix interpretation. Suffix fallback is allowed only on NotFound. Line suffixes identify a file, not a line-jump promise.
- Files must be regular files. Final symlinks are rejected; parent directory symlinks may resolve normally. A single opened descriptor is checked and read, preventing path replacement from retargeting the subsequent read. This is explicit local file selection, not a project-directory sandbox.
- Text is capped at 1 MiB, PNG encoded data at 8 MiB, dimensions at 4096, pixels at 4M, decoder allocation at 64 MiB. Binary and unsafe control characters are rejected. Reads detect files growing past the budget.

Validation: loader and parser unit tests, exact-name/fallback and no-read-before-confirmation tests, detector regression, and full production binary compile. These are not evidence of actual Codex CLI native GUI acceptance or five-terminal rollout.
