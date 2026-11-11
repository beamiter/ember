#!/usr/bin/env python3
"""Extract only real ASCII_BASELINE records; never run code or invent expectations.
Usage: capture_ascii_baseline.py LOG CORPUS SOURCE_LOCK OUTPUT
SOURCE_LOCK must be finalized after the exact pre-extraction revision is published.
"""
import hashlib
import json
import pathlib
import sys

def reject_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f'duplicate JSON object key: {key}')
        result[key] = value
    return result

def strict_json(raw):
    return json.loads(raw, object_pairs_hook=reject_duplicate_keys)

def ends(n, schedule, bulk):
    if schedule == 0:
        return [n]
    if schedule == 1:
        return list(range(17, n, 17)) + [n] if bulk else list(range(1, n + 1))
    if schedule > 5:
        return [schedule - 5, n]
    state, at, result = schedule - 1, 0, []
    mask = (1 << 64) - 1
    while at < n:
        state ^= (state << 13) & mask
        state ^= state >> 7
        state ^= (state << 17) & mask
        at = min(n, at + 1 + state % (64 if bulk else 23))
        result.append(at)
    return result

def keys(corpus):
    for p in corpus['profiles']:
        for seed in range(4):
            yield f"{p['id']}_seed_{seed}"
            for case in corpus['cases']:
                if case['profiles'] is not None and p['id'] not in case['profiles']:
                    continue
                seen=0
                for schedule in range(6):
                    yield f"{p['id']}_seed_{seed}_{case['id']}_partition_{schedule}_setup"
                    seen+=1
                    for feed in range(len(ends(len(bytes.fromhex(case['feed_hex'])),schedule,False))):
                        yield f"{p['id']}_seed_{seed}_{case['id']}_partition_{schedule}_feed_{feed}"
                        seen+=1
                assert seen==case['snapshots_per_seed_profile']

def decode(log, corpus):
    expected_keys = iter(keys(corpus))
    result, values, indices = {}, [], {}
    stdout_hash, record_hash = hashlib.sha256(), hashlib.sha256()
    lane_names = None
    finished = False
    for line in log:
        stdout_hash.update(line)
        if b'test result: ok. 1 passed; 0 failed;' in line:
            finished = True
        if not line.startswith(b'ASCII_BASELINE\t'):
            continue
        record_hash.update(line)
        key, lanes = strict_json(line.split(b'\t', 1)[1])
        if key != next(expected_keys, None) or key in result:
            raise ValueError(f'unexpected, duplicate or unordered key {key}')
        if not isinstance(lanes, dict) or not lanes or not all(
            isinstance(k, str) and isinstance(v, str) for k, v in lanes.items()
        ):
            raise ValueError('invalid snapshot lanes')
        if lane_names is None:
            lane_names = sorted(lanes)
            required = {'window_title', 'icon_title', 'title_stack', 'default_colors', 'dynamic_palette', 'dynamic_fg', 'dynamic_bg',
                        'dynamic_cursor_color', 'output_buffer', 'osc_reply_terminator',
                        'protocol', 'grid', 'hidden', 'dirty_region', 'search_source_revision',
                        'parser', 'commands', 'completed'}
            assert required <= lanes.keys()
        if sorted(lanes) != lane_names:
            raise ValueError('snapshot lane set changed')
        result[key] = {}
        for lane, value in lanes.items():
            if value not in indices:
                indices[value] = len(values)
                values.append(value)
            result[key][lane] = indices[value]
    if next(expected_keys, None) is not None or len(result) != corpus['expected_record_count']:
        raise ValueError('incomplete capture')
    if not finished:
        raise ValueError('capture test did not finish successfully')
    return {'values': values, 'cases': result}, {
        'stdout_sha256': stdout_hash.hexdigest(),
        'raw_record_stream_sha256': record_hash.hexdigest(),
        'record_count': len(result), 'lane_names': lane_names,
    }

def main():
    log_path, corpus_path, lock_path, output = map(pathlib.Path, sys.argv[1:])
    lock = strict_json(lock_path.read_text())
    if not lock.get('capture_ready') or not lock.get('capture_base_commit'):
        raise ValueError('source lock is not frozen for capture')
    corpus = strict_json(corpus_path.read_text())
    if hashlib.sha256(corpus_path.read_bytes()).hexdigest() != lock['capture_corpus_sha256']:
        raise ValueError('corpus differs from frozen source lock')
    with log_path.open('rb') as log:
        data, report = decode(log, corpus)
    raw = json.dumps(data, ensure_ascii=False, separators=(',', ':'), sort_keys=True).encode()
    report.update(original_bytes=len(raw), original_sha256=hashlib.sha256(raw).hexdigest(),
                  source_lock_sha256=hashlib.sha256(lock_path.read_bytes()).hexdigest())
    with output.open('xb') as file:
        file.write(raw)
    with output.with_suffix(output.suffix + '.capture.json').open('x') as file:
        json.dump(report, file, indent=2)
    print(json.dumps({k:v for k,v in report.items() if k!='lane_names'}))

if __name__ == '__main__':
    main()
