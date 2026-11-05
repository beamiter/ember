"""Pure RGB target regressions, not native wheel-input acceptance."""
import importlib.util
from pathlib import Path
import unittest
from PIL import Image, ImageDraw

spec = importlib.util.spec_from_file_location('shell_smoke', Path(__file__).with_name('native-vim9-shell-smoke.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class NativeHoverTests(unittest.TestCase):
    def test_hover_targets_left_native_ink_instead_of_workspace_navigation(self):
        image = Image.new('RGB', (1400, 950), (15, 24, 38))
        draw = ImageDraw.Draw(image)
        draw.rectangle((358, 110, 380, 125), fill=(0x19, 0xc3, 0xa7))
        draw.rectangle((738, 110, 760, 125), fill=(0x19, 0xc3, 0xa7))
        target = smoke.leftmost_native_target(image)
        self.assertEqual((target['x'], target['y']), (358, 110))
        self.assertEqual(target['native_rgb_pixels'], 736)

    def test_missing_or_insufficient_native_ink_cannot_supply_a_coordinate(self):
        for width in [0, 5]:
            image = Image.new('RGB', (1400, 950), (15, 24, 38))
            if width:
                ImageDraw.Draw(image).rectangle((200, 320, 204, 324), fill=(0x19, 0xc3, 0xa7))
            with self.assertRaises(RuntimeError):
                smoke.leftmost_native_target(image)


if __name__ == '__main__':
    unittest.main()
