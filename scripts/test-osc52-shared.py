#!/usr/bin/env python3
"""Compare actual native OSC52 adapters to frozen admission with no host UI link.

Requires cached Cargo metadata. JTERM_ENGINE_SOURCE/JAGENT_SOURCE allow isolated
source-overlay validation; BASE64_SOURCE can select a cached codec; normal runs resolve the locked source dependency.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
def item(source, marker):
    start = source.index(marker)
    brace = source.index('{', start)
    depth, end = 1, brace + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]

metadata = None
def package(name, override):
    global metadata
    if override in os.environ:
        return Path(os.environ[override])
    if metadata is None:
        metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--offline', '--locked', '--format-version=1'], cwd=ROOT))
    return Path(next(p['manifest_path'] for p in metadata['packages'] if p['name'] == name)).parent
engine = package('jterm_engine', 'JTERM_ENGINE_SOURCE')
jagent = package('jagent', 'JAGENT_SOURCE')
base64 = package('base64', 'BASE64_SOURCE')
source = (ROOT / ('src/terminal/state.rs' if (ROOT / 'src/terminal/state.rs').exists() else 'src/terminal.rs')).read_text()
methods = '\n'.join(item(source, 'fn '+name+'(') for name in ['decode_base64', 'handle_osc_52', 'original_handle_osc_52', 'osc52_clipboard_text_is_unsafe', 'take_osc52_clipboard_set', 'take_osc52_clipboard_query'])
predicate = item((jagent / 'src/safety.rs').read_text(), 'pub fn is_unsafe_invisible_char(').replace('is_unsafe_invisible_char', 'is_visual_spoofing_character')
code = '''#![allow(dead_code)]
extern crate self as jterm_engine;
use base64::Engine as _;
#[macro_export] macro_rules! debug_log { ($($arg:tt)*) => {{}}; }
'''
code += '#[path = '+json.dumps(str(engine / 'src/clipboard.rs'))+'] pub mod clipboard;\n'
code += 'mod jterm_core { pub mod review_input { '+predicate+' } }\n'
code += 'struct TerminalState { osc52: clipboard::Osc52Clipboard, osc_reply_terminator: &\'static [u8] }\n'
code += 'impl TerminalState { fn new(_:usize,_:usize)->Self { Self { osc52: Default::default(),osc_reply_terminator:b"\\x1b\\\\" } }\n'+methods+'\n}\n'
code += '#[path = '+json.dumps(str(ROOT / 'src/terminal/osc52_oracles.rs'))+'] mod osc52_oracles;\n'
with tempfile.TemporaryDirectory(prefix='osc52-native-oracle-') as temporary:
    root = Path(temporary)
    (root/'Cargo.toml').write_text('[package]\nname="native_osc52_oracle"\nversion="0.0.0"\nedition="2021"\n[dependencies]\nbase64={path='+json.dumps(str(base64))+'}\n[workspace]\n')
    (root/'src').mkdir()
    (root/'src/lib.rs').write_text(code)
    subprocess.run(['cargo','test','--offline','--manifest-path',str(root/'Cargo.toml'),'--','--nocapture'], check=True, env=dict(os.environ,CARGO_TARGET_DIR=str(root/'target'),CARGO_INCREMENTAL='0'))
