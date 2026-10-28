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

    def test_reference_health_rejects_wrong_duplicate_or_unregistered_pty_marker(self):
        transcript = self.smoke.out / 'sentinels.txt'
        cases = [
            ('ime-ready', b'preedit-switch-ready:42:/dev/pts/1\n'),
            ('ime-ready', b'ime-ready:42:/dev/pts/1\nime-ready:42:/dev/pts/1\n'),
            ('ime-ready', b'prefix-ime-ready:42:/dev/pts/1\n'),
            ('unknown', b'unknown:42:/dev/pts/1\n'),
        ]
        for marker, content in cases:
            with self.subTest(marker=marker, content=content):
                transcript.write_bytes(content)
                calls = []
                smoke = SimpleNamespace(
                    app=SimpleNamespace(poll=lambda: None),
                    transcript=transcript, reference_readiness_marker=marker,
                    run=lambda *args, **kwargs: calls.append(args))
                rectangle = {'rect': {'x': 0, 'y': 0, 'width': 1280, 'height': 900}}
                with self.assertRaises(RuntimeError):
                    visual.reference_header_health(smoke, 'health', None, rectangle)
                self.assertEqual(calls, [], 'Invalid identity must fail before pixel processing')

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

    def test_preselected_sparse_health_layout_uses_same_image_in_every_phase(self):
        before = self.fallback_fixture([word('terminal')], [self.smoke.anchor], baseline=False)
        self.assertIsNone(before)
        baseline = visual.snapshot(self.smoke, 'before', None, False, health_psm=11)
        result = visual.snapshot(self.smoke, 'after', baseline, False, health_psm=11)
        self.assertEqual(result['health_ocr_psm'], 11)
        self.assertEqual(result['anchor_box'], baseline['anchor_box'])
        self.assertEqual(self.smoke.captures, ['before', 'after'])
        self.assertEqual(self.smoke.word_calls, [
            ('before', {'psm': 6, 'capture': False}), ('before', {'psm': 11, 'capture': False}),
            ('after', {'psm': 6, 'capture': False}), ('after', {'psm': 11, 'capture': False})])

    def test_sparse_health_cannot_hide_wrong_anchor_token_or_mutated_pixels(self):
        for case in ['missing', 'duplicate', 'outside', 'moved', 'token', 'sparse-token', 'mutated']:
            with self.subTest(case=case):
                self.smoke.words = Smoke.words.__get__(self.smoke, Smoke)
                baseline = self.capture(label='before')
                alternate = {'missing': [word('other')], 'duplicate': [self.smoke.anchor]*2,
                             'outside': [dict(self.smoke.anchor, left='0')],
                             'moved': [dict(self.smoke.anchor, left='16')],
                             'sparse-token': [self.smoke.anchor, self.token]}.get(case, [self.smoke.anchor])
                self.fallback_fixture([self.token] if case == 'token' else [word('terminal')],
                                      alternate, baseline=False, mutate=case == 'mutated')
                with self.assertRaises(RuntimeError):
                    visual.snapshot(self.smoke, 'after', baseline, False, health_psm=11)

    def test_invalid_health_layout_rejected_before_capture(self):
        with self.assertRaises(RuntimeError):
            visual.snapshot(self.smoke, 'invalid', None, False, health_psm=7)
        self.assertEqual(self.smoke.captures, [])

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
                              ('EMBERPREEDITMARKER:',False),('EMBERPREEOITMARKER',False),('→',False)]:
            with self.subTest(token=token),tempfile.TemporaryDirectory() as d:
                smoke=Smoke(d);before=visual.snapshot(smoke,'before',None,False)
                calls=[]
                def words(label,**kwargs):
                    calls.append(kwargs['psm'])
                    return [smoke.anchor,word('EMBERPREEDITMARKER:' if kwargs['psm']==6 else token)]
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


class PostCommitCaptureTests(unittest.TestCase):
    def test_slow_passive_ocr_runs_after_bounded_input_finalization(self):
        with tempfile.TemporaryDirectory() as directory:
            smoke=Smoke(directory)
            before=visual.snapshot(smoke,'before',None,False)
            clock={'now':5,'open':True};events=[];original=smoke.words
            def finalize():
                self.assertTrue((smoke.out/'after.png').exists())
                self.assertLess(clock['now'],15)
                events.extend(['helper-exited','collector-drained'])
                clock['open']=False
            def slow_words(label,**kwargs):
                clock['now']+=20
                self.assertFalse(clock['open'],'OCR held collector past its 15-second bound')
                events.append('offline-ocr')
                return original(label,**kwargs)
            smoke.words=slow_words
            after=visual.snapshot(smoke,'after',before,False,after_capture=finalize)
            self.assertEqual(after['observed_token_count'],0)
            self.assertEqual(events,['helper-exited','collector-drained','offline-ocr'])
            self.assertGreater(clock['now'],15)

    def test_invalid_capture_never_finalizes_and_mutation_is_rejected(self):
        for case in ['owner','mutation','initial','presence']:
            with self.subTest(case=case),tempfile.TemporaryDirectory() as directory:
                smoke=Smoke(directory);before=visual.snapshot(smoke,'before',None,False);events=[]
                def finalize():
                    events.append('finalize')
                    if case=='mutation':(smoke.out/'after.png').write_bytes(b'changed')
                if case=='owner':smoke.node['pid']=99
                with self.assertRaises(RuntimeError):
                    visual.snapshot(smoke,'after',None if case=='initial' else before,
                                    case=='presence',after_capture=finalize)
                self.assertEqual(events,['finalize'] if case=='mutation' else [])

    def test_missing_or_extra_input_still_rejected_before_offline_ocr(self):
        spec=importlib.util.spec_from_file_location('capture_desktop',Path(__file__).with_name('native-desktop-smoke.py'))
        desktop=importlib.util.module_from_spec(spec);spec.loader.exec_module(desktop)
        for actual in [b'', '你'.encode()+b'\r']:
            with self.subTest(actual=actual),tempfile.TemporaryDirectory() as directory:
                smoke=Smoke(directory);before=visual.snapshot(smoke,'before',None,False)
                capture=Path(directory)/'raw';capture.mkdir()
                (capture/'bytes.bin').write_bytes(actual)
                (capture/'result.json').write_text(json.dumps({'status':'pass','shell_pid':42,'tty':'/dev/pts/0'}))
                count=len(smoke.word_calls)
                def finalize():desktop.Smoke.raw_finish(smoke,capture,'42:/dev/pts/0','你'.encode())
                with self.assertRaises(AssertionError):
                    visual.snapshot(smoke,'after',before,False,after_capture=finalize)
                self.assertEqual(len(smoke.word_calls),count)


class RegisteredLayoutTests(unittest.TestCase):
    def test_layout_is_selected_before_observation_not_retried(self):
        source=Path(__file__).with_name('native-ime-return-smoke.py').read_text()
        self.assertIn("token_psm=11 if args.mode in ('normal', 'chat') else 6", source)
        geometry=Path(__file__).with_name('native-wayland-geometry-smoke.py').read_text()
        self.assertIn("words('remote-tab-before', psm=11, capture=False)", geometry)


class FooterSmoke(Smoke):
    def __init__(self, out):
        super().__init__(out)
        self.node['rect'] = {'x': 0, 'y': 25, 'width': 1280, 'height': 875}
        self.rows = [word('terminal', y=100)]
        self.footer_rows = [word(visual.HEALTH_ANCHOR, x=174, y=7, width=46, height=10)]
        self.footer_dimensions = '1280 24'
        self.mutate_footer = None

    def run(self, args, **kwargs):
        if args[0] == 'convert' and str(args[-1]).endswith('-health-footer.png'):
            self.commands.append((args, kwargs))
            Path(args[-1]).write_bytes(b'footer pixels')
            return SimpleNamespace(stdout='')
        if args[0] == 'identify' and str(args[-1]).endswith('-health-footer.png'):
            self.commands.append((args, kwargs))
            return SimpleNamespace(stdout=self.footer_dimensions)
        return super().run(args, **kwargs)

    def words(self, label, **kwargs):
        self.word_calls.append((label, kwargs))
        if kwargs['psm'] == 7:
            columns = list(word('header'))
            tsv = '\t'.join(columns) + '\n'
            tsv += ''.join('\t'.join(row.get(key, '') for key in columns) + '\n'
                           for row in self.footer_rows)
            (self.out / (label + '-psm7.tsv')).write_text(tsv)
            if self.mutate_footer is not None:
                self.mutate_footer(label)
            return self.footer_rows
        return self.rows


class FooterHealthTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.smoke = FooterSmoke(self.temp.name)

    def capture(self, label='test', baseline=None, **kwargs):
        return visual.snapshot(self.smoke, label, baseline, False,
                               health_psm=11, health_footer=True, **kwargs)

    def test_fixed_footer_preserves_source_and_maps_exact_anchor_and_artifacts(self):
        result = self.capture()
        self.assertEqual(self.smoke.captures, ['test'])
        self.assertEqual(result['health_ocr_psm'], 7)
        self.assertEqual(result['anchor_box'], {'x': 174, 'y': 883, 'width': 46, 'height': 10})
        self.assertEqual(result['image_sha256'], hashlib.sha256(b'fixture').hexdigest())
        evidence = result['health_footer']
        self.assertEqual(evidence['rectangle'], visual.FOOTER_RECT)
        self.assertEqual(evidence['image_sha256'], hashlib.sha256(b'footer pixels').hexdigest())
        self.assertEqual(evidence['ocr_psm'], 7)
        self.assertTrue(Path(evidence['image']).is_file())
        self.assertTrue(Path(evidence['tsv']).is_file())
        self.assertEqual(Path(result['image']).read_bytes(), b'fixture')
        convert = [args for args, _ in self.smoke.commands if args[0] == 'convert']
        self.assertEqual(convert, [['convert', result['image'], '-crop',
                                   '1280x24+0+876', '+repage', evidence['image']]])
        self.assertEqual(self.smoke.word_calls, [
            ('test', {'psm': 6, 'capture': False}), ('test', {'psm': 11, 'capture': False}),
            ('test-health-footer', {'psm': 7, 'capture': False})])

    def test_missing_duplicate_fuzzy_and_malformed_footer_fail_without_fallback(self):
        anchor = self.smoke.footer_rows[0]
        for rows in [[], [word('other')], [dict(anchor, text='master:')],
                     [dict(anchor, text=' master')], [anchor]*2, [dict(anchor, conf='nan')]]:
            with self.subTest(rows=rows):
                self.smoke.footer_rows = rows
                with self.assertRaises(RuntimeError):
                    self.capture(health_fallback=True)

    def test_every_local_word_must_fit_crop_before_coordinate_translation(self):
        anchor = self.smoke.footer_rows[0]
        for changes in [{'left': '-1'}, {'top': '-1'}, {'left': '1270'},
                        {'top': '20'}, {'width': '0'}, {'height': '25'}, {'top': 'bad'}]:
            for unrelated in (False, True):
                with self.subTest(changes=changes, unrelated=unrelated):
                    bad = dict(anchor, **changes)
                    self.smoke.footer_rows = [anchor, dict(bad, text='other')] if unrelated else [bad]
                    with self.assertRaises(RuntimeError):
                        self.capture()

    def test_footer_crop_must_be_owned_and_have_exact_geometry(self):
        self.smoke.node['rect']['height'] = 800
        with self.assertRaisesRegex(RuntimeError, 'outside owned'):
            self.capture()
        self.assertFalse(any(args[0] == 'convert' for args, _ in self.smoke.commands))
        self.smoke.node['rect']['height'] = 875
        for dimensions in ['', '1280 23', '1280 241280 24']:
            self.smoke.footer_dimensions = dimensions
            with self.subTest(dimensions=dimensions), self.assertRaises(RuntimeError):
                self.capture()

    def test_original_and_footer_bytes_must_remain_immutable_during_ocr(self):
        for target in ('footer', 'original'):
            with self.subTest(target=target):
                def mutate(label):
                    name = label if target == 'footer' else label.removesuffix('-health-footer')
                    (self.smoke.out / (name + '.png')).write_bytes(b'changed pixels')
                self.smoke.mutate_footer = mutate
                with self.assertRaisesRegex(RuntimeError, 'changed during OCR'):
                    self.capture()

    def test_anchor_box_must_stay_fixed_across_lifecycle(self):
        before = self.capture(label='before')
        self.smoke.footer_rows = [dict(self.smoke.footer_rows[0], left='175')]
        with self.assertRaisesRegex(RuntimeError, 'anchor moved'):
            self.capture(label='after', baseline=before)

    def test_full_frame_token_checks_cannot_be_hidden_by_healthy_footer(self):
        terminal = word('terminal', y=100)
        token = word(visual.PREEDIT_TOKEN, y=100)
        for layout in (6, 11):
            with self.subTest(layout=layout):
                def words(label, **kwargs):
                    if kwargs['psm'] == layout:
                        return [terminal, token]
                    return FooterSmoke.words(self.smoke, label, **kwargs)
                self.smoke.words = words
                with self.assertRaises(RuntimeError):
                    self.capture()

if __name__ == '__main__':
    unittest.main()
