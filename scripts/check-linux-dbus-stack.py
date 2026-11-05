#!/usr/bin/env python3
"""Verify the vendored Linux adapters and their single audited dependency graph."""
import hashlib
import json
from pathlib import Path
import tomllib

ROOT = Path(__file__).resolve().parents[1]
lock = tomllib.loads((ROOT / 'Cargo.lock').read_text())
packages = lock['package']
by_name = {}
for package in packages:
    by_name.setdefault(package['name'], []).append(package)

for obsolete in (
    'zvariant', 'zvariant_derive', 'zvariant_utils', 'zbus_names',
    'zbus-lockstep', 'zbus-lockstep-macros', 'atspi-connection',
):
    assert obsolete not in by_name, f'obsolete/out-of-scope package: {obsolete}'
for name, version in [('zbus', '6.2.1'), ('zbus_macros', '6.2.1'), ('zbus_xml', '6.0.0')]:
    assert [p['version'] for p in by_name[name]] == [version], (name, by_name[name])

workspace = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']
provenance = json.loads((ROOT / 'docs/evidence/linux-dbus-vendor-provenance.json').read_text())
assert len(provenance) == 5
for crate in provenance:
    name = crate['name']
    directory = ROOT / 'vendor' / name
    assert f'vendor/{name}' in workspace['members'], f'{name} test graph is not audited'
    package, = by_name[name]
    assert package['version'] == crate['version'] and 'source' not in package
    original = (directory / 'Cargo.toml.orig').read_bytes()
    assert hashlib.sha256(original).hexdigest() == crate['original_manifest_sha256']
    assert crate['licenses'], f'{name} license texts missing'
    for license_path in crate['licenses']:
        assert (directory / license_path).is_file(), (name, license_path)
    assert not (directory / 'Cargo.lock').exists(), f'{name} has a separate unaudited lock'

print('Linux D-Bus stack: one zbus 6.2.1 graph; five fork test graphs and provenance verified.')
