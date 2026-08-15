# Engineering handoff

Updated: 2026-08-15

This baseline exact-pins the hardened shared core and jagent revisions and upgrades
Agent review, terminal parsing, configuration, persistence, sidebar/history, links,
and rendering safety. Agent snapshots are now claimed before restore, execution
identities are checked, and terminal-controlled clipboard and link capabilities
fail closed.

## Completed since the previous handoff

- The jterm_core pin advances to `86661a7` and the app-owned boundaries sink
  into it. `src/helpers.rs` is deleted: font and notification helpers now call
  `jterm_core::helper::{fc_list, fc_match, notify_send}` directly, which carry
  the same fixed candidates, canonical-chain trust, clamped child PATH,
  per-stream byte caps, and single group-killing deadline on top of core's
  `SupervisedChild`. The OSC 8 / link opener policy delegates to
  `jterm_core::link::is_openable_url` (`MAX_OSC8_URI_BYTES` aliases core's
  ceiling); local-path and IP *detection* stays local, and activation still
  routes through the shared policy. The session decoder's text budget and
  borrowed deferred raw fields now come from `jterm_core::bounded_json`
  (`TextBudget`, `DeferredRawField`); only ember's schema, repair counters,
  and seeds remain app-side.

- The source installer accepts `--binary PATH`, allowing release archives, CI
  artifacts, and distro staging to reuse the same path contract without Cargo.
  A real `DESTDIR` install/uninstall test checks all six artifacts, modes,
  launcher paths, escaping, and failure diagnostics. Desktop and AppStream
  validation now run in CI; custom desktop executable paths with
  undefined/unportable `%`, forbidden `=`, or control characters fail
  explicitly. `scripts/install.sh` and `scripts/uninstall.sh` now derive the
  default binary from the same `PREFIX/bin` contract (`~/.local/bin` by
  default), and CI checks the scripts with Bash and ShellCheck.

- The app-owned font and notification helpers now run behind a bounded process
  boundary (`helpers.rs`). `fc-list`, `fc-match`, and `notify-send` resolve only
  from fixed absolute system candidates whose canonical file and every directory
  above it are root-owned (or self-owned read-only) and never group/other
  writable; a non-root user's own owner-writable component fails closed. Each
  invocation leads its own process group, drains stdout and stderr concurrently
  under independent byte caps over nonblocking `poll`, and answers to one
  deadline that SIGKILLs and reaps the whole group — a descendant holding an
  inherited pipe cannot outlive it. Exit observation uses `waitid(WNOWAIT)`, so
  the leader stays waitable and its PID/PGID reserved until cleanup; a recycled
  group id can never be signalled. The notification worker's private
  kill-and-wait helper is gone, and text link detection is absolute HTTP(S)
  only: `ftp://` is no longer clickable. Regression tests cover untrusted
  scratch-path binaries, mutable/foreign ownership, concurrent dual-stream
  draining, per-stream caps, WNOWAIT observability, and a deadline killing a
  descendant that holds both pipes.

- Native Codex startup now has a bounded, cancellable background preparation
  phase. Git/worktree identity checks, descriptor pinning, launcher validation,
  prompt construction, and private `CODEX_HOME` setup no longer run on the UI
  thread. A `TaskId` plus local generation and the still-current sharing/redaction
  policy gate completion; the task remains `Created + Unassigned` until the UI
  thread receives the exact current result, while cancellation, stale state,
  and revoked consent drop their FDs, in-memory credential grant, and temporary
  home without spawning a provider. Preparation workers remain globally bounded
  and joined through cleanup even across rapid start/cancel cycles. The provider
  worker repeats descriptor-backed Git identity and trusted launcher checks just
  before spawn. Direct Agent terminals and native compatibility fallbacks can
  both retry after an unsuccessful PTY exit; a successful new PTY atomically
  replaces the exited transcript binding while sticky provenance is preserved.

- The experimental Tasks dashboard now has a live multi-turn Codex app-server
  runtime. It sends explicitly shared, bounded failed-command evidence over
  Codex's structured JSONL protocol, reduces correlated lifecycle events into
  task state, and retains bounded agent/command/file views. A completed turn
  becomes a live review point: bounded user feedback starts another sequential
  turn on the same provider thread with identical cwd, sandbox, environment,
  approval, and containment authority. Duplicate/overlapping turns are rejected,
  a later turn invalidates earlier validation evidence, and **Finish Codex** is
  required before validation can start. Live sessions are capped at 32 turns so
  all completed provider turn IDs remain authoritative tombstones for the whole
  session. The dashboard projects the current/latest turn alongside a compact,
  byte-budgeted completed-turn history keyed by Ember-local turn identities;
  oldest summaries are evicted explicitly while the worktree diff remains
  cumulative. This is still process-local review state, not a durable task
  transcript. Once that session stops, native restart and cross-process thread
  resume remain disabled. The provider runs
  in a descriptor-pinned isolated worktree and transient user-systemd cgroup;
  Ember publishes its terminal event only after the cgroup is empty and the
  leader is reaped. File-change requests require an immutable, completely
  displayable patch snapshot. Approval policy is fixed to `never`; any managed
  request is display-and-deny only because accepted command or file actions are
  not yet bound to Ember's descriptor capability. The provider gets a private
  empty `CODEX_HOME`, an access-token-only external login, and a pre-thread
  effective-config proof that rejects inherited MCP, hooks, plugins, apps,
  project trust, and managed authority. Hosted search and tool network access
  are disabled; the audited 0.147.0 protocol is version-gated, and tool
  subprocesses get a separate no-login, proxy-free environment with a vetted
  absolute PATH. Native failure keeps the worktree available for an explicit
  PTY compatibility continuation. If a later turn fails after an earlier review
  point, that review state remains available and the fully stopped failed session
  can atomically bind a terminal fallback without losing the old state on spawn
  failure. Native session resume remains future work.

- Agent tasks that are ready for review can now re-run their originating
  semantic command as a separate validation terminal inside the isolated
  worktree. Validation requires exact, untruncated, single-line command
  metadata; maps a canonical source-repository subdirectory to the matching
  canonical worktree directory; and rejects missing directories, control or
  bidi-spoofing text, and symlink escapes before spawn. Agent and validation
  PTYs have distinct stable roles and exit reducers, so failed, inconclusive,
  or manually cancelled validation never turns into an Agent runtime failure.
  The Tasks card retains attempt/result state and exposes validation output,
  rerun, diff review, and an explicit pass-gated Mark complete action. Spawn is
  gated before PTY creation while a native event stream is active; one live
  native session may carry sequential turns but cannot restart after stopping;
  and validation uses non-login shell
  command mode plus no-rc/scrubbed startup hooks. The actual source-session
  shell identity is captured before config hot reloads, Git registration and
  branch identity are rechecked, and a descriptor-pinned cwd is carried from
  preflight to child `fchdir` to close pathname replacement races.

- `keybindings.toml` no longer has an `exists()`/path-reopen race. It is opened
  once through Ember's no-follow, nonblocking descriptor boundary, checked as
  an owned single-link regular file, and bounded to 256 KiB before allocation
  or TOML parsing. Missing files still use defaults; unsafe, invalid UTF-8, and
  oversized entries report a path-rich error and cannot stall startup.

- Completed command records now map the pinned `jterm_core::block_contract`
  result into Ember's renderer-owned `BlockOutcome`; Prompt/Running lifecycle
  states remain local. The call happens only after OSC metadata and screen
  fallback have populated the canonical `CommandRecord`, and an explicit
  truncated-command fact still counts as command presence without exposing its
  omitted text. Failure markers/navigation and Commands-sidebar filtering/status
  all classify the raw optional exit before any legacy scalar adapter, so
  background+nonzero and command+unreported cases stay consistent.

- Agent restore now consumes `jterm_core::agent::SessionClaim`, backed by one
  atomic no-replace rename rather than the former local hard-link/unlink pair.
  Exactly one concurrent opener restores a valid snapshot; malformed, future,
  oversized, and semantically invalid evidence remains byte-identical at its
  private claim path. An empty or rejected local session still leaves the
  public path alone, so one process exiting cannot delete a newer checkpoint
  published by another. `jterm_core` is pinned to
  `48d25f155b960417609ffc85a98b7c9ba44c5772` (transitively jagent
  `a09fd1563b862f96bed7047834720aeb31c163e2`). Claim-acquisition errors are
  logged with the public path and leave that path untouched; there is no
  best-effort fallback read or delete.
- The in-flight model request records the task generation it was started for; a
  reply that lands after New Task, a restore, or a session replacement is
  dropped instead of being applied to a transcript that no longer exists.
- Execution generations use `checked_add`; exhaustion seals the session instead
  of reusing an identity a late completion could bind to.
- Session restore now decodes through a schema-aware bounded RawValue decoder.
  The v1–v4 envelope and every nested child are borrowed as
  `serde_json::value::RawValue` slices of the 4 MiB-bounded input, and each
  short-lived parser is finished and dropped before the decoder follows its
  children, so no owned tree ever exists before sanitization. Session, tag,
  retained text, tab, layout-node, depth, and cumulative budgets are enforced
  while parsing; sessions past the retained prefix are schema-validated
  without being retained; an unsupported version short-circuits before any
  payload is decoded. Oversized layout branches are still pruned rather than
  dropped, invalid tabs decode transactionally without losing valid
  neighbours, the active-tab index is remapped onto the input tab it
  originally named, and an invalid `tabs` field falls back to the legacy
  `layout` tree. Required known fields stay strict (including inside discarded
  surplus sessions) while unknown fields, however long their keys, remain
  forward-compatible.
- OSC 52 clipboard *write* now defaults to false. Missing configuration and new
  installs refuse terminal-driven clipboard writes; an explicit `true`
  round-trips unchanged.
- OSC 8 activation is restricted to absolute HTTP(S) URLs with an authority and
  no userinfo, controls, whitespace, backslash, or visually ambiguous
  characters. `file:`, `ssh:`, `git:`, and `mailto:` targets are no longer
  clickable. Links open through a non-user-writable absolute opener, file
  operands are passed after `--`, and the opener process is reaped.

## Remaining boundaries

### Read font inputs through bounded descriptors

The helper *processes* are now bounded (see above), but the font files their
output names are still read with `std::fs::read` in `load_font_from_path` and
the fallback-font loop. Those paths come from fontconfig and the config file,
so they still need regular-file, descriptor-based size limits (no-follow open,
`fstat` regular-file check, byte cap) before allocation. FIFOs and oversized
files behind a font path remain untested.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
bash -n scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
shellcheck scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
bash scripts/test-install-paths.sh
```
