"""Completion probe regressions; native GUI acceptance remains a separate gate."""
import importlib.util
from pathlib import Path
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('completion_smoke', Path(__file__).with_name('native-vim9-buffer-smoke.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


def receipt(**updates):
    value = dict(mode='i', text='', completion=dict(pum_visible=1, selected=-1),
                 render=dict(supported=True, completion={'view': {}}))
    value.update(updates)
    return value


class CompletionProbeTests(unittest.TestCase):
    def test_requires_all_four_original_conditions_in_one_receipt(self):
        good = receipt()
        for value in [receipt(completion=dict(pum_visible=0, selected=-1)),
                      receipt(completion=dict(pum_visible=1, selected=0)),
                      receipt(render=dict(supported=False, completion={'view': {}})),
                      receipt(render=dict(supported=True, completion=None)), {}, good]:
            with self.subTest(value=value):
                read = mock.Mock(return_value=value)
                result = smoke.CompletionProbe(read, {})(-1)
                self.assertIs(result, good if value is good else None)
                read.assert_called_once_with()

    def test_modified_function_key_is_diagnostic_not_accepted(self):
        report = {}
        value = receipt(text='<M-F5>', completion=dict(pum_visible=0, selected=-1),
                        render=dict(supported=True, completion=None))
        self.assertIsNone(smoke.CompletionProbe(lambda: value, report)(-1))
        self.assertEqual(report['completion_observations'][-1]['text'], '<M-F5>')
        self.assertFalse(report['completion_observations'][-1]['pum_visible'])

    def test_no_previous_success_is_reused(self):
        read = mock.Mock(side_effect=[receipt(), {}])
        probe = smoke.CompletionProbe(read, {})
        self.assertTrue(probe(-1))
        self.assertIsNone(probe(-1))

    def test_transitions_are_deduplicated_and_bounded(self):
        report = {}
        value = receipt(fixture_input={key: 'x' * 10000 for key in
                        ('f5_mapping', 'f5_termcode', 'completeopt', 'error', 'unrequested')})
        probe = smoke.CompletionProbe(lambda: value, report)
        for _ in range(50):
            probe(-1)
        self.assertEqual(len(report['completion_observations']), 1)
        fields = report['completion_observations'][0]['fixture_input']
        self.assertEqual({key: len(value) for key, value in fields.items()},
                         dict(f5_mapping=512, f5_termcode=64, completeopt=160, error=160))
        self.assertNotIn('unrequested', fields)
        for i in range(50):
            value['column'] = i
            probe(-1)
        self.assertEqual(len(report['completion_observations']), 16)
        self.assertEqual(report['completion_observations'][0]['column'], 34)
        self.assertEqual(report['completion_observations'][-1]['column'], 49)

    def test_diagnostics_do_not_sleep_or_change_timeout(self):
        with mock.patch.object(smoke.time, 'sleep') as sleep:
            smoke.CompletionProbe(lambda: {}, {})(-1)
        sleep.assert_not_called()
        self.assertEqual(smoke.wait_for.__defaults__, (25,))


if __name__ == '__main__':
    unittest.main()
