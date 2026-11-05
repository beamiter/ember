# Workspace V2.1 stability validation

Baseline: Ember `c0b92ca963c3e3a9fcc6293cbe39f16628454c91`.
Shared release: `885324740c61e3b5c9c4549b4bd82100e1d54f7c`.
Date: 2026-11-05. Compiler: Rust 1.99.1, Linux cloud workspace.

## Failures reproduced from the baseline CI

[CI run 41795609083](https://github.com/beamiter/ember/actions/runs/41795609083)
failed Rustfmt, Clippy, tests, font groups, native frame receipts and the release
clipboard gate. The library suite actually ran 1,412 passing tests, three failing
tests and three ignored tests; compiling tests is not counted as running them.

- `wait_timeout` treated the PTY session drain's retryable `WouldBlock` as a fatal
  error. It now retains the unreaped ownership anchor and retries against the same
  deadline. Deterministic retry/deadline tests supplement the real PTY tests.
- Two UI assertions constructed an obsolete center margin/toolbar profile. The
  tests and production renderer now share the center/content geometry helpers,
  while independent layout, origin, clipping and grid-stability assertions remain.
- The library's immersive classifier was private but only consumed by the binary.
  It is now a documented public library export, without suppressing lint warnings.
- `.cargo/config.toml` silently replaced an immutable Git dependency with a sibling
  directory. This contaminated independent harnesses, changed their resolution and
  broke `--locked`; the receipt harness also had no sibling to load. Production
  now resolves the exact published core revision. The lockfile changed only its
  core source identity, not any dependency versions. An explicitly requested local
  candidate can still be selected with the documented command-line Cargo override.

## Checks executed locally

- `cargo fmt --all -- --check`: passed, including the baseline formatting defects.
- `python3 scripts/check-shared-core-pin.py --self-test`: five tests passed,
  including deliberately mismatched pins, missing Git source and hidden overrides.
- `python3 scripts/check-shared-core-pin.py`: manifest, dev dependency, lockfile and
  repository Cargo configuration agree on the exact shared release.
- `python3 tools/check-host-bundle.py`: 24 companion adapter files/checksums passed.
  The existing Eva delegation adapter is unchanged.
- `python3 scripts/test-files-follow.py`: 14 tests passed.
- `python3 font-group-harness/run.py`: production wiring checks and 23 Python
  regressions passed. Its Rust batches passed 14, 32 and 12 tests respectively;
  the latter two batches overlap, so these are not 58 distinct Rust tests.
- Native-feedback harness: seven tests, formatting and strict Clippy passed.
- Standalone clipboard harness: its previous lock failure is fixed; local native
  compilation is blocked by unavailable `xkbcommon.pc`. This is not a hotplug pass.

- `cargo test --locked --lib wait_timeout_ -- --test-threads=1`: six passed.
- `cargo test --locked --lib`: 1,415 passed, three failed, three ignored. Both
  previously failing UI tests and the PTY regression passed. All three remaining
  failures (`agent::diff` trusted Git tests and the recovery worktree test) reject
  the cloud sandbox's non-system-owned `/` ancestor. The security resolver was not
  weakened. A standard system-owned runner must recheck those preconditions.

- `cargo clippy --locked --all-targets -- -D warnings`: passed. This exposed a
  boxed result API adaptation and pre-existing binary-only lints masked by the
  original library lint; the fixes preserve behavior without suppressions.
- Full `--workspace --all-targets --all-features --no-fail-fast` execution also
  exposed three stale binary UI fixtures: compact Inspector font roles, measured
  product-name width, and a Chat projection that omitted production footer/bubble
  settings. These retain exact geometry, font scaling and safe-padding assertions.
  Re-running the complete command against `8853247` after these fixes executed
  1,415 passing library tests and 2,290 passing binary tests. Each target retained
  the same three trusted-Git environment failures described above; library and
  binary ignored counts were three and six. The separate block-search memory test
  passed. Ignored Chat PTY/GPU tests and unconfigured-provider fallbacks are not
  counted as native or provider acceptance.
- The built executable's host restart/draft smoke cannot create its session host
  in this restricted workspace (`Operation not permitted`). The identical isolated
  test is now a CI gate on the normal Linux runner, even if the main suite fails.

Native release and exact published-commit CI results remain pending. This document is an evidence scope record, not a
claim that V2.1 or V3 acceptance is complete.

## Verification boundaries

No physical keyboard, physical GPU, real Chinese IME candidate window, live model
provider, actual SSH target, or interactive Wayland/X11 acceptance was exercised
in this cloud record. Headless egui tests verify layout/input invariants, not those
hardware or native-environment claims. Ignored/skipped/blocked checks remain such.

The historical `packaging/jterm-core-src.tar.gz` is retained for provenance but is
not selected by production Cargo resolution. The materialization entrypoint uses
`cargo fetch --locked` for the production Git pin and creates no sibling directory.
