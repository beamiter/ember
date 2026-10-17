#!/usr/bin/env python3
"""Hosted Block Search idle-PTY proof; record only, never execute received bytes."""
import json
import os
from pathlib import Path
import select
import termios
import time
import tty

out = Path(os.environ['EMBER_SMOKE_RAW_OUT'])
out.mkdir(parents=True, exist_ok=False)
fd = 0
saved = termios.tcgetattr(fd)
received = bytearray()
metadata = {'pid': os.getpid(), 'shell_pid': os.getppid(), 'tty': os.ttyname(fd)}

def publish_live():
    temporary = out / 'live.tmp'
    temporary.write_bytes(received)
    temporary.replace(out / 'live.bin')

try:
    tty.setraw(fd)
    publish_live()
    (out / 'ready').touch()
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        if (out / 'finish').exists():
            break
        readable, _, _ = select.select([fd], [], [], 0.05)
        if readable:
            chunk = os.read(fd, 4096)
            if not chunk:
                raise RuntimeError('PTY closed during capture')
            received.extend(chunk)
            publish_live()
            if len(received) > 16384:
                raise RuntimeError('Synthetic byte capture bound exceeded')
    else:
        raise RuntimeError('Capture timed out')
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
        temporary = out / 'result.tmp'
        temporary.write_text(json.dumps(metadata, indent=2))
        temporary.replace(out / 'result.json')
