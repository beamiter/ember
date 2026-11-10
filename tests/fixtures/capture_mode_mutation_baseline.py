#!/usr/bin/env python3
"""Parse genuine ignored-test output. Exclusive creation; never invent expected values."""
import json
import pathlib
import sys

def decode(lines):
    result, values, indices = {}, [], {}
    finished = False
    for line in lines:
        if 'test result: ok.' in line:
            finished = True
        if not line.startswith('MODE_BASELINE\t'):
            continue
        key, lanes = json.loads(line.split('\t', 1)[1])
        if not isinstance(key, str) or key in result:
            raise ValueError('invalid or duplicate snapshot key')
        if not isinstance(lanes, dict) or not lanes or not all(
            isinstance(k, str) and isinstance(v, str) for k, v in lanes.items()
        ):
            raise ValueError('invalid snapshot lanes')
        result[key] = {}
        for lane, value in lanes.items():
            if value not in indices:
                indices[value] = len(values)
                values.append(value)
            result[key][lane] = indices[value]
    if not result or not all(f'seed_{i}' in result for i in range(4)):
        raise ValueError('missing baseline or seeds')
    if not finished:
        raise ValueError('capture test did not finish successfully')
    return {'values': values, 'cases': result}

if __name__ == '__main__':
    with pathlib.Path(sys.argv[1]).open() as log:
        data = decode(log)
    corpus = json.loads(pathlib.Path(__file__).with_name('mode_mutation_corpus.json').read_text())
    records = 4
    for _, input_bytes in corpus:
        n = len(input_bytes)
        partitions = {tuple([n]),tuple(range(1,n+1))} | {tuple([at,n]) for at in range(1,n)}
        records += 4 * sum(map(len,partitions))
    if len(data['cases']) != records:
        raise ValueError('incomplete capture record count')
    with pathlib.Path(sys.argv[2]).open('x') as output:
        json.dump(data, output, ensure_ascii=False, separators=(',', ':'), sort_keys=True)
