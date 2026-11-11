# Shared core release repin checklist

Treat a core revision change as a release-wide update, not only a root Cargo
edit. Use the same exact published 40-character revision for every live edge.
Do not replace revision strings in historical fixture provenance or captured
oracle source locks: those identify the original production implementation.

## Update live dependencies and metadata

1. Update root `Cargo.toml` production and development `jterm_core` pins and
   `jterm_engine`, then let Cargo update `Cargo.lock` from the published Git source.
2. Update both `font-group-harness/Cargo.toml` core/engine pins and regenerate its
   separate `Cargo.lock` with Cargo. Do not merely edit the lockfile source string.
3. Update the exact core URL in `deny.toml` `sources.allow-git`. Keep unknown Git
   sources denied; do not widen the allowlist to accommodate a forgotten pin.
4. Update `vendor/host-adapter-bundle.json` `public_core_base`. Preserve
   `adapter_source_core_base`, companion revision, source hashes, and all bundled
   source bytes unless the adapter itself is intentionally being changed.
5. Run `bash scripts/package-shared-core.sh /path/to/core-git-repository` with a
   repository containing the published commit. Commit all four generated outputs:
   `packaging/jterm-core-src.tar.gz`, its `.sha256`, `jterm-core-revision`, and
   `jterm-core-origin.json`. Never hand-edit the archive provenance to pretend an
   old archive contains the new source.

## Required verification before publication

```sh
python3 scripts/check-shared-core-pin.py --self-test
python3 scripts/check-shared-core-pin.py
python3 tools/check-host-bundle.py
cargo fmt --manifest-path tools/native-feedback-harness/Cargo.toml -- --check
cargo test --locked --manifest-path tools/native-feedback-harness/Cargo.toml
cargo clippy --locked --manifest-path tools/native-feedback-harness/Cargo.toml --all-targets -- -D warnings
python3 font-group-harness/run.py
(cd packaging && sha256sum -c jterm-core-src.tar.gz.sha256)
git diff --check
```

Run the exporter twice and compare output hashes for reproducibility. Review
root and harness Cargo lock sources, host `public_core_base`, exact deny allowlist
URL, and packaging revision/origin together. Search the previous revision across
the repository; classify every remaining hit as historical evidence or a live
dependency before committing. Do not rewrite frozen test baselines to pass a
repin. Keep all build output and generated scratch files out of the commit.

Run the affected engine/consumer tests and security source check as well. After
publication, inspect CI for the exact new commit, including Native frame receipt
state, font harness and security jobs; a successful push is not CI acceptance.
