#!/usr/bin/env python3
"""Explicit local mutation controls; restores original parser in finally."""
from pathlib import Path
import os, subprocess, sys
checkout=Path(sys.argv[1]).resolve(); logs=Path(sys.argv[2]).resolve(); logs.mkdir(parents=True, exist_ok=True)
path=checkout/'src/terminal/parser.rs'; original=path.read_text()
env=os.environ | {'CARGO_BUILD_JOBS':'2'}
cargo='cargo'
results=[]
try:
 for name,before,after,test in [
  ('reset_1007','self.protocol.modes_mut().insert(1007);','self.protocol.modes_mut().remove(&1007);','reset_query_frozen_host_differential'),
  ('query_default','"0m".into()','"9m".into()','reset_query_exact_sgr_bytes')]:
  if name == 'reset_1007':
   start=original.index('    fn soft_reset('); end=original.index('    fn decrqm_private_mode_state(',start)
  else:
   start=original.index('    fn decrqss_sgr_payload('); end=original.index('    fn sgr_color_params(',start)
  body=original[start:end]
  assert body.count(before)==1,(name,body.count(before))
  path.write_text(original[:start]+body.replace(before,after)+original[end:])
  with (logs/f'mutant-{name}.log').open('w') as out:
   result=subprocess.run([cargo,'test','--manifest-path',str(checkout/'Cargo.toml'),'--locked','--offline','--lib',test,'--','--test-threads=2'],stdout=out,stderr=subprocess.STDOUT,env=env)
  assert result.returncode==101,(name,result.returncode)
  log=(logs/f'mutant-{name}.log').read_text()
  assert 'test result: FAILED.' in log and 'assertion' in log,name
  results.append(f'{name}: deliberately mutated production, expected assertion failure confirmed (exit 101)')
finally:
 path.write_text(original)
(logs/'mutation-results.log').write_text('\n'.join(results)+'\nProduction parser restored byte-for-byte.\n')
