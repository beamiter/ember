#!/usr/bin/env python3
"""Capture synthetic real foreground PTY bytes; never execute captured input."""
import json
import os
from pathlib import Path
import select
import termios
import time
import tty
from native_ime_capture_budget import CaptureBudget, acknowledge, descriptor, publish_once, require_request

out = Path(os.environ['EMBER_SMOKE_RAW_OUT'])
out.mkdir(parents=True, exist_ok=False)
fd = 0
saved = termios.tcgetattr(fd)
received = bytearray()
metadata = {'pid': os.getpid(), 'shell_pid': os.getppid(), 'tty': os.ttyname(fd)}
budget = CaptureBudget(time.monotonic())

def publish_live():
    temporary = out / 'live.tmp'
    temporary.write_bytes(received)
    temporary.replace(out / 'live.bin')

try:
    tty.setraw(fd)
    publish_live()
    publish_once(out / 'ready', descriptor(os.getpid()))
    while True:
        now = time.monotonic()
        if (out / 'preparing-request.json').exists() and budget.phase == 'capture':
            require_request(out, 'preparing', os.getpid())
            budget.prepare(now)
            acknowledge(out, 'preparing', os.getpid())
        if (out / 'commit-request.json').exists() and budget.phase != 'commit':
            require_request(out, 'commit', os.getpid())
            budget.commit(now)
            acknowledge(out, 'commit', os.getpid())
        budget.check(now)
        if (out / 'finish').exists():
            if budget.phase == 'preparing':
                raise RuntimeError('Capture finish precedes the commit trigger')
            break
        readable, _, _ = select.select([fd], [], [], 0.05)
        if readable:
            chunk = os.read(fd, 4096)
            if not chunk:
                raise RuntimeError('PTY closed during capture')
            received.extend(chunk)
            publish_live()
            if budget.phase == 'preparing':
                raise RuntimeError('Terminal bytes arrived before the composition trigger')
            if len(received) > 16384:
                raise RuntimeError('Synthetic byte capture bound exceeded')
    # Finish is issued only after the input driver has exited. Drain any final
    # bytes that were already queued, without injecting a delimiter/Enter.
    drain_deadline = time.monotonic() + 1
    while select.select([fd], [], [], 0.1)[0]:
        if time.monotonic() >= drain_deadline:
            raise RuntimeError('PTY final drain timed out')
        chunk = os.read(fd, 4096)
        if not chunk:
            raise RuntimeError('PTY closed during final drain')
        received.extend(chunk)
        publish_live()
        if len(received) > 16384:
            raise RuntimeError('Synthetic byte capture bound exceeded')
    metadata['status'] = 'pass'
except Exception as error:
    metadata.update(status='fail', error=str(error))
    raise
finally:
    try:
        termios.tcsetattr(fd, termios.TCSANOW, saved)
    except Exception as error:
        metadata.update(status='fail', restore_error=str(error))
        raise
    finally:
        (out / 'bytes.bin').write_bytes(received)
        metadata['bytes'] = len(received)
        metadata['capture_budget'] = budget.report()
        (out / 'result.json').write_text(json.dumps(metadata, indent=2))
