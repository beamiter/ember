"""Current two-presentation desktop input observes exact owned native controls."""
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest

spec = importlib.util.spec_from_file_location('workspace_desktop', Path(__file__).with_name('native-desktop-smoke.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)


def word(text, x, y, width=40, height=14):
    return dict(text=text, left=str(x), top=str(y), width=str(width), height=str(height))


class PublicPresentationTests(unittest.TestCase):
    def test_legacy_configuration_maps_to_current_presentation(self):
        self.assertEqual(desktop.presentation_mode('normal'), 'block')
        self.assertEqual(desktop.presentation_mode('block'), 'block')
        self.assertEqual(desktop.presentation_mode('chat'), 'chat')

    def test_unknown_presentation_stops_before_any_real_input(self):
        for old, new in [('vte', 'chat'), ('block', 'shell'), ('unknown', 'normal')]:
            with self.subTest(old=old, new=new):
                calls = []
                smoke = SimpleNamespace(key=lambda *args: calls.append(args))
                with self.assertRaisesRegex(RuntimeError, 'Unknown terminal presentation'):
                    desktop.Smoke.switch_mode(smoke, old, new)
                self.assertEqual(calls, [])

    def test_popup_ignores_same_mode_in_toolbar_and_selector(self):
        selector = word('Block', 480, 520)
        correct = word('Chat', 480, 565)
        rows = [word('Chat', 1200, 18), word('Chat', 480, 520), correct]
        self.assertIs(desktop.settings_mode_popup(rows, selector, 'chat'), correct)

    def test_popup_requires_exact_text_and_unique_scoped_control(self):
        selector = word('Block', 480, 520)
        for rows in [[word('chat', 480, 565)], [word('Chat!', 480, 565)],
                     [word('Chat', 480, 565), word('Chat', 480, 585)],
                     [word('Chat', 480, 900)], [word('Chat', 200, 565)]]:
            with self.subTest(rows=rows), self.assertRaisesRegex(RuntimeError, 'Missing or ambiguous'):
                desktop.settings_mode_popup(rows, selector, 'chat')

    def test_popup_does_not_reintroduce_a_public_normal_entry(self):
        selector = word('Chat', 480, 520)
        rows = [word('Normal', 480, 550), word('Block', 480, 565)]
        self.assertEqual(desktop.settings_mode_popup(rows, selector, 'normal')['text'], 'Block')

    def test_invalid_selector_geometry_never_selects_a_target(self):
        with self.assertRaisesRegex(RuntimeError, 'Invalid native Settings selector'):
            desktop.settings_mode_popup([word('Chat', 480, 565)], word('Block', 480, 520, height=0), 'chat')


if __name__ == '__main__':
    unittest.main()
