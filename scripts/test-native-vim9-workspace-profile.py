"""Synthetic current-profile witnesses; these do not run native Vim or a GPU."""
import importlib.util
from pathlib import Path
import unittest
from PIL import Image, ImageDraw
import native_glass_profile as glass

spec = importlib.util.spec_from_file_location('vim_smoke', Path(__file__).with_name('native-vim9-buffer-smoke.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class WorkspaceProfileTests(unittest.TestCase):
    def fixture(self, mode='block'):
        image = Image.new('RGB', (1400, 950), (9, 19, 35))
        rect = glass.mode_slot(dict(x=0, y=0, width=1400, height=950), mode)
        draw = ImageDraw.Draw(image)
        draw.rounded_rectangle((rect['x'], rect['y'], rect['x']+rect['width']-1, rect['y']+rect['height']-1), radius=6, fill=(62,108,255))
        draw.text((rect['x']+7, rect['y']+6), mode.title(), fill=(239,245,255))
        return image

    def test_revision3_dark_canvas_has_exact_registered_bounds_and_real_mode_witness(self):
        left, right, evidence = smoke.workspace_header_bounds(self.fixture(), 'ember')
        self.assertEqual((left, right), (330, 1110))
        self.assertEqual(evidence['paint_witness']['selected_mode'], 'block')
        self.assertIn('Registered', evidence['scope'])

    def test_wrong_or_absent_real_mode_paint_cannot_certify_canvas(self):
        for image in [self.fixture('chat'), Image.new('RGB', (1400,950), (9,19,35))]:
            with self.assertRaises(RuntimeError):
                smoke.workspace_header_bounds(image, 'ember')

    def test_legacy_light_header_does_not_replace_current_profile_witness(self):
        image = Image.new('RGB', (1400,950), (0,0,0))
        ImageDraw.Draw(image).line((292,88,1093,88), fill=(240,240,240))
        with self.assertRaises(RuntimeError):
            smoke.workspace_header_bounds(image, 'ember')


if __name__ == '__main__':
    unittest.main()
