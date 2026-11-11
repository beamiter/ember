# Ember color-state engine extraction

This lane moves Ember's terminal default colors, dynamic foreground/background/cursor
colors, and 256-slot palette overrides into one `jterm_engine::colors::ColorState`.
OSC 4/10/11/12/104/110/111/112 semantics, RGB parsing, palette fallback, theme updates
and color-scheme classification have a single engine implementation. Ember consumes
ordered reply effects immediately. RIS recreates the color owner with the retained
theme without emitting a theme-change notification. Rendering borrows palette state
and reads engine override accessors; it does not maintain a second mirror.

SessionManager's application-theme configuration remains host configuration. OSC
framing, terminal grids, output delivery and rendering remain their existing owners.
This is the Ember consumer migration, not completion of all four terminal consumers
or the wider architecture upgrade.

## Frozen compatibility evidence

The baseline was captured from original production revision
`23f0040401cf92820bcb16b4599efde9bed34759`, before this extraction. Its 61,964
records retain the old observer labels, state values and reply bytes. The candidate
observer only redirects color-field accesses to the engine owner; it does not
regenerate expected results.

- Logical original SHA-256: `f1ba70935de0f091d9b353eb6f1ee66d8ddde6aa32ef8c157c31936bf2d4f23a`
- Packed ordered-shard SHA-256: `04edcd7541838201e585d0505d2898e56a22f82fda2abfa917701255a558dfc7`
- Provenance: `tests/fixtures/color_baseline_provenance/` contains the original
  non-compiling observer copy, source lock, capture report, seal, registration and
  mutation evidence. It also preserves the original sharder and documents the
  transport-only path fix.
- Before migration, two baseline replays and a restored replay passed; deliberately
  dropping query replies, ignoring palette reset and suppressing theme notification
  were each detected. The migrated consumer passes the same sealed golden.

## Validation environment

Initial local consumer validation used Rust stable 1.99.1 and an explicit private
Cargo patch configuration. Final release validation resolves both core and engine
from published Git revision `cf936240e91a933525f30c567370284ff1c2870a`,
without any local override. Cargo generated the matching Git lock entries; the
shared-core pin gate and its seven negative/positive self-tests pass. The engine's complete 112-test suite passes both Rust 1.89 and its declared
Rust 1.86 MSRV. Ember's UI
dependency graph is not claimed to support the engine's lower MSRV.

Consumer checks: sealed golden passes; terminal suite 490 passed, 0 failed,
9 ignored. Full library with `--test-threads=2`: 1,465 passed, 3 failed, 12
ignored. The three failures are existing trusted-system-Git preconditions in this
sandbox (the `/` ancestor is not system-owned), not color comparisons:
`immutable_task_base_keeps_committed_agent_changes_visible`,
`trusted_git_resolves_to_a_system_owned_absolute_binary`, and
`creation_recovery_production_worktree_identity_verification`. Their security checks
remain unchanged. An initial unbounded full-library run was killed with SIGKILL;
the bounded rerun completed without weakening or skipping tests.

Strict `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check`
and `git diff --check` pass. The only observer-only lint adjustment replaces the
hex byte decoder's even-length modulo check with the equivalent low-bit check;
the original observer remains preserved verbatim in provenance.

The published-source rerun (no override) passes the same 490-terminal-test suite,
including the sealed color golden, and locked strict all-target Clippy. The
full-library counts above describe the earlier local-candidate run.
