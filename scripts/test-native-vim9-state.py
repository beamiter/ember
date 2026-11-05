"""Receipt publication regressions; these do not claim native UI acceptance."""
import importlib.util
import json
from pathlib import Path
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('vim_state_smoke', Path(__file__).with_name('native-vim9-buffer-smoke.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class NativeVimStateTests(unittest.TestCase):
    def test_vim_remove_then_rename_gap_returns_the_new_complete_receipt(self):
        path = mock.Mock()
        receipt = dict(line=3, column=1, modified=0)
        path.read_text.side_effect = [FileNotFoundError(), json.dumps(receipt)]
        with mock.patch.object(smoke.time, 'sleep'):
            self.assertEqual(smoke.read_vim_state(path), receipt)
        self.assertEqual(path.read_text.call_count, 2)

    def test_valid_modified_state_is_not_retried_until_it_passes(self):
        path = mock.Mock()
        receipt = dict(line=1, column=1, modified=1, text='unexpected edit')
        path.read_text.side_effect = [json.dumps(receipt), '{"modified":0}']
        with mock.patch.object(smoke.time, 'sleep') as sleep:
            self.assertEqual(smoke.read_vim_state(path), receipt)
        self.assertEqual(path.read_text.call_count, 1)
        sleep.assert_not_called()

    def test_invalid_or_missing_receipt_has_a_strict_deadline(self):
        for fault in [FileNotFoundError(), '{"modified":']:
            with self.subTest(fault=fault):
                path = mock.Mock()
                if isinstance(fault, Exception):
                    path.read_text.side_effect = fault
                else:
                    path.read_text.return_value = fault
                with mock.patch.object(smoke.time, 'monotonic', side_effect=[0, 0, 0.25]), \
                        mock.patch.object(smoke.time, 'sleep') as sleep:
                    self.assertEqual(smoke.read_vim_state(path), {})
                self.assertEqual(path.read_text.call_count, 2)
                sleep.assert_called_once_with(0.005)

    def test_missing_receipt_never_reuses_an_old_success(self):
        path = mock.Mock()
        path.read_text.side_effect = ['{"modified":0}', FileNotFoundError()]
        self.assertEqual(smoke.read_vim_state(path, timeout=0), {'modified': 0})
        self.assertEqual(smoke.read_vim_state(path, timeout=0), {})

    def test_read_permission_errors_are_not_hidden_as_publication_gaps(self):
        path = mock.Mock()
        path.read_text.side_effect = PermissionError('denied')
        with self.assertRaises(PermissionError):
            smoke.read_vim_state(path)


if __name__ == '__main__':
    unittest.main()
