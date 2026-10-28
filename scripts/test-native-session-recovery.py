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
    def test_recovery_region_uses_attached_card_surfaces_not_warning_words(self):
        from PIL import Image, ImageDraw
        image = Image.new('RGB', (1280, 900), (0, 0, 0))
        draw = ImageDraw.Draw(image)
        draw.rectangle((107, 67, 554, 95), fill=(45, 45, 45))
        draw.rectangle((107, 96, 554, 484), fill=(29, 29, 29))
        owned = dict(x=90, y=50, width=1100, height=800)
        self.assertEqual(recovery.recovery_rectangle(image, owned), (107, 67, 555, 485))
        draw.text((120, 200), 'unrelated warning text', fill=(220, 220, 220))
        self.assertEqual(recovery.recovery_rectangle(image, owned), (107, 67, 555, 485))

    def test_absent_duplicate_foreign_or_headerless_card_is_not_evidence(self):
        from PIL import Image, ImageDraw
        for case in ['absent', 'duplicate', 'foreign', 'headerless']:
            with self.subTest(case=case):
                image = Image.new('RGB', (1280, 900), (0, 0, 0))
                draw = ImageDraw.Draw(image)
                owned = dict(x=90, y=50, width=1100, height=800)
                if case != 'absent': draw.rectangle((107, 96, 554, 484), fill=(29, 29, 29))
                if case not in ['absent', 'headerless']:
                    draw.rectangle((107, 67, 554, 95), fill=(45, 45, 45))
                if case == 'duplicate':
                    draw.rectangle((607, 96, 1054, 484), fill=(29, 29, 29))
                    draw.rectangle((607, 67, 1054, 95), fill=(45, 45, 45))
                if case == 'foreign': owned = dict(x=600, y=50, width=600, height=800)
                with self.assertRaises(RuntimeError): recovery.recovery_rectangle(image, owned)

    def test_exact_warning_missing_wrong_or_duplicate_remains_a_failure(self):
        phrase = 'Saved pane layout adjusted: invalid or oversized layout references.'
        for text in ['', phrase.replace('invalid', 'valid'), phrase + ' ' + phrase]:
            with self.subTest(text=text), self.assertRaises(AssertionError):
                recovery.require_phrases(None, 'fixture', [phrase], text)

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
