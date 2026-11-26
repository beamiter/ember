#!/usr/bin/env python3
"""Verify this frontend's frozen host bundle and its immutable public bases."""
import hashlib
import json
import re
from pathlib import Path
import tomllib

root=Path(__file__).resolve().parents[1]
p=root/'vendor/host-adapter-bundle.json'
manifest=json.loads(p.read_text())
assert manifest['schema']==1 and manifest['bundle']=='withdrawal-resume-v1'
frontend_core=manifest['frontend_core_revision']
assert re.fullmatch(r'[0-9a-f]{40}',frontend_core)
remote='https://github.com/beamiter/jcompanion.git'
allowed={'tools/check-host-bundle.py'}
cargo=tomllib.loads((root/'Cargo.toml').read_text())
if cargo['package']['name'] in ['forge','anvil']:
 allowed.update({'src/gtk_close.rs', 'src/gtk_presentation.rs', 'src/gtk_clock_fixture.rs'})
for name in ['companion-host','companion-protocol','companion-client']:
 for item in (root/'vendor'/name).rglob('*'):
  if item.is_file() and not any(part in ['target','__pycache__'] for part in item.relative_to(root).parts):
   allowed.add(str(item.relative_to(root)))
assert allowed==set(manifest['files_sha256']),('unexpected bundle files',allowed^set(manifest['files_sha256']))
for name,want in manifest['files_sha256'].items():
 path=root/name
 assert not Path(name).is_absolute() and '..' not in Path(name).parts and not path.is_symlink()
 assert path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest()==want,name
assert cargo['dependencies']['companion-host']=={'version':'0.1.0','path':'vendor/companion-host'}
patch=cargo['patch'][remote]
assert patch=={name:{'path':'vendor/'+name} for name in ['companion-protocol','companion-client']}
# Exporter origins above remain historical; validate current consumers separately.
for table in ['dependencies','dev-dependencies']:
 for name,dependency in cargo.get(table,{}).items():
  if name.startswith('jterm_'):
   assert dependency.get('git')=='https://github.com/beamiter/jterm_core.git'
   assert dependency.get('rev')==frontend_core and 'path' not in dependency
locked=tomllib.loads((root/'Cargo.lock').read_text())
core_packages=[item for item in locked['package'] if item['name'].startswith('jterm_')]
assert {item['name'] for item in core_packages}=={'jterm_core','jterm_engine','jterm_protocol','jterm_remote_client','jterm_runtime'}
expected_source=f'git+https://github.com/beamiter/jterm_core.git?rev={frontend_core}#{frontend_core}'
assert all(item.get('source')==expected_source for item in core_packages)
assert (root/'packaging/jterm-core-revision').read_text().strip()==frontend_core
origin=json.loads((root/'packaging/jterm-core-origin.json').read_text())
assert origin['published_core_commit']==frontend_core
assert hashlib.sha256((root/'packaging/jterm-core-src.tar.gz').read_bytes()).hexdigest()==origin['archive_sha256']
for package in ['companion-host','companion-client','companion-protocol']:
 data=tomllib.loads((root/'vendor'/package/'Cargo.toml').read_text())
 assert data['package']['name']==package and data['package']['version']==manifest['version']
 for name,dependency in data['dependencies'].items():
  if name in ['companion-protocol','companion-client','companion-core','eva-character-atlas']:
   assert dependency.get('git')==remote and dependency.get('rev')==manifest['public_companion_base'] and 'path' not in dependency
print(json.dumps({'passed':True,'files':len(allowed),'bundle':manifest['bundle'],'public_companion_base':manifest['public_companion_base'],'public_core_base':manifest['public_core_base'],'adapter_source_core_base':manifest['adapter_source_core_base'],'frontend_core_revision':frontend_core,'bundle_manifest_sha256':hashlib.sha256(p.read_bytes()).hexdigest()}))
