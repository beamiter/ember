#!/usr/bin/env python3
"""Mutate actual engine semantics against unchanged Ember oracles in private manifests.

Usage: mutate_reset_query_controls.py EMBER_CHECKOUT CORE_CHECKOUT LOG_DIR
       [--suite reset-query|decrqm]
Use isolated source checkouts: files are restored exactly, but runs mutate them briefly.
The archived pre-extraction runner targeted helpers no longer present in production.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('checkout', type=Path)
    parser.add_argument('core', type=Path)
    parser.add_argument('logs', type=Path)
    parser.add_argument('--suite', choices=['reset-query', 'decrqm'], default='reset-query')
    args = parser.parse_args()
    checkout, core, logs = (p.resolve() for p in (args.checkout, args.core, args.logs))
    logs.mkdir(parents=True, exist_ok=True)
    engine = core/'crates/jterm_engine'
    semantic = engine/'src/semantics.rs'
    host = checkout/'src/terminal/parser.rs'
    originals = {path: path.read_bytes() for path in [semantic, host]}
    if args.suite == 'reset-query':
        cases = [
            ('reset_1007', semantic, 'protocol.modes_mut().insert(1007);', 'protocol.modes_mut().remove(&1007);'),
            ('query_default', semantic, '"0m".into()', '"9m".into()'),
        ]
        test = 'reset_query_original_production_goldens_and_all_partitions'
    else:
        cases = [
            ('origin_owner', semantic, '6 => screen.origin_mode,', '6 => protocol.modes().contains(&6),'),
            ('ansi20', semantic, '20 => 4,', '20 => 2,'),
            ('unsupported1016', semantic, '| 1015\n', '| 1015..=1016\n'),
            ('last_parameter', semantic, 'let mode = params.first().copied().unwrap_or(0);', 'let mode = params.last().copied().unwrap_or(0);'),
            ('private_framing', semantic, '"\\x1b[?{};{}$y",', '"\\x1b[{};{}$y",'),
            ('observer', host, '                self.apply_local_semantic_effects(effects);\n                self.observe_chat_native_takeover();', '                self.apply_local_semantic_effects(effects);'),
        ]
        test = 'decrqm_original_production_golden'
    for name, path, old, _ in cases:
        if originals[path].decode().count(old) != 1:
            raise RuntimeError(f'{name}: source anchor must match exactly once')
    env = os.environ | {'CARGO_BUILD_JOBS':'2'}
    env.setdefault('CARGO_TARGET_DIR', str(logs/'target'))
    with tempfile.TemporaryDirectory(prefix='ember-semantic-mutation-') as directory:
        mirror = Path(directory)
        for entry in checkout.iterdir():
            if entry.name in {'.git', 'target'}:
                continue
            target = mirror/entry.name
            if entry.name in {'Cargo.toml','Cargo.lock'}:
                shutil.copyfile(entry,target)
            else:
                target.symlink_to(entry, target_is_directory=entry.is_dir())
        command = ['cargo','test','--manifest-path',str(mirror/'Cargo.toml'),'--offline']
        for name,path in [('jterm_core',core),('jterm_engine',engine)]:
            command += ['--config',f'patch."https://github.com/beamiter/jterm_core.git".{name}.path='+json.dumps(str(path))]
        command += ['--lib',test,'--','--test-threads=1']
        def run(name):
            result = subprocess.run(command,env=env,cwd=checkout,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
            (logs/f'{name}.log').write_text(result.stdout)
            return result
        baseline = run('baseline')
        if baseline.returncode != 0 or '1 passed;' not in baseline.stdout:
            raise RuntimeError('unchanged baseline must pass exactly one selected test')
        results = []
        try:
            for name,path,old,new in cases:
                path.write_text(originals[path].decode().replace(old,new))
                result = run(name)
                path.write_bytes(originals[path])
                if result.returncode != 101 or 'test result: FAILED.' not in result.stdout or 'assertion' not in result.stdout:
                    raise RuntimeError(f'{name}: expected runtime assertion failure; compile failures do not count')
                results.append(f'{name}: actual source mutant killed by unchanged production oracle')
        finally:
            for path,data in originals.items():
                path.write_bytes(data)
        restored = run('restored')
        if restored.returncode != 0 or '1 passed;' not in restored.stdout:
            raise RuntimeError('restored source must pass')
        (logs/'summary.txt').write_text('\n'.join(results)+'\nSources restored byte-for-byte; final test passes.\n')


if __name__ == '__main__':
    main()
