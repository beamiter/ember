#!/usr/bin/env python3
"""Explicit ORIGINAL-PRODUCTION capture only. Writes JSON to stdout, not fixtures."""
import json
import pathlib
import sys

if len(sys.argv) > 2 and sys.argv[2] == '--partitions':
    line = next(line for line in pathlib.Path(sys.argv[1]).read_text().splitlines()
                if line.startswith('RESET_QUERY_PARTITIONS\t'))
    print(json.dumps(json.loads(line.split('\t', 1)[1]), indent=2))
    sys.exit(0)

line = next(line for line in pathlib.Path(sys.argv[1]).read_text().splitlines()
            if line.startswith('RESET_QUERY_BASELINE\t'))
raw = json.loads(line.split('\t', 1)[1])
values, indices, cases = [], {}, {}
for name, lanes in raw.items():
    cases[name] = {}
    for lane, value in lanes.items():
        if value not in indices:
            indices[value] = len(values)
            values.append(value)
        cases[name][lane] = indices[value]
print(json.dumps({'values': values, 'cases': cases}, indent=2))
