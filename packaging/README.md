# Shared core source snapshot

An isolated checkout needs the sibling `jterm_core` path dependency. Run
`scripts/materialize-shared-core.sh` before Cargo commands to extract the
SHA-256-verified source snapshot next to the application. Existing sibling
checkouts are preserved, including deliberately checked-out upgrade candidates.

When shared core changes, regenerate the deterministic snapshot with
`scripts/package-shared-core.sh /path/to/jterm_core` and commit both the archive
and its `.sha256` file. The archive contains source/runtime/test assets only;
it excludes Git history, build output, user sessions and credentials.
