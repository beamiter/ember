"""Connection readiness may repeat; app launches and input must never repeat."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('display', Path(__file__).with_name('native-x11-display.py'))
display = importlib.util.module_from_spec(spec)
spec.loader.exec_module(display)


class DisplayTests(unittest.TestCase):
    def fixture(self, data=b'137\n', status=None):
        self.calls = []
        server = SimpleNamespace(poll=lambda: status)
        def spawn(args, label, **kwargs):
            self.calls.append((args, label, kwargs))
            os.write(kwargs['pass_fds'][0], data)
            return server
        return spawn, server

    def test_only_probe_repeats_and_allocated_display_is_used(self):
        spawn, server = self.fixture()
        env = {'DISPLAY': ':99'}
        replies = [subprocess.TimeoutExpired('xdpyinfo', 3),
                   SimpleNamespace(returncode=1, stderr='not ready'),
                   SimpleNamespace(returncode=0, stderr='')]
        with tempfile.TemporaryDirectory() as folder, patch.object(display.subprocess, 'run', side_effect=replies) as run, patch.object(display.time, 'sleep'):
            self.assertIs(display.start_xvfb(spawn, env, Path(folder), '640x480x24'), server)
            attempts = [json.loads(line) for line in (Path(folder) / 'x11-readiness.jsonl').read_text().splitlines()]
            self.assertEqual([a.get('returncode') for a in attempts], [None, 1, 0])
            self.assertTrue(all(a['display'] == ':137' for a in attempts))
            self.assertEqual(env['DISPLAY'], ':137')
            self.assertEqual(run.call_count, 3)
            self.assertEqual(len(self.calls), 1)
            args, _, kwargs = self.calls[0]
            self.assertIn('-noreset', args)
            self.assertNotIn(':99', args)
            self.assertEqual(args[-1], str(kwargs['pass_fds'][0]))
            with self.assertRaises(OSError):
                os.fstat(kwargs['pass_fds'][0])

    def test_dead_server_and_invalid_allocation_never_probe_or_publish_display(self):
        for data, status in [(b'', None), (b'99', None), (b'99\nextra\n', None),
                             (b':99\n', None), (b'99\n', 1)]:
            with self.subTest(data=data, status=status), tempfile.TemporaryDirectory() as folder, patch.object(display.subprocess, 'run') as run:
                spawn, _ = self.fixture(data, status)
                env = {}
                with self.assertRaises(RuntimeError):
                    display.start_xvfb(spawn, env, Path(folder), '640x480x24')
                self.assertNotIn('DISPLAY', env)
                run.assert_not_called()

    def test_allocation_timeout_and_spawn_failure_close_both_pipe_ends(self):
        for fail in (False, True):
            descriptors = os.pipe()
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as folder, patch.object(display.os, 'pipe', return_value=descriptors) as pipe, patch.object(display.select, 'select', return_value=([], [], [])):
                spawn, _ = self.fixture()
                if fail:
                    def spawn(*args, **kwargs):
                        raise RuntimeError('spawn failed')
                with self.assertRaises(RuntimeError):
                    display.start_xvfb(spawn, {}, Path(folder), '640x480x24')
                self.assertEqual(pipe.call_count, 1)
                for descriptor in descriptors:
                    with self.assertRaises(OSError):
                        os.fstat(descriptor)


if __name__ == '__main__':
    unittest.main()
