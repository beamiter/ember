# Engineering handoff

Updated: 2026-08-13

This baseline exact-pins the hardened shared core and jagent revisions and upgrades
Agent review, terminal parsing, configuration, persistence, sidebar/history, links,
and rendering safety. Agent snapshots are now claimed before restore, execution
identities are checked, and terminal-controlled clipboard and link capabilities
fail closed.

## Completed since the previous handoff

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
- Session restore now decodes through allocation-charging Serde seeds. Session,
  tag, retained text, tab, layout-node, depth, and cumulative budgets are
  enforced while parsing the 4 MiB-bounded input. Oversized layout branches are
  pruned, and a malformed later tab no longer discards an earlier valid tab.
- OSC 52 clipboard *write* now defaults to false. Missing configuration and new
  installs refuse terminal-driven clipboard writes; an explicit `true`
  round-trips unchanged.
- OSC 8 activation is restricted to absolute HTTP(S) URLs with an authority and
  no userinfo, controls, whitespace, backslash, or visually ambiguous
  characters. `file:`, `ssh:`, `git:`, and `mailto:` targets are no longer
  clickable. Links open through a non-user-writable absolute opener, file
  operands are passed after `--`, and the opener process is reaped.

## Remaining boundaries

### Bound the app-owned helpers and configuration files

The local `fc-list`, `fc-match`, notification, and opener paths still need
trusted helper resolution, process groups, deadlines, and concurrent bounded
pipes. `link.rs` now resolves a trusted absolute opener and reaps it, but the
font and notification helpers do not. Read font inputs through regular-file,
descriptor-based size limits. Test fake PATH entries, descendants holding
pipes, huge output, FIFOs, and oversized files.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
```
