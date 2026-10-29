"""Socket-free capture budget sequencing and isolated real-PTY regression."""
import json
import os
from pathlib import Path
import pty
import subprocess
import sys
import tempfile
import time
import unittest
from native_ime_capture_budget import (
    ACTIVE_SECONDS, CaptureBudget, PREPARATION_SECONDS, acknowledge, arm_capture,
    descriptor, prepare_capture, publish_once, request, require_request,
)


def wait_for(test, label, timeout=3):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = test()
        if value:
            return value
        time.sleep(0.01)
    raise RuntimeError('Timed out: ' + label)


class BudgetTests(unittest.TestCase):
    def test_legacy_active_capture_keeps_original_fifteen_second_bound(self):
        budget = CaptureBudget(100)
        budget.check(114.999)
        with self.assertRaisesRegex(RuntimeError, 'capture timed out'):
            budget.check(115)
        self.assertEqual(ACTIVE_SECONDS, 15)

    def test_passive_preparation_is_separate_and_finitely_bounded(self):
        budget = CaptureBudget(100)
        budget.prepare(100.125)
        budget.check(116)
        budget.check(124.999)
        with self.assertRaisesRegex(RuntimeError, 'preparing timed out'):
            budget.check(125)
        self.assertEqual(PREPARATION_SECONDS, 25)

    def test_trigger_starts_original_budget_once_not_an_ocr_heartbeat(self):
        budget = CaptureBudget(100)
        budget.prepare(100.125)
        budget.commit(120)
        budget.check(134.999)
        with self.assertRaisesRegex(RuntimeError, 'commit timed out'):
            budget.check(135)
        self.assertEqual(budget.report()['events'], [
            {'phase': 'capture', 'elapsed': 0},
            {'phase': 'preparing', 'elapsed': 0.125},
            {'phase': 'commit', 'elapsed': 20},
        ])

    def test_rearming_or_expired_preparation_cannot_extend_the_deadline(self):
        budget = CaptureBudget(0)
        with self.assertRaisesRegex(RuntimeError, 'requires preparation'):
            budget.commit(1)
        budget.prepare(2)
        with self.assertRaisesRegex(RuntimeError, 'once-only'):
            budget.prepare(3)
        budget.commit(4)
        with self.assertRaisesRegex(RuntimeError, 'requires preparation'):
            budget.commit(5)
        with self.assertRaisesRegex(RuntimeError, 'commit timed out'):
            budget.check(19)
        expired = CaptureBudget(0)
        with self.assertRaisesRegex(RuntimeError, 'capture timed out'):
            expired.prepare(15)

    def test_backwards_and_nonfinite_clocks_fail_closed(self):
        for now in [float('nan'), float('inf'), -1]:
            with self.subTest(now=now), self.assertRaisesRegex(RuntimeError, 'monotonic'):
                CaptureBudget(0).check(now)

    def test_descriptors_and_acknowledgements_keep_exact_phase_and_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            publish_once(out / 'ready', descriptor(42))
            (out / 'live.bin').write_bytes(b'')
            def prepare_ack(test, label):
                require_request(out, 'preparing', 42)
                acknowledge(out, 'preparing', 42)
                assert test(), label
            self.assertEqual(prepare_capture(out, prepare_ack), 42)
            wrong = dict(descriptor(43), phase='preparing')
            (out / 'preparing-ready.json').write_text(json.dumps(wrong))
            with self.assertRaisesRegex(RuntimeError, 'acknowledgement'):
                arm_capture(out)
            self.assertFalse((out / 'commit-request.json').exists())

    def test_legacy_collector_and_exited_collector_are_not_rearmed(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            (out / 'ready').touch()
            self.assertIsNone(prepare_capture(out, lambda *_: self.fail('legacy was rearmed')))
            (out / 'result.json').write_text('{"status":"fail"}')
            with self.assertRaisesRegex(RuntimeError, 'capture exit'):
                arm_capture(out)

    def test_advertised_phase_capture_cannot_skip_prepare_request(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            publish_once(out / 'ready', descriptor(42))
            (out / 'live.bin').write_bytes(b'')
            with self.assertRaisesRegex(RuntimeError, 'preparation request is missing'):
                arm_capture(out)
            self.assertFalse((out / 'commit-request.json').exists())

    def test_phase_publication_is_atomic_and_once_only(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            request(out, 'preparing', 42)
            self.assertEqual(json.loads((out / 'preparing-request.json').read_text()), dict(descriptor(42), phase='preparing'))
            self.assertFalse(list(out.glob('*.pending')))
            with self.assertRaises(FileExistsError):
                request(out, 'preparing', 42)

    def test_malformed_or_unregistered_budget_never_silently_falls_back(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            (out / 'live.bin').write_bytes(b'')
            for value in [dict(descriptor(42), active_seconds=30), dict(descriptor(42), pid=True), {'profile': 'unknown'}]:
                (out / 'ready').write_text(json.dumps(value))
                with self.subTest(value=value), self.assertRaisesRegex(RuntimeError, 'descriptor'):
                    prepare_capture(out, lambda *_: self.fail('Invalid descriptor reached wait'))
            self.assertFalse((out / 'preparing-request.json').exists())

    def test_real_pretrigger_bytes_and_early_finish_fail_closed(self):
        for action in ('bytes', 'finish'):
            with self.subTest(action=action), tempfile.TemporaryDirectory() as directory:
                out = Path(directory) / 'capture'
                master, slave = pty.openpty()
                env = os.environ.copy()
                env['EMBER_SMOKE_RAW_OUT'] = str(out)
                proc = subprocess.Popen([sys.executable, str(Path(__file__).with_name('native-ime-pty-capture.py'))],
                                        stdin=slave, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
                os.close(slave)
                try:
                    wait_for((out / 'ready').exists, 'negative raw ready')
                    prepare_capture(out, wait_for)
                    if action == 'bytes':
                        os.write(master, b'x')
                    else:
                        (out / 'finish').touch()
                    proc.communicate(timeout=3)
                    self.assertNotEqual(proc.returncode, 0)
                    result = json.loads((out / 'result.json').read_text())
                    self.assertEqual(result['status'], 'fail')
                    self.assertEqual((out / 'bytes.bin').read_bytes(), b'x' if action == 'bytes' else b'')
                    with self.assertRaisesRegex(RuntimeError, 'capture exit'):
                        arm_capture(out)
                finally:
                    if proc.poll() is None:
                        proc.terminate()
                        proc.communicate(timeout=3)
                    os.close(master)

    def test_real_pty_survives_passive_preparation_then_exact_triggered_commit(self):
        # Original collector exited at15s while OCR still ran. This actual
        # foreground helper must survive16s preparation and keep the same15s
        # active budget after the explicit acknowledged trigger.
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / 'capture'
            master, slave = pty.openpty()
            env = os.environ.copy()
            env['EMBER_SMOKE_RAW_OUT'] = str(out)
            proc = subprocess.Popen([sys.executable, str(Path(__file__).with_name('native-ime-pty-capture.py'))],
                                    stdin=slave, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
            os.close(slave)
            try:
                wait_for((out / 'ready').exists, 'real raw ready')
                pid = prepare_capture(out, wait_for)
                self.assertEqual(pid, proc.pid)
                time.sleep(16)
                self.assertIsNone(proc.poll(), 'Collector exited during passive preparation')
                self.assertEqual((out / 'live.bin').read_bytes(), b'')
                arm_capture(out)
                expected = '你'.encode()
                os.write(master, expected)
                wait_for(lambda: (out / 'live.bin').read_bytes() == expected, 'exact synthetic UTF-8')
                (out / 'finish').touch()
                stdout, stderr = proc.communicate(timeout=3)
                self.assertEqual(proc.returncode, 0, (stdout, stderr))
                self.assertEqual((out / 'bytes.bin').read_bytes(), expected)
                result = json.loads((out / 'result.json').read_text())
                self.assertEqual(result['status'], 'pass')
                self.assertEqual(result['capture_budget']['phase'], 'commit')
                self.assertEqual(result['capture_budget']['active_limit_seconds'], 15)
                self.assertGreaterEqual(result['capture_budget']['events'][-1]['elapsed'], 16)
                if os.environ.get('EMBER_CAPTURE_BUDGET_EVIDENCE'):
                    evidence = Path(os.environ['EMBER_CAPTURE_BUDGET_EVIDENCE'])
                    evidence.mkdir(parents=True, exist_ok=True)
                    publish_once(evidence / 'native-capture-budget-receipt.json', {
                        'result': result, 'expected_hex': expected.hex(),
                        'actual_hex': (out / 'bytes.bin').read_bytes().hex(),
                        'scope': 'isolated synthetic PTY helper, not an Ember GUI or IME run',
                    })
            finally:
                if proc.poll() is None:
                    proc.terminate()
                    proc.communicate(timeout=3)
                os.close(master)


if __name__ == '__main__':
    unittest.main()
