"""Bounded preparation/trigger budgets for the owned synthetic IME collector."""
import json
import math
import time

PROFILE = 'composition-trigger-v1'
PREPARATION_SECONDS = 25
ACTIVE_SECONDS = 15


class CaptureBudget:
    def __init__(self, now):
        self.started = self.last = now
        self.phase = 'capture'
        self.deadline = now + ACTIVE_SECONDS
        self.events = [{'phase': self.phase, 'elapsed': 0.0}]

    def check(self, now):
        if not math.isfinite(now) or now < self.last:
            raise RuntimeError('Invalid capture monotonic clock')
        self.last = now
        if now >= self.deadline:
            raise RuntimeError('Capture ' + self.phase + ' timed out')

    def prepare(self, now):
        self.check(now)
        if self.phase != 'capture':
            raise RuntimeError('Capture preparation is once-only')
        self.phase = 'preparing'
        self.deadline = self.started + PREPARATION_SECONDS
        self.events.append({'phase': self.phase, 'elapsed': now - self.started})

    def commit(self, now):
        self.check(now)
        if self.phase != 'preparing':
            raise RuntimeError('Capture commit requires preparation')
        self.phase = 'commit'
        self.deadline = now + ACTIVE_SECONDS
        self.events.append({'phase': self.phase, 'elapsed': now - self.started})

    def report(self):
        return {'phase': self.phase, 'events': self.events,
                'preparation_limit_seconds': PREPARATION_SECONDS,
                'active_limit_seconds': ACTIVE_SECONDS}


def descriptor(pid):
    return {'profile': PROFILE, 'pid': pid,
            'preparation_seconds': PREPARATION_SECONDS, 'active_seconds': ACTIVE_SECONDS}


def publish_once(path, value):
    if path.exists():
        raise FileExistsError(path)
    pending = path.with_name(path.name + '.pending')
    with pending.open('x') as stream:
        json.dump(value, stream)
    pending.replace(path)


def request(out, phase, pid):
    publish_once(out / (phase + '-request.json'), dict(descriptor(pid), phase=phase))


def acknowledge(out, phase, pid):
    publish_once(out / (phase + '-ready.json'), dict(descriptor(pid), phase=phase))


def require_request(out, phase, pid):
    value = json.loads((out / (phase + '-request.json')).read_text())
    if value != dict(descriptor(pid), phase=phase):
        raise RuntimeError('Invalid capture phase request')


def _ack(out, phase, pid):
    if (out / 'result.json').exists():
        raise RuntimeError('Capture already exited before ' + phase)
    path = out / (phase + '-ready.json')
    if not path.exists():
        return False
    if json.loads(path.read_text()) != dict(descriptor(pid), phase=phase):
        raise RuntimeError('Invalid capture phase acknowledgement')
    return True


def prepare_capture(out, wait_for):
    # The 90s switch collector keeps its existing lifecycle. Empty ready files
    # are its explicit legacy capability; no script-name inference is used.
    raw = (out / 'ready').read_bytes()
    if raw == b'':
        return None
    value = json.loads(raw)
    pid = value.get('pid')
    if type(pid) is not int or pid <= 0 or value != descriptor(pid):
        raise RuntimeError('Unregistered capture budget descriptor')
    if (out / 'live.bin').read_bytes() != b'':
        raise RuntimeError('Capture preparation requires zero-byte proof')
    request(out, 'preparing', pid)
    wait_for(lambda: _ack(out, 'preparing', pid), 'owned capture preparation acknowledgement')
    return pid


def arm_capture(out):
    if (out / 'result.json').exists():
        raise RuntimeError('Refusing Return after capture exit')
    path = out / 'preparing-request.json'
    raw = (out / 'ready').read_bytes()
    if raw == b'':
        if path.exists():
            raise RuntimeError('Legacy capture cannot advertise prepared phases')
        return  # Only an actual empty ready file selects the legacy lifecycle.
    ready = json.loads(raw)
    pid = ready.get('pid')
    if type(pid) is not int or pid <= 0 or ready != descriptor(pid):
        raise RuntimeError('Unregistered capture budget descriptor')
    if not path.exists():
        raise RuntimeError('Capture preparation request is missing')
    value = json.loads(path.read_text())
    if value != dict(descriptor(pid), phase='preparing'):
        raise RuntimeError('Invalid prepared capture descriptor')
    if not _ack(out, 'preparing', pid):
        raise RuntimeError('Capture preparation was not acknowledged')
    request(out, 'commit', pid)
    deadline = time.monotonic() + 1.0
    while not _ack(out, 'commit', pid):
        if time.monotonic() >= deadline:
            raise RuntimeError('Capture did not acknowledge the commit trigger')
        time.sleep(0.01)
    if (out / 'live.bin').read_bytes() != b'':
        raise RuntimeError('Commit trigger requires zero-byte proof')
