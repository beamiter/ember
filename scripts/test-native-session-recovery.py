"""Pure exact recovery-button targeting; not real desktop acceptance."""
import copy
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock

spec = importlib.util.spec_from_file_location('recovery', Path(__file__).with_name('native-session-recovery.py'))
recovery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recovery)


def word(text, x, y, w, h, line='1'):
    return dict(text=text, left=str(x), top=str(y), width=str(w), height=str(h),
                page_num='1', block_num='20', par_num='1', line_num=line)


class RecoveryPhraseTests(unittest.TestCase):
    def rows(self, y=203):
        return [word('Recheck', 30, y, 51, 11), word('original', 85, y-10, 46, 31),
                word('metadata', 139, y-10, 58, 31)]

    def test_expanded_word_boxes_keep_exact_center_aligned_button(self):
        for y in [203, 187]:
            smoke = SimpleNamespace(words=Mock(return_value=self.rows(y)), click=Mock())
            recovery.click_phrase(smoke, 'Recheck original metadata', 'fixture')
            smoke.click.assert_called_once_with(55, y+5)

    def test_equal_boxes_and_unordered_tsv_are_sorted_within_one_line(self):
        rows = self.rows()
        for row in rows:
            row.update(top='203', height='11')
        self.assertEqual(recovery.phrase_target(rows[::-1], 'Recheck original metadata')['left'], '30')

    def test_invalid_or_ambiguous_labels_never_click(self):
        for case in ['different-line', 'vertical-center', 'no-overlap', 'huge-bridge',
                     'reverse', 'far', 'overlap', 'duplicate', 'missing', 'case', 'typo']:
            with self.subTest(case=case):
                rows = self.rows()
                if case == 'different-line': rows[1]['line_num'] = '2'
                elif case == 'vertical-center': rows[1]['top'] = '210'
                elif case == 'no-overlap': rows[1].update(top='240', height='10')
                elif case == 'huge-bridge': rows[1].update(top='190', height='100')
                elif case == 'reverse': rows[0]['left'], rows[1]['left'] = rows[1]['left'], rows[0]['left']
                elif case == 'far': rows[2]['left'] = '300'
                elif case == 'overlap': rows[2]['left'] = '90'
                elif case == 'duplicate':
                    other = copy.deepcopy(rows)
                    for row in other: row['line_num'] = '2'
                    rows += other
                elif case == 'missing': rows.pop()
                elif case == 'case': rows[0]['text'] = 'recheck'
                else: rows[1]['text'] = 'origina1'
                smoke = SimpleNamespace(words=Mock(return_value=rows), click=Mock())
                with self.assertRaises(RuntimeError):
                    recovery.click_phrase(smoke, 'Recheck original metadata', 'fixture')
                smoke.click.assert_not_called()


if __name__ == '__main__':
    unittest.main()
