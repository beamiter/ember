"""Offline contracts only; X11 event and native completion gates remain required."""
import ast
import ctypes
import subprocess
import sys
from pathlib import Path
import unittest
from unittest import mock
import native_x11_plain_key as plain

F5 = plain.F5_KEYSYM


class PlainKeyTests(unittest.TestCase):
    def test_keycode_is_resolved_not_hardcoded(self):
        for code in (8, 9, 71, 93, 200):
            self.assertEqual(plain.select_plain_f5([(code, F5, F5)], {64}, set(), 0, 0), code)

    def test_three_digit_tokens_bypass_real_xlib_symbol_parser(self):
        x = ctypes.CDLL('libX11.so.6')
        x.XStringToKeysym.argtypes = [ctypes.c_char_p]
        x.XStringToKeysym.restype = ctypes.c_ulong
        # Old libxdo calls this before its decimal numeric-keycode branch.
        self.assertNotEqual(x.XStringToKeysym(b'8'), 0)
        self.assertNotEqual(x.XStringToKeysym(b'9'), 0)
        for code in range(8, 256):
            token = f'{code:03d}'
            self.assertEqual(x.XStringToKeysym(token.encode()), 0, token)
            self.assertEqual(int(token, 10), code)

    def test_blocked_query_child_is_killed_with_bounded_timeout(self):
        real_run = subprocess.run
        def blocked_query(*args, **kwargs):
            return real_run([sys.executable, '-c', 'import time; time.sleep(60)'], **kwargs)
        with mock.patch.object(plain, 'QUERY_TIMEOUT_SECONDS', 0.1), \
             mock.patch.object(plain.subprocess, 'run', side_effect=blocked_query):
            with self.assertRaisesRegex(RuntimeError, 'timed out'):
                plain.resolve_plain_f5(':fixture')

    def test_sparse_alt_mapping_never_supplies_required_modifiers(self):
        # A retained Alt->Level1 entry is irrelevant: only the real no-modifier
        # lookup and Level1 symbol qualify, with no active Alt.
        self.assertEqual(plain.select_plain_f5([(93, F5, F5)], {64}, set(), 0, 0), 93)
        with self.assertRaisesRegex(RuntimeError, 'active X11 modifiers'):
            plain.select_plain_f5([(93, F5, F5)], {64}, set(), 0, 8)

    def test_every_active_modifier_is_rejected_not_cleared(self):
        for mask in (1, 2, 4, 8, 16, 32, 64, 128):
            with self.subTest(mask=mask), self.assertRaises(RuntimeError):
                plain.select_plain_f5([(93, F5, F5)], {64}, set(), 0, mask)
        with self.assertRaises(RuntimeError):
            plain.select_plain_f5([(93, F5, F5)], {64}, {64}, 0, 0)

    def test_does_not_silently_switch_active_layout_group(self):
        with self.assertRaisesRegex(RuntimeError, 'group zero'):
            plain.select_plain_f5([(93, F5, F5)], {64}, set(), 1, 0)

    def test_no_mapping_ambiguity_or_modifier_key_accepted(self):
        for rows, modifier_codes in [([], set()), ([(93, F5, F5), (94, F5, F5)], set()),
                                     ([(93, F5, F5)], {93}), ([(7, F5, F5)], set())]:
            with self.subTest(rows=rows), self.assertRaises(RuntimeError):
                plain.select_plain_f5(rows, modifier_codes, set(), 0, 0)

    def test_level1_and_unmodified_lookup_must_agree(self):
        for row in [(93, F5, 0), (93, 0, F5), (93, F5+1, F5)]:
            with self.subTest(row=row), self.assertRaises(RuntimeError):
                plain.select_plain_f5([row], set(), set(), 0, 0)

    def test_missing_display_fails_closed_with_installed_xlib(self):
        with self.assertRaisesRegex(RuntimeError, 'Cannot open fixture X11'):
            plain.resolve_plain_f5('invalid-display-without-colon')

    def test_actual_smoke_wrapper_preserves_sequence_and_other_chords(self):
        source = Path(__file__).with_name('native-vim9-buffer-smoke.py').read_text()
        tree = ast.parse(source)
        fn = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == 'xdo')
        response = mock.Mock(returncode=0)
        subprocess = mock.Mock()
        subprocess.run.return_value = response
        resolver = mock.Mock(return_value={'keycode': 93})
        namespace = dict(resolve_plain_f5=resolver, report={}, env={'DISPLAY': ':fixture'}, subprocess=subprocess)
        exec(compile(ast.fix_missing_locations(ast.Module(body=[fn], type_ignores=[])), '<actual-wrapper>', 'exec'), namespace)
        for keys in [('A', 'F5'), ('A', 'F5', 'ctrl+n', 'ctrl+e')]:
            namespace['xdo']('key', *keys)
            self.assertEqual(subprocess.run.call_args.args[0], ['xdotool', 'key', *['093' if k=='F5' else k for k in keys]])
        for code in (8, 9):
            resolver.return_value = {'keycode': code}
            namespace['xdo']('key', 'A', 'F5')
            self.assertEqual(subprocess.run.call_args.args[0], ['xdotool', 'key', 'A', f'{code:03d}'])
        resolver.reset_mock()
        namespace['xdo']('key', 'alt+F5')
        self.assertEqual(subprocess.run.call_args.args[0], ['xdotool', 'key', 'alt+F5'])
        resolver.assert_not_called()
        namespace['xdo']('key', 'ctrl+n')
        resolver.assert_not_called()
        # Exactly the existing four completion stimuli, and original wait deadline.
        calls=[n for n in ast.walk(tree) if isinstance(n, ast.Call) and isinstance(n.func,ast.Name)
               and n.func.id=='xdo' and any(isinstance(a,ast.Constant) and a.value=='F5' for a in n.args)]
        self.assertEqual(len(calls),4)
        wait=next(n for n in tree.body if isinstance(n,ast.FunctionDef) and n.name=='wait_for')
        self.assertEqual(wait.args.defaults[-1].value,25)

    def test_wrapper_does_not_send_input_when_verification_fails(self):
        tree=ast.parse(Path(__file__).with_name('native-vim9-buffer-smoke.py').read_text())
        fn=next(n for n in ast.walk(tree) if isinstance(n,ast.FunctionDef) and n.name=='xdo')
        subprocess=mock.Mock()
        namespace=dict(resolve_plain_f5=mock.Mock(side_effect=RuntimeError('active Alt')), report={}, env={'DISPLAY': ':fixture'}, subprocess=subprocess)
        exec(compile(ast.fix_missing_locations(ast.Module(body=[fn],type_ignores=[])), '<actual-wrapper>','exec'),namespace)
        with self.assertRaisesRegex(RuntimeError,'active Alt'):
            namespace['xdo']('key','A','F5')
        subprocess.run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
