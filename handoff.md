# Engineering handoff

Updated: 2026-08-01

This baseline exact-pins the hardened shared core and jagent revisions and upgrades
Agent review, terminal parsing, configuration, persistence, sidebar/history, links,
and rendering safety. Agent snapshots are now claimed atomically, execution
identities are checked, and terminal-controlled clipboard and link capabilities
fail closed.

## Completed since the previous handoff

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
- OSC 52 clipboard *write* now defaults to false. Missing configuration and new
  installs refuse terminal-driven clipboard writes; an explicit `true`
  round-trips unchanged.
- OSC 8 activation is restricted to absolute HTTP(S) URLs with an authority and
  no userinfo, controls, whitespace, backslash, or visually ambiguous
  characters. `file:`, `ssh:`, `git:`, and `mailto:` targets are no longer
  clickable. Links open through a non-user-writable absolute opener, file
  operands are passed after `--`, and the opener process is reaped.

## Remaining boundaries

### Decode sessions while enforcing allocation budgets

`src/session_persistence.rs` still constructs snapshots and intermediate layout
`serde_json::Value`s before enforcing the 64-session, tag, tab, layout-depth,
field, and cumulative budgets. Replace that path with bounded seeds/visitors —
jterm1's `src/session.rs` now has a worked example for the same shape — and add
adversarial wide, deep, and cumulative tests.

### Bound the app-owned helpers and configuration files

The local `fc-list`, `fc-match`, notification, and opener paths still need
trusted helper resolution, process groups, deadlines, and concurrent bounded
pipes. `link.rs` now resolves a trusted absolute opener and reaps it, but the
font and notification helpers do not. Read font and keybinding files through
regular-file, descriptor-based size limits. Test fake PATH entries, descendants
holding pipes, huge output, FIFOs, and oversized files.

### Carry the epoch into execution effects

`AgentEffect::RunCommand` still correlates on the checked execution generation
alone. That is sufficient today because the panel owns exactly one session, but
carrying `AgentSessionEpoch` alongside the generation would make a stale effect
detectable at the boundary rather than by construction.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
```
