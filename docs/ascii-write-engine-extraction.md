# Ember printable-ASCII write-step candidate

This narrow lane changes only the non-insert ASCII run loop to call
`ScreenState::step_ascii_write`. Autowrap is read once per run. `NeedsWrap`
consumes no input and immediately executes the existing host wrap/history/rebase
chain. `Written` immediately applies the original row mutation before advancing
input. `Stopped` ends the run. There is no second cursor owner or effect queue.

The parser's ASCII-charset eligibility and event/takeover observation cadence,
insert fallback, Unicode/combining handling and REP dispatch remain unchanged.
Geometry, history, output provenance and business lifecycle stay host-owned.
This is not a four-frontend completion claim or a new Unicode writer.

## Sealed original-production evidence

The independent original capture contains 70,940 snapshots. Its canonical JSON
is 967,963,975 bytes, SHA-256
`a748121d6e84c0d2fcf4fde66e4f464eeaf253e453415848913684725f46eef0`.
The lossless canonical gzip is 6,768,207 bytes, SHA-256
`d184a50c0962de6f4990d849496b9098030b67908ffef33e61bc9167e40d0161`,
stored in two ordered shards. Candidate testing never regenerates expected
bytes. Test-only `flate2 = "=1.1.9"` decodes the bounded gzip transport; existing
production dependency versions are unchanged.

Original production passed four replays, including the final bounded loader,
and four negative controls detected pending-latch, consumed-byte, REP-source
and effect-order mutations. Frozen capture and replay observers are preserved
separately in `tests/fixtures/ascii_baseline_provenance`; the consumer replay
remains an additional acceptance gate. The core step candidate passes 122 tests on Rust 1.86 and 1.89; its unit
checks are supplementary, not substitutes for original-production evidence.

Exact-path whitespace attributes preserve the four replay logs' original EOF
blank lines and the original `test-dependency.diff` context-line space. Only
these evidence paths relax their applicable end-of-file/end-of-line formatting
checks; production and other artifacts retain normal whitespace validation.

## Validation and published dependency

Local-candidate focused replay passed all 70,940 snapshots. Heavy runs were
sequential, one process and one test thread: terminal 493 passed, 0 failed,
11 ignored; full library 1,468 passed, 3 failed, 14 ignored. The only full-library
failures are the known trusted-system-Git sandbox preconditions:
`immutable_task_base_keeps_committed_agent_changes_visible`,
`trusted_git_resolves_to_a_system_owned_absolute_binary`, and
`creation_recovery_production_worktree_identity_verification`. No security
checks or expected snapshots were weakened to pass these checks.

The release resolves root and font-harness core/engine from published revision
`4db2e5fe9d8fd6e27856df9b65ccb847147df5b9`, with Cargo-generated Git lockfiles
and no local patch. The exact deny source allowlist and host-bundle public base
match; adapter-source identity and bundled bytes are unchanged. Only historical
title documentation and frozen provenance retain the previous revision.

The source exporter verifies 292 Git files and reproduces identical outputs in
two runs. Archive SHA-256:
`c3ce85c16a4a466360314910290990d1258a58540ee8578fbf43fc8925b2a4e8`.
Archive checksum, pin gate and seven self-tests, host-bundle 24-file checks,
strict all-target Clippy, native-feedback harness seven tests/format/Clippy,
font harness 58 Rust tests plus 23 Python wiring tests, and the complete
`security-check.sh --policy` gate pass.

Published engine tests on Rust 1.86 pass 122 tests in a fresh target directory.
An initial reuse of the earlier title-engine target resolved stale API artifacts;
the failure log and clean-target success are retained in validation records.
The published Git source contains the new API. No production workaround was added.

Final published-source terminal validation completes with 493 passed, 0 failed
and 11 ignored (154.81 seconds), including the sealed 70,940-snapshot ASCII
golden and previous frozen suites. Independent review confirms the full adapter,
provenance, live pins and generated archive. Complete diff whitespace checking
against `a93b37a033218ee2a818dd9adc2cd0db350e6ee5` passes with the narrow
artifact attributes above.
