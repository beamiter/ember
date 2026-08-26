# Engineering handoff

Updated: 2026-08-26 (Block Search 3.3)

This baseline exact-pins the hardened shared core and jagent revisions and upgrades
Agent review, terminal parsing, configuration, persistence, sidebar/history, links,
and rendering safety. Agent snapshots are now claimed before restore, execution
identities are checked, and terminal-controlled clipboard and link capabilities
fail closed.

## Completed since the previous handoff

- **Block Search 3.3 (2026-08-26)**: continuous review now advances only
  after the selected completed record is revalidated and revealed. Plain Enter
  likewise closes only after success; a concurrently evicted target leaves the
  picker open, reports the stale record, and forces a fresh hit computation.

- **Block Search 3.2 (2026-08-26)**: the virtual result list now supports
  Home/End and bounded ten-row PageUp/PageDown moves in addition to wrapping
  arrows. Its footer reports current position plus total/cap state, and every
  keyboard jump requests one precise scroll-to-selection operation.

- **Block Search 3.1 (2026-08-26)**: `All / Cmd / Out` surface scopes and a
  `Ctrl+O` cycle now restrict matching before the result cap. The same scope
  covers empty-query metadata browsing, and query/filter/index refresh safety
  remains intact.

- **Block Search 3.0 (2026-08-26)**: the bounded cache now combines `Aa`,
  regex, and Unicode whole-word matching, exposed through both pointer controls
  and `Ctrl+I` / `Ctrl+R` / `Ctrl+W`. Boundary filtering is allocation-free per
  line, and case-insensitive whole-word literals use the linear regex engine so
  a line full of rejected partial matches cannot cause quadratic remapping.

- **Single-interpretation native JSON boundaries (2026-08-25)**: after their
  existing raw byte ceilings, the private `auth.json` reader and every Codex
  app-server JSONL record now run through
  `jterm_core::bounded_json::validate_no_duplicate_members` before typed or
  `Value` decoding. Duplicate object members are rejected recursively,
  including escaped-equivalent names and duplicates inside ignored/future
  extension objects. The private serde_json RawValue sentinel is also reserved,
  so feature-unified `Value` decoding cannot reparse unchecked embedded JSON.
  An app-server frame therefore cannot select one `id`,
  `method`, or nested result for request correlation while another decoder or
  audit surface sees a different value; credential parsing likewise has one
  structural interpretation. The shared preflight retains no decoded value
  tree and never reflects the untrusted member name in its error.

- **Shared terminal metadata contracts (2026-08-25)**: jsh session ids now
  delegate to `jterm_core`'s exact grammar and byte bound instead of maintaining
  a local mirror. The Commands sidebar also reuses Block chrome's duration
  formatter, keeping millisecond, second, minute, and hour labels identical.

- Block Mode convergence closes the remaining interaction gaps with Frost.
  Normal text selections in collapsed projections now carry stable raw-cell or
  raw-row endpoint identities across compatible plan updates; width changes,
  column selections, effective-collapse changes, ambiguous reflow and eviction
  still fail closed, including a live-grid trailing blank that is trimmed when
  its row enters history. Block Search adds `Shift+Enter` reveal-and-step without
  closing, uses a fixed-height virtual list that preserves the complete mouse
  scrollbar extent without constructing all 500 possible hit widgets, keeps a
  stationary hover from stealing keyboard traversal, preserves pointer scroll
  when a background refresh retains the exact highlighted row, and
  diagnoses a pane with no OSC 133 marks with the actionable jsh installer path.
  Cold projection planning
  consumes layout streams once and reuses per-group scratch allocations while
  preserving the existing incremental plan, cache-key and revision contracts.
  The shared core exact pin advances to
  `21437ba6f0cb85e74d4ce2a03ef1857de2c55d9d`, adding core-owned Agent
  claim durability and recursive duplicate-member rejection at jagent's JSON
  boundaries without changing `block_contract` semantics.

- Block Search 2.0 replaces Ember's former potentially hundreds-of-megabytes,
  open-time-only cache. Completed records are sampled lazily newest-first under
  an 8 MiB retained source budget and normalized into a 16 MiB retained cache; both
  budgets count Vec storage, stable record-id capacity, original text, and
  lowercase text. The UI says when older blocks were omitted. Finalized-record
  identity is `(len, oldest sequence, newest sequence)`, so same-length deque
  rotation rebuilds the cache and Enter refreshes that identity before acting;
  an old hit cannot bind to replacement history. The picker adds
  All/Failed/Slow/Bookmarked/Background browsing plus `Aa` case matching and a
  bounded Rust-regex mode. Raw query state is rebuilt into a compact allocation
  capped at 4 KiB (plus at most one complete UTF-8 overflow scalar), regex
  compilation at 2 MiB; invalid regexes
  retain the prior usable index/hits but hide and disable them until corrected.
  Lowercase expansion and regex byte ranges map back to original Unicode scalar
  spans, long previews surround the match, and accepted output hits validate the
  complete cached span before revealing its exact physical soft-wrap row;
  collapsed output expands only after that raw anchor proves its owner, with
  stale span → logical-line start → already-revealed block header fallbacks.
  Rebuild drops the prior cache/hit Vec allocations before source extraction,
  preventing old+source+new coexistence. The first rejected source (bounded by
  the 256 KiB per-record output cap) and one rejected lowercase candidate are
  transient allocations outside those retained ceilings. Cache reconstruction
  currently remains
  synchronous on Ember's UI thread, but only on open/finalized-version changes
  and under the two retained ceilings; keystrokes only scan the cache.

- Font file inputs now read through a bounded descriptor boundary
  (`src/font_file.rs`). Every candidate named by `fc-match` output or the
  config file is opened with `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`, validated
  as a regular file through the resulting descriptor, capped at
  `MAX_FONT_BYTES` (64 MiB — desktop fonts are a few MiB, the largest CJK
  collections stay under half of that) before allocation, and read from that
  descriptor only; the path is never reopened, so there is no TOCTOU window.
  `load_font_from_path` and the bold-variant fallback loop skip a rejected
  candidate with a path-rich error and fall through to the next one, so a
  planted symlink, FIFO, device, or oversized file can neither stall nor
  redirect startup. The `persistence_file` ownership/hard-link/permission
  contract is deliberately not reused: system fonts are root-owned and
  world-readable by design. Headless regression tests cover a symlink to a
  real font, a non-blocking FIFO rejection, an oversized sparse file, an
  exact-limit load, and the fallback loop skipping a non-regular candidate.

- The jterm_core pin advances to `b8b1b89`, which routes desktop notifications
  through core's bounded helper boundary. The local `src/review_text.rs`
  duplicate of core's review boundary is dissolved: the visual-spoofing
  character class and its whole-string predicates now come from
  `jterm_core::review_input::{is_visual_spoofing_character,
  contains_visual_spoofing}` at every former call site (agent event/session-ID
  guards, codex app-server display bounding, native follow-up validation, diff
  and command preview neutralization, agent-panel spoof checks). What remains
  in `src/review_text.rs` is ember-only surface policy with no core
  counterpart: the per-surface byte budgets (`MAX_AGENT_COMMAND_BYTES`,
  `MAX_HISTORY_COMMAND_BYTES`, `MAX_PROMPT_INSERT_BYTES`), the parameterized
  `validate_single_line` with its limit-carrying `ReviewTextError`, and the
  multiline `sanitize_prompt_payload` / `sanitize_history_replay` /
  `visible_bounded` helpers, all now delegating their spoof class to core. The
  duplicated full-list test collapses to a wiring/budget smoke test since core
  owns the character-class coverage.

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
  Completion provenance, lifecycle health, and their assessment function are
  now direct public re-exports of the shared contract rather than local semantic
  mirrors. The shared enums retain Ember's existing inherent `schema_name()`
  API, while compatibility helpers keep JSON/diagnostic call sites explicit.

- Agent restore now consumes `jterm_core::agent::SessionClaim`, backed by one
  atomic no-replace rename rather than the former local hard-link/unlink pair.
  Exactly one concurrent opener restores a valid snapshot; malformed, future,
  oversized, and semantically invalid evidence remains byte-identical at its
  private claim path. An empty or rejected local session still leaves the
  public path alone, so one process exiting cannot delete a newer checkpoint
  published by another. Core now syncs retirement of the public name before it
  exposes a live session, so a crash cannot replay an already consumed approval.
  `jterm_core` is pinned to `21437ba6f0cb85e74d4ce2a03ef1857de2c55d9d`
  (transitively jagent `a462ec81f3a4c6ad85a455780ced232172f127ea`).
  Claim-acquisition errors are logged with the public path and leave that path
  untouched; there is no best-effort fallback read or delete.
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

No open boundaries remain; the font-input descriptor boundary recorded above
was the last item in this section.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
bash -n scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
shellcheck scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
bash scripts/test-install-paths.sh
```
