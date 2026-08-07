# Engineering handoff

Updated: 2026-08-08

This baseline exact-pins the hardened shared core and jagent revisions and upgrades
Agent review, terminal parsing, configuration, persistence, sidebar/history, links,
and rendering safety. Agent snapshots are now claimed atomically, execution
identities are checked, and terminal-controlled clipboard and link capabilities
fail closed.

## Completed since the previous handoff

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

- `src/persistence_file.rs` gained `claim_exclusive`, a no-clobber
  hard-link/unlink claim, and `src/agent_panel.rs` restores through it. Exactly
  one opener ever observes the snapshot, so two windows opening at once cannot
  both resume the same transcript, and evidence that cannot become a session is
  left at the claim path rather than deleted.
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
