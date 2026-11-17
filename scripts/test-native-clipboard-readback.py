#!/usr/bin/env python3
"""Isolated production-method contracts; not native clipboard acceptance."""
import ast
import json
from pathlib import Path
import subprocess
import tempfile
import types
import unittest
from unittest import mock


source = Path(__file__).with_name('native-desktop-smoke.py').read_text()
tree = ast.parse(source)
methods = [node for cls in tree.body if isinstance(cls, ast.ClassDef)
           for node in cls.body if isinstance(node, ast.FunctionDef)
           and node.name in {'own_clipboard', 'record_clipboard_readback_failure'}]
namespace = {'json': json, 'subprocess': subprocess, 'time': types.SimpleNamespace(sleep=lambda _: None)}
exec(compile(ast.Module(body=methods, type_ignores=[]), '<production-methods>', 'exec'), namespace)


class ReadbackTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.owner = mock.Mock()
        self.owner.poll.return_value = None
        self.driver = types.SimpleNamespace(out=Path(self.temp.name), backend='wayland', env={}, logs=[], processes=[])
        for name in ('own_clipboard', 'record_clipboard_readback_failure'):
            setattr(self.driver, name, types.MethodType(namespace[name], self.driver))
        self.addCleanup(lambda: [log.close() for log in self.driver.logs])

    def exercise(self, result=None, error=None):
        self.driver.read_clipboard = mock.Mock(return_value=result, side_effect=error)
        with mock.patch.object(subprocess, 'Popen', return_value=self.owner), mock.patch.object(namespace['time'], 'sleep') as sleep:
            try:
                self.driver.own_clipboard('private-payload', 'synthetic-case')
            finally:
                sleep.assert_called_once_with(0.5)
                self.driver.read_clipboard.assert_called_once_with()

    def evidence(self):
        text = (self.driver.out / 'clipboard-readback-failure.json').read_text()
        self.assertNotIn('private-payload', text)
        self.assertNotIn('private-stderr', text)
        self.assertNotIn('private-args', text)
        return json.loads(text)

    def test_success_has_no_failure_evidence(self):
        self.exercise(b'private-payload')
        self.assertFalse((self.driver.out / 'clipboard-readback-failure.json').exists())

    def test_nonzero_exit_is_rethrown_without_retry_or_content(self):
        error = subprocess.CalledProcessError(1, ['private-args'], output=b'private-payload', stderr=b'private-stderr')
        with self.assertRaises(subprocess.CalledProcessError) as caught:
            self.exercise(error=error)
        self.assertIs(caught.exception, error)
        data = self.evidence()
        self.assertEqual(data['reader_returncode'], 1)
        self.assertEqual(data['reader_stdout_bytes'], 15)
        self.assertEqual(data['reader_stderr_bytes'], 14)
        self.assertIsNone(data['owner_returncode'])
        self.assertFalse(data['reader_timeout'])

    def test_timeout_preserves_original_failure(self):
        error = subprocess.TimeoutExpired(['private-args'], 30, output=b'private-payload', stderr=b'private-stderr')
        self.owner.poll.return_value = 7
        with self.assertRaises(subprocess.TimeoutExpired):
            self.exercise(error=error)
        data = self.evidence()
        self.assertTrue(data['reader_timeout'])
        self.assertEqual(data['owner_returncode'], 7)

    def test_mismatch_keeps_exact_equality_gate(self):
        with self.assertRaisesRegex(AssertionError, 'Clipboard owner readback mismatch'):
            self.exercise(b'wrong')
        self.assertEqual(self.evidence()['reader_stdout_bytes'], 5)

    def test_evidence_write_error_does_not_mask_original(self):
        error = subprocess.CalledProcessError(1, ['private-args'])
        with mock.patch.object(Path, 'write_text', side_effect=OSError('unavailable')):
            with self.assertRaises(subprocess.CalledProcessError) as caught:
                self.exercise(error=error)
        self.assertIs(caught.exception, error)


if __name__ == '__main__':
    unittest.main()
