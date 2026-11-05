"""Pure exit-evidence/confirmation checks; these do not claim native UI acceptance."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('vim_exit_smoke', Path(__file__).with_name('native-vim9-buffer-smoke.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


def line(words, top, group):
    rows = []
    x = 24
    for word in words:
        width = len(word) * 7
        rows.append(dict(text=word, left=str(x), top=str(top), width=str(width), height='14',
                         page_num='1', block_num=str(group), par_num='1', line_num='1'))
        x += width + 7
    return rows


def dialog():
    return line(['Eva', 'is', 'leaving', 'this', 'window.'], 90, 1) + line(['Close', 'anyway'], 162, 2)


class NativeExitEvidenceTests(unittest.TestCase):
    def test_exact_close_message_and_button(self):
        self.assertEqual(smoke.close_confirmation_target(dialog(), 1400, 950), (66, 169))

    def test_absent_message_or_button_never_supplies_a_click(self):
        for rows in [[], dialog()[:5], dialog()[5:]]:
            self.assertIsNone(smoke.close_confirmation_target(rows, 1400, 950))

    def test_duplicate_button_or_message_rejected(self):
        for rows in [dialog() + line(['Close', 'anyway'], 190, 3),
                     dialog() + line(['Eva', 'is', 'leaving', 'this', 'window.'], 50, 3)]:
            with self.assertRaises(RuntimeError):
                smoke.close_confirmation_target(rows, 1400, 950)

    def test_foreign_or_discontinuous_words_rejected(self):
        for edit in ['outside', 'gap', 'above']:
            rows = dialog()
            if edit == 'outside':
                rows[-1]['left'] = '1399'
            elif edit == 'gap':
                rows[-1]['left'] = '500'
            else:
                for row in rows[5:]:
                    row['top'] = '20'
            with self.assertRaises(RuntimeError):
                smoke.close_confirmation_target(rows, 1400, 950)

    def test_wrong_confirmation_text_is_not_repaired(self):
        rows = dialog()
        rows[0]['text'] = 'Eve'
        self.assertIsNone(smoke.close_confirmation_target(rows, 1400, 950))

    def test_shell_receipt_requires_exact_nonce_pid_and_real_pty_shape(self):
        self.assertEqual(smoke.shell_identity('before-nonce:123:/dev/pts/7\n', 'before-nonce'), ['123', '/dev/pts/7'])
        for value in ['other:123:/dev/pts/7', 'before-nonce:1:/dev/pts/7',
                      'before-nonce:no:/dev/pts/7', 'before-nonce:123:not a tty',
                      'before-nonce:123:/dev/pts/no', 'before-nonce:123:/dev/pts/7\nextra']:
            with self.assertRaises(RuntimeError):
                smoke.shell_identity(value, 'before-nonce')

    def test_process_identity_keeps_start_time_and_handles_pid_reuse(self):
        with tempfile.TemporaryDirectory() as root:
            proc = Path(root)
            (proc / '123').mkdir()
            stat = proc / '123/stat'
            # Field 3 (state) followed by fields 4..21, then field22 starttime.
            prefix = '123 (fixture vim ) name) S ' + ' '.join(['0'] * 18) + ' '
            stat.write_text(prefix + '12345 99\n')
            self.assertEqual(smoke.process_incarnation(123, proc), '12345')
            stat.write_text(prefix + '99999 99\n')
            self.assertEqual(smoke.process_incarnation(123, proc), '99999')
            stat.unlink()
            self.assertIsNone(smoke.process_incarnation(123, proc))

    def test_invalid_pid_never_counts_as_exit(self):
        for pid in [0, 1, -1, '123', True]:
            with self.assertRaises(RuntimeError):
                smoke.process_incarnation(pid)


if __name__ == '__main__':
    unittest.main()
