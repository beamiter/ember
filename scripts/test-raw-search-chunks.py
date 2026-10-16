#!/usr/bin/env python3
"""Compile actual terminal modules with cached Rust deps, without GUI/native linking."""
import argparse,os,subprocess,tempfile
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--deps',type=Path,required=True);p.add_argument('--regression',action='store_true');p.add_argument('--allocation-probe',action='store_true');a=p.parse_args()
r=Path(__file__).resolve().parents[1];deps=a.deps.resolve()
mods=['terminal','block_mode','kitty_graphics','output_batch','review_text','fold_preview','debug']
source='#![allow(dead_code)]\npub use jterm_core::char_width;\n'
for m in mods:
 path=r/'src'/('terminal/mod.rs' if m=='terminal' else m+'.rs');source+=f'#[path="{path}"] mod {m};\n'
if a.allocation_probe: source += (r/'scripts/raw-worker-allocation-probe.rs').read_text()
with tempfile.TemporaryDirectory(prefix='ember-raw-chunks-') as temp:
 t=Path(temp);(t/'test.rs').write_text(source)
 cmd=[os.environ.get('RUSTC','rustc'),'--edition=2021','--test','-C','debuginfo=0',str(t/'test.rs'),'-L','dependency='+str(deps)]
 for crate in ['unicode_normalization','uuid','parking_lot','jterm_core','base64','smallvec','unicode_width','regex','log','image','flate2','serde','serde_json','memchr','libc']:
  choices=list(deps.glob('lib'+crate+'-*.rlib'))
  if not choices: raise RuntimeError((crate,choices))
  choices.sort(key=lambda p:p.stat().st_mtime,reverse=True)
  cmd+=['--extern',crate+'='+str(choices[0])]
 cmd+=['-o',str(t/'tests')];subprocess.run(cmd,check=True)
 subprocess.run([str(t/'tests'),'raw_worker_allocation_probe' if a.allocation_probe else 'search_chunk','--test-threads=1','--nocapture'],check=True)
 if a.regression:
  subprocess.run([str(t/'tests'),'terminal::','--test-threads=1'],check=True)
  subprocess.run([str(t/'tests'),'block_mode::','--test-threads=1'],check=True)
