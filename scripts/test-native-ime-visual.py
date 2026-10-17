"""Socket-free snapshot orchestration and fail-closed pixel/ownership checks."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('visual', Path(__file__).with_name('native-ime-visual.py'))
visual = importlib.util.module_from_spec(spec)
spec.loader.exec_module(visual)


def word(text, x=15, y=25, width=180, height=20):
    return {'level': '5', 'conf': '95.0', 'text': text, 'left': str(x), 'top': str(y),
            'width': str(width), 'height': str(height)}


class Smoke:
    def __init__(self, out):
        self.out = Path(out)
        self.app = SimpleNamespace(pid=42)
        self.node = {'app_id': 'io.github.beamiter.ember', 'pid': 42, 'id': 5,
                     'focused': True, 'rect': {'x': 10, 'y': 20, 'width': 400, 'height': 300}}
        self.tree = {'nodes': [self.node]}
        self.after_tree = None
        self.outputs = [{'active': True, 'scale': 1, 'transform': 'normal', 'rect': visual.OUTPUT_RECT}]
        self.dimensions = '1280 900'
        self.anchor = word(visual.HEALTH_ANCHOR, y=280, width=60)
        self.rows = [self.anchor]
        self.captures = []
        self.word_calls = []
        self.commands = []
        self.crop_bytes = {}
        self.tree_calls = 0

    def run(self, args, **kwargs):
        self.commands.append((args, kwargs))
        if args[:3] == ['swaymsg', '-t', 'get_tree']:
            self.tree_calls += 1
            tree = self.after_tree if self.tree_calls % 2 == 0 and self.after_tree is not None else self.tree
            return SimpleNamespace(stdout=json.dumps(tree))
        if args[:3] == ['swaymsg', '-t', 'get_outputs']:
            return SimpleNamespace(stdout=json.dumps(self.outputs))
        if args[0] == 'identify':
            return SimpleNamespace(stdout=self.dimensions)
        if args[0] == 'convert':
            return SimpleNamespace(stdout=self.crop_bytes[Path(args[1]).stem])
        raise AssertionError(args)

    def screenshot(self, label):
        self.captures.append(label)
        (self.out / (label + '.png')).write_bytes(b'fixture')

    def words(self, label, **kwargs):
        self.word_calls.append((label, kwargs))
        return self.rows


class VisualTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.smoke = Smoke(self.temp.name)
        self.token = word(visual.PREEDIT_TOKEN)

    def capture(self, present=False, baseline=None, label='test'):
        return visual.snapshot(self.smoke, label, baseline, present)

    def test_same_image_single_capture_and_explicit_ocr(self):
        result = self.capture()
        self.assertEqual(result['observed_token_count'], 0)
        self.assertEqual(self.smoke.captures, ['test'])
        self.assertEqual(self.smoke.word_calls, [('test', {'psm': 6, 'capture': False})])
        self.assertEqual(self.smoke.tree_calls, 2)
        self.assertTrue((self.smoke.out / 'test-before-tree.json').exists())
        self.assertTrue((self.smoke.out / 'test-after-tree.json').exists())
        self.assertTrue((self.smoke.out / 'test-outputs.json').exists())
        identify = [args for args, _ in self.smoke.commands if args[0] == 'identify']
        self.assertEqual(identify[0][-1], result['image'])

    def test_present_absent_and_duplicate(self):
        for count, present, success in [(0, False, True), (1, True, True),
                                       (0, True, False), (1, False, False), (2, True, False), (2, False, False)]:
            with self.subTest(count=count, present=present):
                self.smoke.rows = [self.smoke.anchor] + [self.token] * count
                if success:
                    self.capture(present)
                else:
                    with self.assertRaises(RuntimeError):
                        self.capture(present)

    def test_outside_crossing_and_invalid_tokens_fail_even_absence(self):
        for changes in [{'left': '0'}, {'top': '0'}, {'left': '400'}, {'top': '310'},
                        {'width': '0'}, {'left': '-1'}, {'left': 'bad'}]:
            self.smoke.rows = [self.smoke.anchor, dict(self.token, **changes)]
            for present in (False, True):
                with self.subTest(changes=changes, present=present), self.assertRaises(RuntimeError):
                    self.capture(present)

    def test_empty_malformed_and_unhealthy_ocr_fail(self):
        for rows in [[], [{}], [dict(self.smoke.anchor, conf='nan')],
                     [dict(self.smoke.anchor, level='4')], [dict(self.smoke.anchor, text='')],
                     [word('other')], [self.smoke.anchor] * 2,
                     [dict(self.smoke.anchor, left='0')]]:
            self.smoke.rows = rows
            with self.subTest(rows=rows), self.assertRaises(RuntimeError):
                self.capture()

    def test_fuzzy_matching_never_satisfies_presence(self):
        for token in [visual.PREEDIT_TOKEN.lower(), visual.PREEDIT_TOKEN + '.',
                      'X' + visual.PREEDIT_TOKEN, visual.PREEDIT_TOKEN.replace('I', 'l')]:
            self.smoke.rows = [self.smoke.anchor, word(token)]
            with self.assertRaises(RuntimeError):
                self.capture(True)

    def test_ownership_focus_and_output_containment(self):
        original = copy.deepcopy(self.smoke.tree)
        for mutate in [lambda n: n.update(pid=43), lambda n: n.update(focused=False),
                       lambda n: n['rect'].update(x=-1), lambda n: n['rect'].update(width=1280),
                       lambda n: n['rect'].update(height='300')]:
            self.smoke.tree = copy.deepcopy(original)
            mutate(self.smoke.tree['nodes'][0])
            with self.assertRaises(RuntimeError):
                self.capture()
        for nodes in [[], [original['nodes'][0]] * 2]:
            self.smoke.tree = {'nodes': nodes}
            with self.assertRaises(RuntimeError):
                self.capture()

    def test_geometry_changes_across_capture_and_lifecycle(self):
        before = self.capture()
        self.smoke.after_tree = copy.deepcopy(self.smoke.tree)
        self.smoke.after_tree['nodes'][0]['rect']['x'] += 1
        with self.assertRaises(RuntimeError):
            self.capture()
        self.smoke.after_tree = None
        self.smoke.node['id'] += 1
        with self.assertRaises(RuntimeError):
            self.capture(baseline=before)

    def test_output_and_actual_image_geometry(self):
        original = copy.deepcopy(self.smoke.outputs)
        for outputs in [[], original * 2, [dict(original[0], scale=1.5)],
                        [dict(original[0], transform='90')], [dict(original[0], rect={})]]:
            self.smoke.outputs = outputs
            with self.assertRaises(RuntimeError):
                self.capture()
        self.smoke.outputs = original
        for dimensions in ['640 450', '', '1280 9001280 900']:
            self.smoke.dimensions = dimensions
            with self.assertRaises(RuntimeError):
                self.capture()

    def test_anchor_must_remain_at_exact_box(self):
        before = self.capture()
        self.smoke.rows = [dict(self.smoke.anchor, left='16')]
        with self.assertRaises(RuntimeError):
            self.capture(baseline=before)

    def lifecycle(self):
        before = self.capture(label='before')
        self.smoke.rows += [self.token]
        during = self.capture(True, before, 'during')
        self.smoke.rows = [self.smoke.anchor]
        after = self.capture(False, before, 'after')
        count = 180 * 20 * 3
        self.smoke.crop_bytes = {'before': b'\x01' * count, 'during': b'\x02' * count, 'after': b'\x01' * count}
        return before, during, after

    def test_exact_raw_pixel_restoration_and_artifacts(self):
        result = visual.verify_restoration(self.smoke, *self.lifecycle())
        self.assertTrue(result['during_differs'])
        self.assertTrue(result['after_restored'])
        self.assertEqual(result['sha256']['before'], result['sha256']['after'])
        for phase in ['before', 'during', 'after']:
            self.assertTrue((self.smoke.out / ('preedit-region-' + phase + '.rgb')).exists())
        for args, kwargs in self.smoke.commands:
            if args[0] == 'convert':
                self.assertEqual(args[2:], ['-crop', '180x20+15+25', '+repage', '-colorspace',
                                           'sRGB', '-alpha', 'off', '-depth', '8', 'RGB:-'])
                self.assertNotIn('text', kwargs)

    def test_ocr_absence_cannot_hide_persistent_or_changed_pixels(self):
        lifecycle = self.lifecycle()
        for value in [self.smoke.crop_bytes['during'], b'\x03' * (180 * 20 * 3), b'']:
            self.smoke.crop_bytes['after'] = value
            with self.assertRaises(RuntimeError):
                visual.verify_restoration(self.smoke, *lifecycle)
        self.smoke.crop_bytes['after'] = self.smoke.crop_bytes['before']
        self.smoke.crop_bytes['during'] = self.smoke.crop_bytes['before']
        with self.assertRaises(RuntimeError):
            visual.verify_restoration(self.smoke, *lifecycle)

    def fallback_fixture(self, primary, alternate, *, baseline=True, mutate=False):
        before = self.capture(label='before') if baseline else None
        self.smoke.captures.clear()
        self.smoke.word_calls.clear()
        def words(label, **kwargs):
            self.smoke.word_calls.append((label, kwargs))
            if kwargs['psm'] == 11:
                if mutate:
                    (self.smoke.out / (label + '.png')).write_bytes(b'changed')
                return alternate
            return primary
        self.smoke.words = words
        return before

    def test_fallback_same_image_exact_anchor_and_hash(self):
        before = self.fallback_fixture([word('healthy')], [self.smoke.anchor])
        result = visual.snapshot(self.smoke, 'test', before, False, health_fallback=True)
        self.assertEqual(result['health_ocr_psm'], 11)
        self.assertEqual(result['anchor_box'], before['anchor_box'])
        self.assertEqual(result['image_sha256'], hashlib.sha256(b'fixture').hexdigest())
        self.assertEqual(self.smoke.captures, ['test'])
        self.assertEqual(self.smoke.word_calls, [('test', {'psm': 6, 'capture': False}),
                                               ('test', {'psm': 11, 'capture': False})])

    def test_primary_success_does_not_consult_alternative(self):
        before = self.fallback_fixture([self.smoke.anchor], [])
        result = visual.snapshot(self.smoke, 'test', before, False, health_fallback=True)
        self.assertEqual(result['health_ocr_psm'], 6)
        self.assertEqual(len(self.smoke.word_calls), 1)

    def test_primary_duplicate_wrong_box_or_missing_baseline_cannot_fallback(self):
        for primary, baseline in [([self.smoke.anchor]*2, True),
                                  ([dict(self.smoke.anchor, left='16')], True),
                                  ([word('healthy')], False)]:
            with self.subTest(primary=primary, baseline=baseline):
                # Restore the fake's original words method before each baseline.
                self.smoke.words = Smoke.words.__get__(self.smoke, Smoke)
                before = self.fallback_fixture(primary, [self.smoke.anchor], baseline=baseline)
                with self.assertRaises(RuntimeError):
                    visual.snapshot(self.smoke, 'test', before, False, health_fallback=True)
                self.assertEqual(len(self.smoke.word_calls), 1)

    def test_fallback_missing_duplicate_wrong_box_and_outside_rejected(self):
        for alternate in [[word('healthy')], [self.smoke.anchor]*2,
                          [dict(self.smoke.anchor, left='16')],
                          [dict(self.smoke.anchor, left='0')], []]:
            with self.subTest(alternate=alternate):
                self.smoke.words = Smoke.words.__get__(self.smoke, Smoke)
                before = self.fallback_fixture([word('healthy')], alternate)
                with self.assertRaises(RuntimeError):
                    visual.snapshot(self.smoke, 'test', before, False, health_fallback=True)

    def test_fallback_cannot_hide_token_or_changed_image(self):
        for alternate, mutate in [([self.smoke.anchor,self.token], False),
                                  ([self.smoke.anchor], True)]:
            with self.subTest(mutate=mutate):
                self.smoke.words = Smoke.words.__get__(self.smoke, Smoke)
                before = self.fallback_fixture([word('healthy')], alternate, mutate=mutate)
                with self.assertRaises(RuntimeError):
                    visual.snapshot(self.smoke, 'test', before, False, health_fallback=True)

    def test_helper_and_visual_token_match(self):
        source = Path(__file__).with_name('native-ime-return.c').read_text()
        self.assertIn('static const char preedit_token[] = "' + visual.PREEDIT_TOKEN + '";', source)
        self.assertNotIn(visual.PREEDIT_TOKEN, Path(__file__).with_name('native-ime-return-smoke.py').read_text())


class SelectedLayoutTests(unittest.TestCase):
    def test_presence_layout_never_trims_arrow_or_fuzzy_text(self):
        for token,success in [('EMBERPREEDITMARKER',True),(' EMBERPREEDITMARKER',False),
                              ('→EMBERPREEDITMARKER',False),('EMBERPREEDITMARKER ',False),
                              ('EMBERPREEOITMARKER',False),('→',False)]:
            with self.subTest(token=token),tempfile.TemporaryDirectory() as d:
                smoke=Smoke(d);before=visual.snapshot(smoke,'before',None,False)
                calls=[]
                def words(label,**kwargs):
                    calls.append(kwargs['psm'])
                    return [smoke.anchor,word(' EMBERPREEDITMARKER' if kwargs['psm']==6 else token)]
                smoke.words=words
                if success:
                    result=visual.snapshot(smoke,'during',before,True,token_psm=11)
                    self.assertEqual(result['token_ocr_psm'],11)
                else:
                    with self.assertRaises(RuntimeError):visual.snapshot(smoke,'during',before,True,token_psm=11)
                self.assertEqual(calls,[6,11])
    def test_selected_layout_missing_baseline_absence_or_unsupported_rejected_before_capture(self):
        for baseline,present,psm in [(None,True,11),({},False,11),({},True,7)]:
            with tempfile.TemporaryDirectory() as d:
                smoke=Smoke(d)
                with self.assertRaises(RuntimeError):visual.snapshot(smoke,'bad',baseline,present,token_psm=psm)
                self.assertEqual(smoke.captures,[])
    def test_selected_presence_rejects_duplicate_moved_outside_or_mutated_evidence(self):
        for case in ['duplicate','moved','outside','mutated','missing-anchor']:
            with self.subTest(case=case),tempfile.TemporaryDirectory() as d:
                smoke=Smoke(d);before=visual.snapshot(smoke,'before',None,False)
                def words(label,**kwargs):
                    if kwargs['psm']==6:
                        return ([word('other')] if case=='missing-anchor' else [smoke.anchor])+([word(visual.PREEDIT_TOKEN)] if case=='moved' else [])
                    if case=='mutated':(smoke.out/(label+'.png')).write_bytes(b'changed')
                    token=word(visual.PREEDIT_TOKEN,x=0 if case=='outside' else 16)
                    return [smoke.anchor]+[token]*(2 if case=='duplicate' else 1)
                smoke.words=words
                with self.assertRaises(RuntimeError):visual.snapshot(smoke,'during',before,True,token_psm=11)

if __name__ == '__main__':
    unittest.main()
