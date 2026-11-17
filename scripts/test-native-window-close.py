#!/usr/bin/env python3
"""Pure evidence-guard regressions; no native/GUI acceptance is claimed."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('close_fixture', Path(__file__).with_name('native-window-close.py'))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def identity(pid, start=10, state='S'):
    return dict(pid=pid, start_time=start, state=state, parent=1, group=pid, foreground=pid)


def words(text):
    return [dict(text=word, block_num='1', par_num='1', line_num='1',
                 left=str(i * 20), top='40', width='20', height='10')
            for i, word in enumerate(text.split())]


class EvidenceGuardTests(unittest.TestCase):
    def test_original_live_identity(self):
        a = identity(3)
        self.assertTrue(fixture.same_live_process(a, a.copy()))
        with patch.object(fixture, 'process_state', side_effect=lambda pid: identity(pid)):
            fixture.assert_jobs_live([a, identity(4)])

    def test_expired_zombie_missing_or_reused_job_rejected(self):
        a = identity(3)
        for current in [None, identity(3, state='Z'), identity(3, state='X'), identity(3, start=11), identity(4)]:
            with self.subTest(current=current):
                self.assertFalse(fixture.same_live_process(a, current))
                with patch.object(fixture, 'process_state', return_value=current):
                    with self.assertRaises(AssertionError):
                        fixture.assert_jobs_live([a, identity(4)])

    def test_hidden_job_alone_or_duplicate_does_not_prove_active_tab(self):
        a = identity(3)
        for jobs in [[], [a], [a, a.copy()]]:
            with self.subTest(jobs=jobs), self.assertRaises(AssertionError):
                fixture.assert_jobs_live(jobs)

    def test_dead_active_job_is_detected_even_when_hidden_job_lives(self):
        with patch.object(fixture, 'process_state', side_effect=lambda pid: identity(pid) if pid == 3 else None):
            with self.assertRaises(AssertionError):
                fixture.assert_jobs_live([identity(3), identity(4)])

    def test_proc_start_time_and_parent_are_read_after_final_parenthesis(self):
        fields = ['S', '31', '41', '31', '1', '41'] + ['0'] * 13 + ['987654']
        with patch.object(Path, 'read_text', return_value='41 (name with ) parens) ' + ' '.join(fields)):
            result = fixture.process_state(41)
        self.assertEqual(result, dict(pid=41, state='S', parent=31, group=41, foreground=41, start_time=987654))
        with patch.object(Path, 'read_text', side_effect=FileNotFoundError):
            self.assertIsNone(fixture.process_state(41))

    def test_remaining_terminal_or_eva_dialog_rejected(self):
        for phrase in ['Close window', 'Keep window open', 'Close anyway', 'Cancel']:
            with self.subTest(phrase=phrase), self.assertRaises(AssertionError):
                fixture.assert_dialog_absent(words(phrase))
        fixture.assert_dialog_absent(words('Terminal ready'))

    def test_empty_ocr_is_not_dismissal_evidence(self):
        for sample in [[], [dict(text=' ')]]:
            with self.subTest(sample=sample), self.assertRaises(AssertionError):
                fixture.assert_dialog_absent(sample)

    def test_button_words_must_share_line(self):
        sample = words('Close window')
        sample[1]['line_num'] = '2'
        self.assertIsNone(fixture.button(sample, 'Close window'))
        self.assertIsNotNone(fixture.button(words('Close window'), 'Close window'))

    def test_jobs_cannot_naturally_expire_during_bounded_scenario_and_exit_waits(self):
        self.assertGreater(fixture.JOB_SECONDS, fixture.SCENARIO_SECONDS + 15 + 10)


if __name__ == '__main__':
    unittest.main()
