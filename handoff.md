# Engineering handoff

Updated: 2026-08-01

This baseline exact-pins the hardened shared core and jagent revisions and upgrades
Agent review, terminal parsing, configuration, persistence, sidebar/history, links,
and rendering safety.

## Remaining boundaries

### Atomically claim Agent snapshots and bind async work to an epoch

The app still restores with a separate read/remove sequence. Add a namespace-locked
or rename-based one-winner claim in `src/agent_panel.rs` and
`src/persistence_file.rs`; preserve invalid evidence rather than deleting it. Carry
the core session epoch through every Agent effect/completion and replace wrapping
execution generations with checked exhaustion. Test two concurrent openers, stale
completion after New Task, invalid quarantine, and `u64::MAX`.

### Decode sessions while enforcing allocation budgets

`src/session_persistence.rs` currently constructs snapshots and intermediate layout
`serde_json::Value`s before enforcing the 64-session, tag, tab, layout-depth, field,
and cumulative budgets. Replace this path with bounded seeds/visitors and adversarial
wide/deep/cumulative tests.

### Make terminal-controlled capabilities opt-in and URL-safe

Change OSC52 clipboard write so missing configuration and new installs default to
false while explicit `true` round-trips. Restrict OSC8 activation to strict HTTP(S)
URLs with authority and no userinfo, controls, whitespace, bidi, or default-ignorable
characters. Use trusted absolute openers and `--` for file operands.

### Bound app-owned helpers and configuration files

The local `fc-list`, `fc-match`, notification, and opener paths still need trusted
helper resolution, process groups, deadlines, and concurrent bounded pipes. Read font
and keybinding files through regular-file, descriptor-based size limits. Test fake
PATH entries, descendants holding pipes, huge output, FIFOs, and oversized files.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
```
