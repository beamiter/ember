"""Pure exact geometry, rounding, stale-capture and pointer positive-control checks."""
import copy
from fractions import Fraction
import importlib.util
import io
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('geometry', Path(__file__).with_name('native-wayland-geometry.py'))
g = importlib.util.module_from_spec(spec)
spec.loader.exec_module(g)


def word(text='Remote', x=120, y=120, width=90, height=30):
    return {'text': text, 'level': '5', 'conf': '96.0', 'left': str(x), 'top': str(y),
            'width': str(width), 'height': str(height)}


def fixture(scale='1.5', floating=False, origin=(0, 0)):
    scale = Fraction(scale)
    extent = dict(x=origin[0], y=origin[1], width=int(2400 / scale), height=int(1800 / scale))
    owned = dict(x=origin[0]+30, y=origin[1]+40, width=720, height=720) if floating else extent.copy()
    output = {'name': 'HEADLESS-1', 'active': True, 'scale': float(scale), 'transform': 'normal',
              'current_mode': {'width': 2400, 'height': 1800}, 'rect': extent}
    node = {'app_id': 'io.github.beamiter.ember', 'pid': 42, 'id': 5, 'focused': True,
            'border': 'none', 'rect': owned}
    tree = {'floating_nodes' if floating else 'nodes': [node]}
    return [output], tree, owned


class FakeSmoke:
    def __init__(self, out, outputs, tree):
        self.out = Path(out)
        self.outputs, self.tree = outputs, tree
        self.app = SimpleNamespace(pid=42)
        self.pointer = SimpleNamespace(stdin=io.StringIO(), poll=lambda: None)
        self.acks = []
        self.captures = []
        self.dimensions = '2400 1800'
        self.after_capture = None

    def run(self, args, **kwargs):
        if args[0] == 'identify':
            return SimpleNamespace(stdout=self.dimensions)
        return SimpleNamespace(stdout=json.dumps(self.outputs if args[2] == 'get_outputs' else self.tree))

    def screenshot(self, label):
        self.captures.append(label)
        if self.after_capture:
            self.after_capture()

    def pointer_ack(self, expected):
        self.acks.append(expected)


class GeometryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.outputs, self.tree, self.owned = fixture()
        self.measured = g.layout(self.outputs, self.tree, 42, '1.5', self.owned, False)
        self.smoke = FakeSmoke(self.temp.name, self.outputs, self.tree)

    def test_owned_settings_ocr_uses_exact_physical_crop_at_every_scale(self):
        for scale, expected in [('1', (30,40,720,720)), ('1.5', (45,60,1080,1080)), ('2', (60,80,1440,1440))]:
            outputs, tree, owned = fixture(scale, True, origin=(100,-50))
            measured = g.layout(outputs, tree, 42, scale, owned, True)
            self.assertEqual(g.physical_app_rect(measured), dict(zip(('x','y','width','height'),expected)))
        broken = copy.deepcopy(measured); broken['app_rect']['x'] = -10000
        with self.assertRaises(RuntimeError):g.physical_app_rect(broken)
        broken = copy.deepcopy(measured); broken['scale'] = '5/4'
        with self.assertRaises(RuntimeError):g.physical_app_rect(broken)

    def test_owned_settings_ocr_preserves_exact_words_and_global_coordinates(self):
        from PIL import Image
        outputs, tree, owned = fixture('1.5', True)
        measured = g.layout(outputs, tree, 42, '1.5', owned, True)
        source = self.smoke.out / 'settings.png'
        Image.new('RGB', g.PHYSICAL, (15,24,38)).save(source)
        header = 'level\tleft\ttop\twidth\theight\tconf\ttext\n'
        def ocr(args, **kwargs):
            self.assertEqual(args[0], 'tesseract')
            self.assertEqual(args[-3:], ['--psm', '11', 'tsv'])
            with Image.open(args[1]) as image:self.assertEqual(image.size,(1080,1080))
            return SimpleNamespace(stdout=header+'5\t100\t100\t50\t20\t96\tRemote\n')
        original = source.read_bytes()
        with patch.object(self.smoke, 'run', side_effect=ocr):
            rows = g.settings_words(self.smoke, 'settings', measured, psm=11)
        self.assertEqual(rows[0]['text'],'Remote')
        self.assertEqual((rows[0]['left'], rows[0]['top']),('145','160'))
        self.assertEqual(source.read_bytes(),original)
        metadata = json.loads((self.smoke.out/'settings-owned-ocr.json').read_text())
        self.assertEqual(metadata['psm'],11)
        self.assertEqual(metadata['physical_region'],dict(x=45,y=60,width=1080,height=1080))

    def test_owned_settings_ocr_rejects_cross_crop_words_and_mutated_pixels(self):
        from PIL import Image
        outputs, tree, owned = fixture('1.5', True)
        measured = g.layout(outputs, tree, 42, '1.5', owned, True)
        source = self.smoke.out/'settings.png'
        header = 'level\tleft\ttop\twidth\theight\tconf\ttext\n'
        for case in ['cross-crop', 'changed-source']:
            Image.new('RGB', g.PHYSICAL, (15,24,38)).save(source)
            def ocr(*args, **kwargs):
                if case == 'changed-source':source.write_bytes(b'changed')
                x = 1070 if case == 'cross-crop' else 100
                return SimpleNamespace(stdout=header+f'5\t{x}\t100\t50\t20\t96\tRemote\n')
            with patch.object(self.smoke, 'run', side_effect=ocr), self.assertRaises(RuntimeError):
                g.settings_words(self.smoke, 'settings', measured, psm=6)
        with self.assertRaises(RuntimeError):g.settings_words(self.smoke,'settings',measured,psm=7)

    def test_six_actual_geometry_combinations(self):
        for scale in ('1', '1.5', '2'):
            for floating in (False, True):
                outputs, tree, owned = fixture(scale, floating)
                result = g.layout(outputs, tree, 42, scale, owned, floating)
                self.assertEqual(result['app_rect'], owned)
                self.assertEqual(result['floating'], floating)
                self.assertEqual(result['physical'], [2400, 1800])

    def test_scale_transform_mode_extent_and_multioutput_rejected(self):
        for change in [{'scale': 2}, {'transform': '90'}, {'current_mode': {'width': 1280, 'height': 900}},
                       {'rect': dict(self.outputs[0]['rect'], width=1599)}, {'active': False}]:
            outputs = [dict(self.outputs[0], **change)]
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                g.layout(outputs, self.tree, 42, '1.5', self.owned, False)
        for outputs in ([], self.outputs * 2):
            with self.assertRaises(RuntimeError):
                g.layout(outputs, self.tree, 42, '1.5', self.owned, False)
        with self.assertRaises(RuntimeError):
            g.layout(self.outputs, self.tree, 42, '1.25', self.owned, False)

    def test_exact_narrow_size_and_floating_required(self):
        outputs, tree, owned = fixture('2', True)
        for mutate in [lambda n: n['rect'].update(width=721), lambda n: n['rect'].update(height=719),
                       lambda n: n['rect'].update(x=31), lambda n: n.update(border='normal')]:
            altered = copy.deepcopy(tree)
            mutate(altered['floating_nodes'][0])
            with self.assertRaises(RuntimeError):
                g.layout(outputs, altered, 42, '2', owned, True)
        with self.assertRaises(RuntimeError):
            g.layout(outputs, {'nodes': tree['floating_nodes']}, 42, '2', owned, True)

    def test_wrong_pid_focus_duplicate_missing_and_offoutput(self):
        for mutate in [lambda n: n.update(pid=43), lambda n: n.update(focused=False),
                       lambda n: n['rect'].update(x=-1), lambda n: n['rect'].update(width='1600')]:
            tree = copy.deepcopy(self.tree)
            mutate(tree['nodes'][0])
            with self.assertRaises(RuntimeError):
                g.layout(self.outputs, tree, 42, '1.5', self.owned, False)
        for tree in ({}, {'nodes': self.tree['nodes'] * 2}):
            with self.assertRaises(RuntimeError):
                g.layout(self.outputs, tree, 42, '1.5', self.owned, False)

    def test_exact_rational_conversion_at_all_scales(self):
        for scale, expected in [('1', (165, 135, 2400, 1800)),
                                ('1.5', (110, 90, 1600, 1200)), ('2', (82, 67, 1200, 900))]:
            outputs, tree, owned = fixture(scale)
            measured = g.layout(outputs, tree, 42, scale, owned, False)
            self.assertEqual(g.pointer_target(word(), measured), expected)

    def test_output_origin_removed_for_normalized_layout_pointer(self):
        outputs, tree, owned = fixture('1.5', origin=(100, -50))
        measured = g.layout(outputs, tree, 42, '1.5', owned, False)
        logical = g.logical_box(word(), measured)
        self.assertEqual((logical['x'], logical['y']), (180, 30))
        self.assertEqual(g.pointer_target(word(), measured), (110, 90, 1600, 1200))

    def test_edges_rounding_and_no_clamping(self):
        self.assertEqual(g.pointer_target(word(x=0, y=0, width=6, height=6), self.measured), (2, 2, 1600, 1200))
        self.assertEqual(g.pointer_target(word(x=2394, y=1794, width=6, height=6), self.measured), (1598, 1198, 1600, 1200))
        for row in [word(x=-1), word(x=2399), word(y=1799), word(width=0),
                    word(x=0, y=0, width=1, height=1)]:
            with self.subTest(row=row), self.assertRaises(RuntimeError):
                g.pointer_target(row, self.measured)

        self.assertEqual(g.pointer_target(word(x=1, y=1, width=1, height=1), self.measured), (1, 1, 1600, 1200))

    def test_clipped_and_outside_app_exact_words_rejected(self):
        outputs, tree, owned = fixture('1.5', True)
        measured = g.layout(outputs, tree, 42, '1.5', owned, True)
        for row in [word(x=0), word(x=1100), word(y=10)]:
            with self.assertRaises(RuntimeError):
                g.exact_words([row], 'Remote', measured)

    def test_empty_malformed_and_literal_word_matching(self):
        for rows in [[], [{}], [dict(word(), conf='nan')], [dict(word(), level='4')]]:
            with self.assertRaises(RuntimeError):
                g.exact_words(rows, 'Remote', self.measured)
        for text in ['remote', 'Remote.', 'XRemote']:
            self.assertEqual(g.exact_words([word(text)], 'Remote', self.measured), [])

    def test_capture_brackets_geometry_and_actual_image_dimensions(self):
        self.assertEqual(g.capture(self.smoke, 'capture', '1.5', self.owned, False), self.measured)
        self.assertEqual(self.smoke.captures, ['capture'])
        for suffix in ['before-outputs', 'before-tree', 'after-outputs', 'after-tree']:
            self.assertTrue((self.smoke.out / ('capture-' + suffix + '.json')).exists())
        self.smoke.dimensions = '1280 900'
        with self.assertRaises(RuntimeError):
            g.capture(self.smoke, 'bad', '1.5', self.owned, False)

    def test_geometry_change_during_capture_rejected(self):
        self.smoke.after_capture = lambda: self.tree['nodes'][0].update(id=6)
        with self.assertRaises(RuntimeError):
            g.capture(self.smoke, 'changed', '1.5', self.owned, False)

    def test_stale_geometry_before_click_sends_no_input(self):
        for mutate in [lambda: self.tree['nodes'][0].update(id=6),
                       lambda: self.outputs[0].update(name='HEADLESS-2'),
                       lambda: self.tree['nodes'][0]['rect'].update(x=1),
                       lambda: self.outputs[0].update(scale=2)]:
            self.outputs, self.tree, self.owned = fixture()
            self.smoke.outputs, self.smoke.tree = self.outputs, self.tree
            mutate()
            with self.assertRaises(RuntimeError):
                g.click(self.smoke, 'stale', word(), self.measured, '1.5', self.owned.copy(), False)
            self.assertEqual(self.smoke.pointer.stdin.getvalue(), '')
            self.assertEqual(self.smoke.acks, [])

    def test_actual_pointer_extent_and_exact_once_click(self):
        result = g.click(self.smoke, 'click', word(), self.measured, '1.5', self.owned, False)
        self.assertEqual(result, (110, 90, 1600, 1200))
        self.assertEqual(self.smoke.pointer.stdin.getvalue(), 'click 110 90 1600 1200\n')
        self.assertEqual(self.smoke.acks, ['done'])

    def test_pointer_exit_and_postclick_change_fail(self):
        self.smoke.pointer.poll = lambda: 2
        with self.assertRaises(RuntimeError):
            g.click(self.smoke, 'dead', word(), self.measured, '1.5', self.owned, False)
        self.assertEqual(self.smoke.pointer.stdin.getvalue(), '')
        self.smoke.pointer.poll = lambda: None
        self.smoke.pointer_ack = lambda expected: self.tree['nodes'][0].update(id=6)
        with self.assertRaises(RuntimeError):
            g.click(self.smoke, 'changed', word(), self.measured, '1.5', self.owned, False)
        self.assertEqual(self.smoke.pointer.stdin.getvalue().count('click'), 1)

    def test_navigation_anchors_and_remote_heading(self):
        rows = [word('Appearance', x=120), word('Advanced', x=240), word('Remote', x=360)]
        self.assertEqual(g.remote_tab(rows, self.measured), rows[2])
        for invalid in [rows + [rows[2]], rows[:2], rows + [word('Hosts', y=180)],
                        [rows[0], rows[1], word('Remote', y=180)]]:
            with self.assertRaises(RuntimeError):
                g.remote_tab(invalid, self.measured)
        after = rows + [word('Remote', y=180), word('Hosts', x=240, y=180)]
        self.assertEqual(len(g.remote_heading(after, self.measured)), 2)
        for invalid in [rows, after + [after[-1]], rows + [word('Hosts', x=240, y=180)],
                        rows + [word('Remote', x=300, y=180), word('Hosts', x=240, y=180)]]:
            with self.assertRaises(RuntimeError):
                g.remote_heading(invalid, self.measured)


if __name__ == '__main__':
    unittest.main()
