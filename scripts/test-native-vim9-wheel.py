"""Preserve the wheel evidence gate while improving pixel observation cadence."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('vim_wheel_smoke', Path(__file__).with_name('native-vim9-buffer-smoke.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class NativeWheelEvidenceTests(unittest.TestCase):
    def positions(self, ys):
        return smoke.fractional_wheel_positions(list(enumerate(ys)), 520, 439, 27)

    def test_failed_ci_trace_is_still_rejected(self):
        # Run 42030654704 sampled only these positions, 34-49 ms apart.
        self.assertEqual(self.positions([520, 519, 468, 439, 439, 439, 439, 439]), [])

    def test_complete_rows_never_count_as_fractional(self):
        self.assertEqual(self.positions([520, 493, 466, 439]), [])

    def test_missing_outside_and_near_integer_pixels_are_rejected(self):
        self.assertEqual(self.positions([None, 530, 430, 519, 518, 495, 491, 468, 464, 440]), [])

    def test_real_intermediate_pixels_outside_rejection_band_pass(self):
        self.assertEqual(self.positions([520, 506, 480, 439]), [506, 480])


if __name__ == '__main__':
    unittest.main()
