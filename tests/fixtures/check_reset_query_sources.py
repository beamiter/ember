#!/usr/bin/env python3
"""Check original source provenance; pass workspace root with original checkouts."""
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1]).resolve()
fixture = pathlib.Path(__file__).parent
proof = json.loads((fixture / 'reset_query_provenance.json').read_text())
for name, expected in proof['sha256'].items():
    path = root / name
    # parser differs only by cfg(test) module when auditing this patched checkout;
    # originals must be supplied, never silently strip or normalize source.
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    assert actual == expected, (name, actual, expected)
frost = (root / 'frost/src/terminal.rs').read_text()
start = frost.index('    fn soft_reset(')
end = frost.index('\n    fn clear_screen(', start)
assert frost[start:end] == (fixture / 'frost_soft_reset_frozen.txt').read_text()
print('PASS: original source hashes and verbatim Frost nonmigration baseline')
