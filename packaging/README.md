# Shared core source snapshot

The application pins `jterm_core` and the independent `jterm_engine` crate to one
exact Git revision in `Cargo.toml` and `Cargo.lock`.
`scripts/materialize-shared-core.sh` fetches the locked dependencies for current
builds without creating a sibling checkout or extracting this archive.

The SHA-256-verified archive retains a source snapshot of the same release,
including `crates/` so offline/path-based consumers have the engine source too.
`jterm-core-revision` and `jterm-core-origin.json` record its exact Git provenance.

When shared core changes, first update the application's manifests and lockfile,
then run `scripts/package-shared-core.sh /path/to/core-git-repository` with Python
3.11 or newer and Git. The repository must contain the pinned commit. The helper
exports immutable Git objects, ignoring local edits, and rejects mismatched core
and engine pins or lockfile sources. It checks every exported blob and executable
bit against the pinned tree, normalizes archive timestamps and ownership, and
updates the archive, checksum, revision and origin metadata together. Commit all
four outputs together. Repeated runs with the same tooling produce identical
bytes. Verify the checksum with `cd packaging && sha256sum -c
jterm-core-src.tar.gz.sha256`.

The archive contains Cargo files, README, source, runtime, scripts, tests, assets
and crates; it excludes other repository documentation, CI files, Git history,
build output, user sessions and credentials. The checksum covers this compressed
subset archive, not a complete Git export or a recursive Nix NAR. No NAR hash is
asserted. The generator does not verify remote publication or run a Nix, Flatpak
or GUI application build.
