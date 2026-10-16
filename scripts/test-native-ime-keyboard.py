"""Socket-free causal parser and cancellation-order tests."""
import importlib.util
from pathlib import Path
import tempfile
import subprocess
import unittest

spec = importlib.util.spec_from_file_location('ime_keyboard', Path(__file__).with_name('native-ime-keyboard.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

PRESS = '[1] -> zwp_virtual_keyboard_v1#5.modifiers(1, 0, 0, 0)\n'
RELEASE = '[2] -> zwp_virtual_keyboard_v1#5.modifiers(0, 0, 0, 0)\n'
SYNC = '[3] -> wl_display#1.sync(new id wl_callback#9)\n'
DONE = '[4] wl_callback#9.done(42)\n'


class Pipe:
    def __init__(self, events):
        self.events, self.closed = events, False

    def close(self):
        self.events.append('close')
        self.closed = True


class Process:
    def __init__(self):
        self.events = []
        self.stdin = Pipe(self.events)
        self.status = None

    def poll(self):
        return self.status

    def terminate(self):
        self.events.append('terminate')

    def wait(self, timeout):
        self.events.append('reap')
        self.status = -15


class BarrierTests(unittest.TestCase):
    def test_exact_argument_vector(self):
        self.assertEqual(module.KEYBOARD_ARGS,
                         ['wtype', '-s', '250', '-M', 'shift', '-m', 'shift',
                          '-', '-k', 'Return', '-s', '600000'])

    def test_exact_callback_and_old_wayland_format(self):
        for text in [PRESS + RELEASE + SYNC + DONE,
                     (PRESS + RELEASE + SYNC + DONE).replace('#', '@')]:
            self.assertTrue(module.modifier_roundtrip_complete(text))

    def test_missing_or_wrong_release(self):
        for text in [PRESS + SYNC + DONE, RELEASE + SYNC + DONE,
                     PRESS + RELEASE.replace('#5', '#6') + SYNC + DONE,
                     PRESS.replace('(1,', '(4,') + RELEASE + SYNC + DONE]:
            self.assertFalse(module.modifier_roundtrip_complete(text))

    def test_stale_unrelated_and_partial_callbacks(self):
        for text in [DONE + PRESS + RELEASE + SYNC,
                     PRESS + RELEASE + SYNC + DONE.replace('#9', '#8'),
                     PRESS + RELEASE + SYNC + DONE.rstrip('\n'),
                     PRESS + RELEASE.rstrip('\n'),
                     PRESS + RELEASE + SYNC.rstrip('\n'),
                     PRESS + RELEASE + DONE]:
            self.assertFalse(module.modifier_roundtrip_complete(text))

    def test_press_sync_does_not_ack_release(self):
        self.assertFalse(module.modifier_roundtrip_complete(PRESS + SYNC + DONE + RELEASE))

    def test_early_key_event_rejected(self):
        self.assertFalse(module.modifier_roundtrip_complete(
            PRESS + RELEASE + '[3] -> zwp_virtual_keyboard_v1#5.key(0, 1, 1)\n' + SYNC + DONE))

    def keyboard(self):
        obj = object.__new__(module.CompositionKeyboard)
        obj.process = Process()
        obj.released = False
        return obj

    def test_failure_kills_and_reaps_before_pipe_close(self):
        keyboard = self.keyboard()
        keyboard.stop()
        self.assertEqual(keyboard.process.events, ['terminate', 'reap', 'close'])
        self.assertFalse(keyboard.released)

    def test_timeout_escalation_reaps_before_pipe_close(self):
        keyboard = self.keyboard()
        def wait(timeout):
            keyboard.process.events.append('wait')
            if 'kill' not in keyboard.process.events:
                raise subprocess.TimeoutExpired('owned-wtype', timeout)
            keyboard.process.status = -9
        keyboard.process.wait = wait
        keyboard.process.kill = lambda: keyboard.process.events.append('kill')
        keyboard.stop()
        self.assertEqual(keyboard.process.events, ['terminate', 'wait', 'kill', 'wait', 'close'])

    def test_return_requires_marker_and_zero_bytes_and_is_once_only(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            marker = path / 'preedit'
            (path / 'live.bin').write_bytes(b'')
            keyboard = self.keyboard()
            with self.assertRaises(RuntimeError):
                keyboard.release_return(marker, path)
            self.assertEqual(keyboard.process.events, [])
            marker.touch()
            (path / 'live.bin').write_bytes(b'x')
            with self.assertRaises(RuntimeError):
                keyboard.release_return(marker, path)
            self.assertEqual(keyboard.process.events, [])
            (path / 'live.bin').write_bytes(b'')
            keyboard.release_return(marker, path)
            self.assertEqual(keyboard.process.events, ['close'])
            with self.assertRaises(RuntimeError):
                keyboard.release_return(marker, path)
            keyboard.stop()
            self.assertEqual(keyboard.process.events, ['close', 'terminate', 'reap'])


if __name__ == '__main__':
    unittest.main()
