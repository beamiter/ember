#!/usr/bin/env python3
"""Deterministic synthetic output through the real foreground PTY, no app API."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import time

WIDTH = 96
RAW_LINES = 100000
SNAPSHOT_LINES = 2000
SNAPSHOTS = 64


def line(mode, index, record=0):
    if mode == 'raw':
        token = {16: 'E03EARLYHIT', 50000: 'E03MIDDLEHIT',
                 99950: 'E03LATEHIT'}.get(index, 'E03FILLER')
    elif mode == 'snapshot':
        token = f'E03SNAPSHOT{record:02d}' if index == 16 else 'E03CAPTURED'
    else:
        token = 'E03FLUSH'
    prefix = f'{token} ROW{index:06d} '
    return (prefix + '.' * (WIDTH - len(prefix)) + '\n').encode('ascii')


def emit(mode, record, output, manifest, require_tty=True):
    if mode not in ('raw', 'snapshot', 'flush') or not 0 <= record < SNAPSHOTS:
        raise ValueError('Invalid bounded fixture mode/record')
    tty = None
    columns = None
    if require_tty:
        if not os.isatty(output.fileno()):
            raise RuntimeError('Fixture requires real PTY stdout')
        tty = os.ttyname(output.fileno())
        columns = os.get_terminal_size(output.fileno()).columns
        if columns < 100:
            raise RuntimeError(f'Fixture would wrap at {columns} columns')
    count = {'raw': RAW_LINES, 'snapshot': SNAPSHOT_LINES, 'flush': 256}[mode]
    digest, total = hashlib.sha256(), 0
    started = time.monotonic_ns()
    # Bounded output chunks; never construct a 100k-line string.
    for start in range(0, count, 128):
        chunk = b''.join(line(mode, i, record) for i in range(start, min(start + 128, count)))
        output.write(chunk)
        digest.update(chunk)
        total += len(chunk)
    output.flush()
    result = dict(mode=mode, record=record, lines=count, bytes=total,
                  sha256=digest.hexdigest(), started_ns=started,
                  finished_ns=time.monotonic_ns(), pid=os.getpid(),
                  shell_pid=os.getppid(), tty=tty, columns=columns)
    # Exclusive creation: repeating a command cannot masquerade as one action.
    with manifest.open('x') as stream:
        json.dump(result, stream, indent=2)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['raw', 'snapshot', 'flush'])
    parser.add_argument('record', type=int, nargs='?', default=0)
    args = parser.parse_args()
    root = Path(os.environ['EMBER_E03_FIXTURES'])
    emit(args.mode, args.record, sys.stdout.buffer,
         root / f'{args.mode}-{args.record:02d}.json')


if __name__ == '__main__':
    main()
