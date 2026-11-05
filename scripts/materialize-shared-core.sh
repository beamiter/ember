#!/usr/bin/env bash
# Acquire the immutable Cargo-locked core. Historical archives never override it.
# Local candidates require an explicit caller-supplied Cargo --config override.
set -euo pipefail
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
(cd "$repo_dir" && cargo fetch --locked)
printf 'Using Cargo-locked shared core; no sibling checkout created.\n'
