# Ember title-state extraction

The terminal owns one `jterm_engine::titles::TitleState` for window title, icon
title and title stack. OSC 0/1/2 admission still runs Ember's trusted-chrome
sanitizer, including `review_input` visual-spoofing replacement, before passing
text to the engine. App tab labels read the owner, with no duplicate mirror.

Title queries 20/21 use ordered reply effects immediately at the original parser
dispatch position. Replies retain fixed ST and their original control/bidi filter.
Save/restore 22/23 preserves target selection and unconditional pop semantics,
200 Unicode scalar title bounds and a 16-entry stack. Geometry operations
11/13/14/16/18/19 remain in the host. RIS recreates the owner as part of existing
terminal reset; DECSTR and alternate-screen behavior are not redefined.

## Independent original-production evidence

The sealed baseline is from `53170edb13cfc7ea2340a0b4c1046472db1bc11b`, before
consumer migration: 36,184 snapshots, three original-production replays and three
detected negative controls. The logical SHA-256 is
`c9f1ea4440fd05d46ac33531309ce7a0ca6d8fd794961beb906a1a0abc4d072b`;
the two-shard packed SHA-256 is
`08d235c774c108c3444d4e0c72f070dfb52577c220e17688cb654592c7af0baa`.
Provenance, original observers and mutation/replay evidence live under
`tests/fixtures/title_baseline_provenance`. Expected bytes are never regenerated
from the extracted implementation. Existing observers only redirect accesses to
the owner while retaining their original lane labels.

The engine's 118 tests pass Rust 1.86 and 1.89. Consumer tests use stable 1.99.1;
local candidate testing uses an explicit private Cargo patch, not a checked-in
path dependency. This is an Ember title ownership lane, not completion of the
four-terminal architecture upgrade or a change to other frontends' title policy.

Local full-library validation completes with 1,466 passed, 3 known sandbox
trusted-system-Git precondition failures and 13 ignored tests. The failures are
`immutable_task_base_keeps_committed_agent_changes_visible`,
`trusted_git_resolves_to_a_system_owned_absolute_binary` and
`creation_recovery_production_worktree_identity_verification`: this sandbox's `/`
ancestor is not system-owned. No security checks were weakened. An initial
terminal run overlapped the full suite and was SIGKILLed under resource pressure;
heavy suites are subsequently run in separate batches with two test threads.
Strict all-target Clippy, formatting and diff checks pass.
The separate terminal-suite rerun passes: 491 passed, 0 failed, 10 ignored,
including the sealed 36,184-snapshot title golden and prior frozen oracles.

## Published release repin

The release resolves root and font-harness core/engine dependencies from the real
published Git commit `25226a726eb595a723872a1b0381bba7643f2e20`, without a local
patch. Cargo generated both lockfiles. The exact deny allowlist and host-bundle
`public_core_base` use that revision; adapter-source identity and bundled source
bytes are unchanged. The host-bundle check verifies all 24 source files.

The source exporter verified 291 Git files and produced identical results on two
runs. The generated archive SHA-256 is
`18ffaeb53edb2affd54ed0a32d99cf1dbc94ef87ba09ad35ff3993881ee224c0`;
its checksum and revision/origin metadata validate. The root pin gate and seven
self-tests pass. The only remaining textual reference to the preceding core SHA
is the historical color-extraction validation record, not a live dependency.

Published-source strict all-target Clippy and formatting pass. The native-feedback
harness passes formatting, all seven tests and strict Clippy. The engine's
published-source MSRV and terminal/font harness checks are recorded separately
from the earlier private-candidate checks above.
Published engine Rust 1.86 validation passes all 118 tests. The font-group runner
passes production-wiring/extraction checks and 58 Rust tests (14 + 32 + 12).
The published-source terminal suite, run without an override in a separate batch,
passes 491 tests, with 0 failures and 10 ignored, including the immutable title
golden. Font wiring additionally passes 23 Python checks.

The three frozen replay logs retain their original bytes, including their final
blank lines. Exact-path `.gitattributes` entries disable only `blank-at-eof`
whitespace reporting for `replay1.log`, `replay2.log` and `replay3_restored.log`
under `tests/fixtures/title_baseline_provenance`. Production code, other artifacts
and other whitespace checks are unaffected. The complete change set relative to
`07909916ed31de27fc3a97fcc21d5e9a5483dbc4` passes `git diff --check` with these
narrow evidence-preservation attributes, not merely the uncommitted diff check.
