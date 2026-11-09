#!/usr/bin/env bash
# Export and verify the exact Cargo-pinned Git tree, never the working directory.
set -euo pipefail
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
core_dir=${1:-"$repo_dir/../jterm_core"}
python3 - "$repo_dir" "$core_dir" <<'PY'
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import tomllib

repo, core = map(Path, sys.argv[1:])
def git(*args):
    return subprocess.check_output(['git', '-C', str(core), *args])

manifest = tomllib.loads((repo / 'Cargo.toml').read_text())
pins = {manifest['dependencies'][name]['rev'] for name in ('jterm_core', 'jterm_engine')}
if len(pins) != 1:
    raise SystemExit('Core and engine must pin the same exact revision')
revision = pins.pop()
if len(revision) != 40 or git('rev-parse', revision + '^{commit}').decode().strip() != revision:
    raise SystemExit('Expected an exact, available 40-character commit ID')
lock = tomllib.loads((repo / 'Cargo.lock').read_text())
for name in ('jterm_core', 'jterm_engine'):
    packages = [p for p in lock['package'] if p['name'] == name]
    if len(packages) != 1 or not packages[0].get('source', '').endswith('#' + revision):
        raise SystemExit(f'{name} lockfile source does not match the manifest pin')
tree = git('rev-parse', revision + '^{tree}').decode().strip()
scope = ['Cargo.toml', 'Cargo.lock', 'README.md', 'src', 'runtime', 'scripts', 'tests', 'assets', 'crates']
expected = {}
all_blobs = 0
for record in git('ls-tree', '-rz', revision).split(b'\0'):
    if not record:
        continue
    metadata, path = record.split(b'\t', 1)
    mode, kind, oid = metadata.decode().split()
    name = path.decode()
    all_blobs += 1
    if name.split('/')[0] in scope:
        if kind != 'blob' or mode not in ('100644', '100755', '120000'):
            raise SystemExit(f'Unsupported Git member: {name} ({mode} {kind})')
        expected[name] = (mode, oid)

source = git('archive', '--format=tar', revision, '--', *scope)
seen = set()
normalized = io.BytesIO()
with tarfile.open(fileobj=io.BytesIO(source)) as original, tarfile.open(fileobj=normalized, mode='w', format=tarfile.GNU_FORMAT) as output:
    members = sorted(original.getmembers(), key=lambda m: m.name)
    for member in members:
        clean = tarfile.TarInfo(member.name)
        clean.uid = clean.gid = clean.mtime = 0
        clean.uname = clean.gname = ''
        clean.mode = 0o755 if member.isdir() else member.mode
        if member.isdir():
            clean.type = tarfile.DIRTYPE
            output.addfile(clean)
            continue
        if member.name not in expected:
            raise SystemExit(f'Unexpected archive member: {member.name}')
        mode, oid = expected[member.name]
        if member.issym():
            data = member.linkname.encode()
            clean.type = tarfile.SYMTYPE
            clean.linkname = member.linkname
            actual_mode = '120000'
        elif member.isfile():
            data = original.extractfile(member).read()
            clean.size = len(data)
            actual_mode = '100755' if member.mode & 0o111 else '100644'
            clean.mode = 0o755 if actual_mode == '100755' else 0o644
        else:
            raise SystemExit(f'Unsupported archive type: {member.name}')
        actual_oid = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        if (actual_mode, actual_oid) != (mode, oid):
            raise SystemExit(f'Git blob or executable bit mismatch: {member.name}')
        seen.add(member.name)
        output.addfile(clean, io.BytesIO(data) if member.isfile() else None)
if seen != set(expected):
    raise SystemExit('Archive omitted pinned Git blobs')
archive_buffer = io.BytesIO()
with gzip.GzipFile(filename='', mode='wb', fileobj=archive_buffer, mtime=0) as compressed:
    compressed.write(normalized.getvalue())
archive = archive_buffer.getvalue()
digest = hashlib.sha256(archive).hexdigest()
metadata = {
    'published_core_commit': revision,
    'published_core_tree': tree,
    'archive_sha256': digest,
    'archive_members': len(members),
    'archive_files': len(expected),
    'omitted_paths': all_blobs - len(expected),
    'scope': 'Cargo files, README.md, src/, runtime/, scripts/, tests/, assets/ and crates/ (including the independent engine) from the exact pinned Git commit. Other repository files are excluded.',
    'verification': 'Every exported blob hash and executable bit was checked against the exact pinned Git tree. Archive timestamps and owner IDs are normalized. The generator exports Git objects, not working-tree files.',
    'publication_boundary': 'The published_core_* keys identify the Cargo-pinned release commit/tree. This local exporter does not verify remote availability or perform application builds.',
    'source_nar_verification': 'No NAR hash is asserted. This subset archive is not a complete Git export; its SHA-256 is not a recursive NAR hash.'
}
outputs = {
    'jterm-core-src.tar.gz': archive,
    'jterm-core-src.tar.gz.sha256': f'{digest}  jterm-core-src.tar.gz\n'.encode(),
    'jterm-core-revision': (revision + '\n').encode(),
    'jterm-core-origin.json': (json.dumps(metadata, indent=2) + '\n').encode(),
}
for name, data in outputs.items():
    destination = repo / 'packaging' / name
    fd, temporary = tempfile.mkstemp(prefix='.' + name + '.', dir=destination.parent)
    try:
        with os.fdopen(fd, 'wb') as handle:
            handle.write(data)
        os.chmod(temporary, 0o644)
        os.replace(temporary, destination)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
print(f'Packaged {revision} ({len(expected)} verified files), SHA-256 {digest}')
PY
