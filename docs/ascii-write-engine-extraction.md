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
`e23a2efa3a1c1faeb1867e7b94ec7ad1c8823b22`, with Cargo-generated Git lockfiles
and no local patch. The exact deny source allowlist and host-bundle public base
match; adapter-source identity and bundled bytes are unchanged. Historical
title documentation and frozen provenance retain their original
source revisions. This repin adds the Frost title profile without changing
Ember production code or frozen expectations.

The source exporter verifies 293 Git files and reproduces identical outputs in
two runs. Archive SHA-256:
`487b2ea6f41851cb56c70b6be643fea49aa4d1137f91bd33ae7bc6bf587ad3d0`.
Archive checksum, pin gate and seven self-tests, host-bundle 24-file checks,
strict all-target Clippy, native-feedback harness seven tests/format/Clippy,
font harness 58 Rust tests plus 23 Python wiring tests, and the complete
`security-check.sh --policy` gate pass. Cargo-audit 0.22.2 with
`--deny warnings` also passes the root, font, native-feedback and EVA harness
lockfiles; vendored standalone lockfiles are not used by these build graphs.

At the original ASCII publication, engine tests on Rust 1.86 passed 122 tests
in a fresh target directory.
An initial reuse of the earlier title-engine target resolved stale API artifacts;
the failure log and clean-target success are retained in validation records.
The published Git source contains the new API. No production workaround was added.

Final published-source terminal validation completes with 493 passed, 0 failed
and 11 ignored (151.93 seconds), including the sealed 70,940-snapshot ASCII
golden and previous frozen suites. The repin leaves production source, adapter
bytes and all frozen fixtures unchanged. Complete diff whitespace checking
against `2360fe7347483e29ca096e17124d7bd08c3109f1` passes with the narrow
artifact attributes above.
