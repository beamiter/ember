"""Independent native-body registration and exact occurrence union negatives."""
import importlib.util
from pathlib import Path
import unittest
import tempfile

spec = importlib.util.spec_from_file_location('visual_body', Path(__file__).with_name('native-ime-visual.py'))
visual = importlib.util.module_from_spec(spec)
spec.loader.exec_module(visual)


def word(x=20, y=40, text=visual.PREEDIT_TOKEN, width=180, height=14):
    return dict(level='5', conf='95', text=text, left=str(x), top=str(y),
                width=str(width), height=str(height))


class BodyObserverTests(unittest.TestCase):
    rectangle = dict(rect=dict(x=0, y=0, width=1280, height=900))

    def merge(self, passes):
        return visual.merge_exact_token_observations(passes, self.rectangle)[0]

    def test_missing_passes_do_not_erase_a_real_occurrence(self):
        self.assertEqual(self.merge([('canvas', []), ('body', [word()])]), [word()])

    def test_same_physical_occurrence_across_layouts_is_deduplicated(self):
        self.assertEqual(len(self.merge([('canvas', [word()]),
                                        ('body', [word()]),
                                        ('sparse', [])])), 1)

    def test_one_pixel_layout_disagreement_remains_a_geometry_error(self):
        for changed in [word(x=21), word(y=41), word(width=179), word(height=13)]:
            with self.subTest(changed=changed), self.assertRaisesRegex(RuntimeError, 'geometry'):
                self.merge([('canvas', [word()]), ('body', [changed])])

    def test_two_occurrences_from_different_bodies_remain_two(self):
        self.assertEqual(len(self.merge([('canvas', []), ('body0', [word()]),
                                        ('body1', [word(y=90)])])), 2)

    def test_canvas_duplicate_cannot_be_hidden_by_single_body_match(self):
        self.assertEqual(len(self.merge([('canvas', [word(), word(y=90)]),
                                        ('body', [word()])])), 2)

    def test_duplicate_rows_in_one_pass_fail_even_at_the_same_box(self):
        with self.assertRaisesRegex(RuntimeError, 'Duplicate'):
            self.merge([('body', [word(), word()])])

    def test_partially_overlapping_words_are_rejected_as_ambiguous(self):
        with self.assertRaisesRegex(RuntimeError, 'geometry'):
            self.merge([('canvas', [word()]), ('body', [word(x=190)])])

    def test_fuzzy_text_still_counts_as_zero(self):
        for text in [visual.PREEDIT_TOKEN.lower(), visual.PREEDIT_TOKEN+'.',
                     'X'+visual.PREEDIT_TOKEN, visual.PREEDIT_TOKEN.replace('I', 'l')]:
            with self.subTest(text=text):
                self.assertEqual(self.merge([('body', [word(text=text)])]), [])

    def test_no_word_can_cross_owned_rectangle(self):
        with self.assertRaisesRegex(RuntimeError, 'outside owned rectangle'):
            self.merge([('body', [word(x=1200)])])

    @staticmethod
    def pixels(rects, width=600, height=100):
        pixels = bytearray(bytes((250, 251, 255)) * width * height)
        for x, y, w, h in rects:
            for row in range(y, y+h):
                start = (row*width+x)*3
                pixels[start:start+w*3] = bytes((9, 12, 23))*w
        return bytes(pixels)

    def test_registration_measures_all_bodies_without_text_input(self):
        pixels = self.pixels([(10, 5, 300, 21), (320, 40, 260, 20),
                              (10, 70, 300, 24), (1, 95, 2, 2)])
        self.assertEqual(visual.native_body_rectangles(pixels, 600, 100), [
            dict(x=10, y=5, width=300, height=21), dict(x=320, y=40, width=260, height=20)])

    def test_real_caret_split_plane_reunites_before_width_filter(self):
        # Actual failed frame: left9px strip and right593px body, both21 high.
        pixels = self.pixels([(5, 5, 9, 21), (20, 5, 593, 21)], width=650)
        registration = visual.native_body_registration(pixels, 650, 100)
        self.assertEqual(len(registration), 1)
        self.assertEqual(registration[0]['rectangle'], dict(x=5, y=5, width=608, height=21))
        self.assertEqual([c['width'] for c in registration[0]['components']], [9, 593])
        self.assertEqual([c['dark_pixels'] for c in registration[0]['components']], [189, 12453])

    def test_height_or_y_one_pixel_difference_does_not_aggregate(self):
        for right in [(210, 5, 200, 20), (210, 6, 200, 21)]:
            with self.subTest(right=right):
                self.assertEqual(visual.native_body_rectangles(
                    self.pixels([(0, 5, 200, 21), right]), 600, 100), [])

    def test_same_height_separate_bodies_still_preserve_two_words(self):
        regions = visual.native_body_rectangles(
            self.pixels([(0, 5, 270, 21), (330, 5, 270, 21)]), 600, 100)
        self.assertEqual(regions, [dict(x=0, y=5, width=600, height=21)])
        occurrences = self.merge([('registered-plane', [word(x=20), word(x=350)])])
        self.assertEqual(len(occurrences), 2, 'plane union must never mean maximum token count')

    def test_thin_contract_bounds_come_from_fixture_em_and_single_line(self):
        self.assertEqual(visual.INLINE_FIXTURE_FONT_SIZE, 16)
        self.assertEqual(visual.INLINE_FIXTURE_LINE_HEIGHT, 23)
        for height, count in [(15, 0), (16, 1), (23, 1), (24, 0), (67, 0)]:
            with self.subTest(height=height):
                self.assertEqual(len(visual.native_body_rectangles(
                    self.pixels([(0, 0, 300, height)]), 600, 100)), count)

    def test_contract_missing_or_different_fields_fail_closed(self):
        valid = dict(font_size=16.0, ui_font_size=16.0, line_spacing=1.0)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'config.toml'
            for key in valid:
                for value in [None, 14.0]:
                    config = dict(valid)
                    if value is None: del config[key]
                    else: config[key] = value
                    path.write_text(''.join(f'{k}={v}\n' for k,v in config.items()))
                    with self.subTest(key=key, value=value), self.assertRaisesRegex(RuntimeError, 'Unregistered'):
                        visual.inline_body_fixture_contract(path)
            path.write_text(''.join(f'{k}={v}\n' for k,v in valid.items()))
            self.assertEqual(visual.inline_body_fixture_contract(path)['line_height'], 23)

    def test_missing_pass_requires_the_same_exact_word_in_a_thin_body(self):
        visual.require_legacy_token_geometry([word()], [], self.rectangle, [('body', [word()])])
        for bodies in [[], [('body', [])], [('body', [word(y=90)])]]:
            with self.subTest(bodies=bodies), self.assertRaisesRegex(RuntimeError, 'geometry'):
                visual.require_legacy_token_geometry([word()], [], self.rectangle, bodies)

    def test_fourteen_point_contract_requires_its_exact_registered_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'config.toml'
            path.write_text('font_size=14.0\nui_font_size=14.0\nline_spacing=1.0\n')
            contract = visual.inline_body_fixture_contract(path)
            self.assertEqual((contract['font_size'], contract['min_height'], contract['line_height']), (14, 14, 20))
            for config in ['font_size=14\nui_font_size=16\nline_spacing=1\n',
                           'font_size=15\nui_font_size=15\nline_spacing=1\n',
                           'font_size=14\nui_font_size=14\nline_spacing=1.25\n',
                           'font_size=14\nui_font_size=14\nline_spacing=true\n']:
                path.write_text(config)
                with self.subTest(config=config), self.assertRaisesRegex(RuntimeError, 'Unregistered'):
                    visual.inline_body_fixture_contract(path)

    def test_fourteen_point_body_bounds_do_not_expand_sixteen_point_contract(self):
        for height, count in [(13, 0), (14, 1), (18, 1), (20, 1), (21, 0), (23, 0)]:
            with self.subTest(height=height):
                self.assertEqual(len(visual.native_body_rectangles(
                    self.pixels([(0, 0, 300, height)]), 600, 100, min_height=14, max_height=20)), count)
        self.assertEqual(visual.INLINE_FIXTURE_REGISTRATIONS[16.0][:2], (16, 23))

    def test_fourteen_point_components_still_require_exact_collinearity(self):
        for right in [(210, 5, 200, 17), (210, 6, 200, 18)]:
            with self.subTest(right=right):
                self.assertEqual(visual.native_body_rectangles(
                    self.pixels([(0, 5, 200, 18), right]), 600, 100, min_height=14, max_height=20), [])

    def test_fourteen_point_two_bodies_do_not_reduce_to_one_occurrence(self):
        regions = visual.native_body_rectangles(self.pixels([(0, 5, 300, 18), (0, 45, 300, 18)]),
                                               600, 100, min_height=14, max_height=20)
        self.assertEqual(len(regions), 2)
        self.assertEqual(len(self.merge([('body0', [word(y=5)]), ('body1', [word(y=45)])])), 2)

    def test_fourteen_point_wrong_palette_and_invalid_height_contract_fail_closed(self):
        pixels = self.pixels([(0, 5, 300, 18)])
        self.assertEqual(visual.native_body_rectangles(pixels.replace(bytes((9, 12, 23)), bytes((10, 12, 23))),
                                                     600, 100, min_height=14, max_height=20), [])
        for minimum, maximum in [(0, 20), (14, 13), (True, 20), (14, 20.0)]:
            with self.subTest(bounds=(minimum, maximum)), self.assertRaisesRegex(RuntimeError, 'height contract'):
                visual.native_body_rectangles(pixels, 600, 100, min_height=minimum, max_height=maximum)

    def test_nonempty_layout_disagreement_never_gets_the_thin_exemption(self):
        with self.assertRaisesRegex(RuntimeError, 'geometry'):
            visual.require_legacy_token_geometry([word()], [word(x=21)], self.rectangle,
                                                 [('body', [word()])])

    def test_multiline_body_retains_legacy_missing_pass_failure(self):
        regions = visual.native_body_rectangles(self.pixels([(0, 0, 300, 67)]), 600, 100)
        self.assertEqual(regions, [])
        with self.assertRaisesRegex(RuntimeError, 'geometry'):
            visual.require_legacy_token_geometry([word()], [], self.rectangle, [])

    def test_two_thin_body_tokens_cannot_satisfy_exactly_one(self):
        occurrences = self.merge([('body0', [word()]), ('body1', [word(y=90)])])
        self.assertEqual(len(occurrences), 2)
        with self.assertRaisesRegex(RuntimeError, 'geometry'):
            visual.require_legacy_token_geometry(occurrences, [], self.rectangle,
                                                 [('body0', [word()]), ('body1', [word(y=90)])])

    def test_full_image_word_requires_one_independent_large_plane(self):
        canvas = dict(x=292,y=116,width=682,height=784)
        plane = dict(rectangle=dict(x=324,y=730,width=610,height=67))
        target = word(x=355,y=757,width=172,height=12)
        self.assertIs(visual.require_native_plane_witness(target,self.rectangle,canvas,[plane]),plane)

    def test_no_outside_crossing_header_or_inspector_word_gets_large_plane_witness(self):
        canvas = dict(x=292,y=116,width=682,height=784)
        plane = dict(rectangle=dict(x=324,y=730,width=610,height=67))
        for target in [word(x=355,y=757,width=172,height=12),word(x=310,y=757),
                       word(x=900,y=757),word(x=355,y=100),word(x=1000,y=757)]:
            planes = [] if int(target['left'])==355 and int(target['top'])==757 else [plane]
            with self.subTest(target=target), self.assertRaises(RuntimeError):
                visual.require_native_plane_witness(target,self.rectangle,canvas,planes)

    def test_ambiguous_large_planes_cannot_be_selected_by_first_match(self):
        canvas = dict(x=292,y=116,width=682,height=784)
        planes = [dict(rectangle=dict(x=324,y=700,width=610,height=100)),
                  dict(rectangle=dict(x=324,y=730,width=610,height=67))]
        with self.assertRaisesRegex(RuntimeError,'ambiguous'):
            visual.require_native_plane_witness(word(x=355,y=757),self.rectangle,canvas,planes)

    def test_large_plane_registration_still_requires_native_palette(self):
        pixels = self.pixels([(0,0,300,67)])
        self.assertEqual(len(visual.native_body_registration(pixels,600,100,single_line=False)),1)
        changed = pixels.replace(bytes((9,12,23)),bytes((10,12,23)))
        self.assertEqual(visual.native_body_registration(changed,600,100,single_line=False),[])

    def test_two_large_body_words_still_count_two(self):
        words = self.merge([('whole-image',[word(x=355,y=400),word(x=355,y=757)])])
        self.assertEqual(len(words),2)

    def test_small_icons_do_not_register_as_terminal_bodies(self):
        self.assertEqual(visual.native_body_rectangles(self.pixels([(10, 5, 28, 28)]), 600, 100), [])

    def test_incorrect_rgb_byte_count_fails_closed(self):
        with self.assertRaisesRegex(RuntimeError, 'RGB byte count'):
            visual.native_body_rectangles(b'', 600, 100)

    def test_mask_does_not_connect_rows_across_horizontal_wrap(self):
        # Two bodies touching opposite edges are separate, not adjacent bytes.
        self.assertEqual(visual.native_body_rectangles(
            self.pixels([(0, 0, 250, 20), (350, 20, 250, 20)]), 600, 100), [
                dict(x=0, y=0, width=250, height=20), dict(x=350, y=20, width=250, height=20)])


if __name__ == '__main__':
    unittest.main()
