# Shared core source snapshot

The application pins `jterm_core` to an exact Git revision in `Cargo.toml` and
`Cargo.lock`. `scripts/materialize-shared-core.sh` fetches that locked dependency
for current builds without creating a sibling checkout.

The SHA-256-verified archive retains a source snapshot of the same published
revision. For path-based consumers, the helper extracts the snapshot next to the
application and preserves existing sibling checkouts. `jterm-core-revision` and
`jterm-core-origin.json` record the archive's published Git provenance.

When shared core changes, regenerate the deterministic snapshot with
`scripts/package-shared-core.sh /path/to/exact-published-core`. The source
directory must contain the exact published commit without local edits. Commit
the archive, its `.sha256` file, the revision, and the updated origin metadata
together. The archive contains Cargo files, README, source, runtime, scripts,
tests and assets; it excludes Git history, build output, user sessions and
credentials.
