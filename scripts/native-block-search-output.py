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


PILOT_MARKERS = {
    'raw': ('TARGET PAPAYA', 'NEIGHBOR PAPAYA'),
    'snapshot': ('TARGET WALNUT', 'NEIGHBOR WALNUT'),
}
PILOT_COLUMN = 36
PILOT_LIVE_MARKER = 'LATEST CEDAR'
# Keep the live positive control clear of the card border and following prompt.
PILOT_LIVE_ROW = 248


def line(mode, index, record=0, observer_pilot=False):
    if mode == 'raw':
        token = {16: 'E03EARLYHIT', 50000: 'E03MIDDLEHIT',
                 99950: 'E03LATEHIT'}.get(index, 'E03FILLER')
    elif mode == 'snapshot':
        token = f'E03SNAPSHOT{record:02d}' if index == 16 else 'E03CAPTURED'
    else:
        token = 'E03FLUSH'
    prefix = f'{token} ROW{index:06d} '
    marker_row = observer_pilot and record == 0 and (
        (mode in PILOT_MARKERS and index in (16, 17)) or (mode == 'flush' and index == PILOT_LIVE_ROW))
    if observer_pilot and mode in PILOT_MARKERS and record == 0 and index in (16, 17):
        prefix = prefix.ljust(PILOT_COLUMN) + PILOT_MARKERS[mode][index - 16] + '    '
    elif observer_pilot and mode == 'flush' and record == 0 and index == PILOT_LIVE_ROW:
        prefix = prefix.ljust(PILOT_COLUMN) + PILOT_LIVE_MARKER + '    '
    # Quiet padding keeps sparse-text OCR from joining these independent
    # control words to a dense dot leader. Width and source budgets stay fixed.
    quiet_live_neighbor = (observer_pilot and mode == 'flush' and record == 0
                           and abs(index - PILOT_LIVE_ROW) <= 1)
    padding = ' ' if marker_row or quiet_live_neighbor else '.'
    return (prefix + padding * (WIDTH - len(prefix)) + '\n').encode('ascii')


def emit(mode, record, output, manifest, require_tty=True, observer_pilot=False):
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
        chunk = b''.join(line(mode, i, record, observer_pilot) for i in range(start, min(start + 128, count)))
        output.write(chunk)
        digest.update(chunk)
        total += len(chunk)
    output.flush()
    result = dict(mode=mode, record=record, lines=count, bytes=total,
                  sha256=digest.hexdigest(), started_ns=started,
                  finished_ns=time.monotonic_ns(), pid=os.getpid(),
                  shell_pid=os.getppid(), tty=tty, columns=columns)
    if observer_pilot:
        result['observer_pilot'] = True
    # Exclusive creation: repeating a command cannot masquerade as one action.
    with manifest.open('x') as stream:
        json.dump(result, stream, indent=2)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['raw', 'snapshot', 'flush'])
    parser.add_argument('record', type=int, nargs='?', default=0)
    parser.add_argument('--observer-pilot', action='store_true')
    args = parser.parse_args()
    root = Path(os.environ['EMBER_E03_FIXTURES'])
    emit(args.mode, args.record, sys.stdout.buffer,
         root / f'{args.mode}-{args.record:02d}.json', observer_pilot=args.observer_pilot)


if __name__ == '__main__':
    main()
