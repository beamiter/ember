"""Owned synthetic wtype keyboard with a fail-closed one-Return stdin barrier."""
import re
import subprocess


KEYBOARD_ARGS = ['wtype', '-s', '250', '-M', 'shift', '-m', 'shift',
                 '-', '-k', 'Return', '-s', '600000']


def modifier_roundtrip_complete(log):
    """Require Shift mask 1 -> 0 on one object, then its exact sync reply."""
    keyboard = None
    released = False
    callback = None
    # An incomplete trailing line must never be evidence.
    for line in log.splitlines(keepends=True):
        if not line.endswith('\n'):
            continue
        modifier = re.search(r'-> zwp_virtual_keyboard_v1([@#]\d+)\.modifiers\((\d+), 0, 0, 0\)', line)
        if modifier:
            obj, mask = modifier.groups()
            if keyboard is None and mask == '1':
                keyboard = obj
            elif keyboard == obj and mask == '0' and not released:
                released = True
            else:
                return False
            continue
        if re.search(r'-> zwp_virtual_keyboard_v1[@#]\d+\.key\(', line):
            return False
        if released and callback is None:
            sync = re.search(r'-> wl_display[@#]\d+\.sync\(new id wl_callback([@#]\d+)\)', line)
            if sync:
                callback = sync.group(1)
                continue
        if callback and re.search(r'(?<!-> )wl_callback' + re.escape(callback) + r'\.done\(\d+\)', line):
            return True
    return False


class CompositionKeyboard:
    def __init__(self, smoke):
        self.trace = smoke.out / 'ime-composition-keyboard.log'
        log = self.trace.open('w')
        smoke.logs.append(log)
        env = smoke.env.copy()
        env['WAYLAND_DEBUG'] = 'client'
        self.process = subprocess.Popen(KEYBOARD_ARGS, env=env,
                                        stdin=subprocess.PIPE, stdout=log, stderr=log)
        smoke.processes.append(self.process)
        self.released = False

    def ready(self):
        if self.process.poll() is not None:
            raise RuntimeError('Composition keyboard exited before readiness')
        return modifier_roundtrip_complete(self.trace.read_text())

    def release_return(self, preedit_marker, capture):
        if self.released or self.process.poll() is not None:
            raise RuntimeError('Composition Return barrier is no longer available')
        if not preedit_marker.is_file() or (capture / 'live.bin').read_bytes() != b'':
            raise RuntimeError('Refusing Return without preedit and zero-byte proof')
        # No input bytes are written. EOF advances exactly once to -k Return.
        self.released = True
        self.process.stdin.close()

    def stop(self):
        # Never close an unreleased pipe before its process has been reaped:
        # doing so would send Return on an unsuccessful setup path.
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        if not self.process.stdin.closed:
            self.process.stdin.close()
