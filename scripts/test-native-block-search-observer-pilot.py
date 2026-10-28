#!/usr/bin/env python3
"""Pure negative controls for the preregistered pilot. Not native pixel acceptance."""
import copy, hashlib, importlib.util, io, json, tempfile, unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('pilot_test',Path(__file__).with_name('native-block-search-observer-pilot.py'))
p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)

def word(text,x,y,line):
    return dict(text=text,left=str(x),top=str(y),width='35',height='9',
                page_num='1',block_num='1',par_num='1',line_num=str(line))
def words(mode='raw'):
    a,b=p.base.fixture.PILOT_MARKERS[mode]
    return [word(a.split()[0],300,160,1),word(a.split()[1],340,160,1),
            word(b.split()[0],300,174,2),word(b.split()[1],340,174,2)]
def capture():
    return dict(activation=100,started_ns=101,finished_ns=102,exclusive_fresh_path=True,png_sha256='a'*64)
def tree():
    root=p.base.public.probe.ROOT
    return dict(owner=':1.2',pid=42,root=root,nodes=[dict(path=root,parent=None,owner=':1.2',pid=42,
        name='Ember',role=23,children=[],states=[0,0],interfaces=[])])

class Tests(unittest.TestCase):
    def test_terminal_region_uses_native_surface_not_sidebar_or_marker_text(self):
        from PIL import Image, ImageDraw
        image = Image.new('RGB', (1800, 1000), (0, 0, 0))
        draw = ImageDraw.Draw(image)
        draw.rectangle((130, 150, 350, 850), fill=(9, 12, 23))  # Sidebar decoy.
        draw.rectangle((1422, 592, 1680, 730), fill=(9, 12, 23))  # Inspector decoy.
        draw.rectangle((422, 155, 1355, 858), fill=(9, 12, 23))
        self.assertEqual(p.terminal_rectangle(image, (100, 50, 1600, 900)),
                         (422, 155, 1356, 859))
        # A marker-looking label cannot move the ROI; only the fixed surface
        # boundary is observed and no expected target word is passed to it.
        draw.text((700, 300), 'TARGET PAPAYA', fill=(240, 240, 240))
        self.assertEqual(p.terminal_rectangle(image, (100, 50, 1600, 900)),
                         (422, 155, 1356, 859))

    def test_black_foreign_tiny_or_offscreen_surface_fails_closed(self):
        from PIL import Image, ImageDraw
        for case in ('black', 'foreign-only', 'tiny', 'offscreen'):
            with self.subTest(case=case):
                image = Image.new('RGB', (1800, 1000), (0, 0, 0))
                draw = ImageDraw.Draw(image)
                owned = (100, 50, 1600, 900)
                if case == 'foreign-only': draw.rectangle((1422, 592, 1680, 730), fill=(9, 12, 23))
                if case == 'tiny': draw.rectangle((450, 200, 500, 250), fill=(9, 12, 23))
                if case == 'offscreen': owned = (300, 50, 1600, 900)
                with self.assertRaises(RuntimeError): p.terminal_rectangle(image, owned)

    def test_same_observer_locates_pre_redesign_full_width_terminal(self):
        from PIL import Image, ImageDraw
        for palette in ((29, 29, 29), (39, 39, 39)):
            with self.subTest(palette=palette):
                image = Image.new('RGB', (1800, 1000), (0, 0, 0))
                ImageDraw.Draw(image).rectangle((108, 100, 1680, 905), fill=palette)
                self.assertEqual(p.terminal_rectangle(image, (100, 50, 1600, 900)),
                                 (108, 100, 1681, 906))

    def test_two_equal_wide_surfaces_are_not_a_unique_terminal(self):
        from PIL import Image, ImageDraw
        image = Image.new('RGB', (1800, 1000), (0, 0, 0))
        draw = ImageDraw.Draw(image)
        draw.rectangle((422, 155, 1355, 400), fill=(9, 12, 23))
        draw.rectangle((422, 500, 1355, 745), fill=(9, 12, 23))
        with self.assertRaisesRegex(RuntimeError, 'ambiguous'):
            p.terminal_rectangle(image, (100, 50, 1600, 900))

    def test_live_baseline_waits_for_painted_output_before_single_search_action(self):
        smoke=object.__new__(p.PilotSmoke)
        smoke.app=SimpleNamespace(pid=42)
        smoke.public=SimpleNamespace(window='owned')
        smoke.events=[]
        live=[word('LATEST',300,160,1),word('CEDAR',340,160,1)]
        observations=iter([([],{'finished_ns':1}), (live,{'finished_ns':2})])
        smoke.fresh_pixels=lambda label:next(observations)
        with patch.object(p.base.SearchSmoke,'open_find') as action, patch.object(p.time,'sleep'):
            smoke.open_find('E03EARLYHIT')
        action.assert_called_once_with('E03EARLYHIT')
        self.assertEqual(smoke.pilot_baseline['capture']['finished_ns'],2)

    def test_missing_live_baseline_never_opens_search(self):
        smoke=object.__new__(p.PilotSmoke)
        smoke.fresh_pixels=lambda label:([],{})
        with patch.object(p.base.SearchSmoke,'open_find') as action, \
                patch.object(p.time,'monotonic',side_effect=[0,1,31]),patch.object(p.time,'sleep'):
            with self.assertRaisesRegex(RuntimeError,'baseline deadline exhausted'):
                smoke.open_find('E03EARLYHIT')
        action.assert_not_called()

    def test_both_native_entry_points_use_pixel_oracle(self):
        source=Path(p.base.__file__).read_text()
        self.assertIn("smoke_class = load('e03_observer_pilot', 'native-block-search-observer-pilot.py').PilotSmoke",source)
        self.assertNotIn('if args.observer_pilot else SearchSmoke',source)

    def test_exact_pair_both_modes(self):
        for mode in ['raw','snapshot']:self.assertEqual(p.pixel_claim(words(mode),mode,capture(),100)['source'],'exact OCR words from one newly captured PNG')
    def test_picker_only_query_or_hit_is_not_body(self):
        with self.assertRaises(RuntimeError):p.pixel_claim(words()[:2],'raw',capture(),100)
        t=tree();t['nodes'][0]['name']='Search scope: Out'
        with self.assertRaises(RuntimeError):p.closed_picker(t,':1.2',42)
    def test_submitted_tree_body_never_supplies_pixel_success(self):
        t=tree();t['nodes'][0]['name']='TARGET PAPAYA NEIGHBOR PAPAYA';p.closed_picker(t,':1.2',42)
        with self.assertRaises(RuntimeError):p.pixel_claim([],'raw',capture(),100)
    def test_wrong_neighbor_unnavigated_or_duplicate_pair_fails(self):
        wrong=words();wrong[-1]['text']='WALNUT'
        for rows in [wrong, [],words()[2:],words()+words()]:
            with self.assertRaises(RuntimeError):p.pixel_claim(rows,'raw',capture(),100)
    def test_geometry_rejects_wrong_row_and_same_line(self):
        for y in [160,188,220]:
            rows=words()
            for w in rows[2:]:w['top']=str(y)
            with self.assertRaises(RuntimeError):p.pixel_claim(rows,'raw',capture(),100)
        rows=words();rows.append(word('intervening',50,171,3))
        with self.assertRaises(RuntimeError):p.pixel_claim(rows,'raw',capture(),100)
    def test_inflated_word_height_cannot_relax_adjacent_row_bound(self):
        for indices in [(2,3),(0,1)]:
            rows=words()
            for index in indices: rows[index]['height']='200'
            for row in rows[2:]: row['top']='300'
            with self.assertRaises(RuntimeError):p.pixel_claim(rows,'raw',capture(),100)

    def test_old_screenshot_or_other_activation_cannot_pass(self):
        for key,value in [('activation',99),('started_ns',99),('started_ns',100),
                          ('finished_ns',100),('exclusive_fresh_path',False),('png_sha256','')]:
            c=capture();c[key]=value
            with self.assertRaises(RuntimeError):p.pixel_claim(words(),'raw',c,100)
    def test_capture_path_reuse_rejected_before_reading_old_pixels(self):
        with tempfile.TemporaryDirectory() as tmp:
            smoke=object.__new__(p.PilotSmoke);smoke.out=Path(tmp);smoke.sequence=0
            smoke.observe=lambda *args:self.fail('Old pixels must never be read')
            (smoke.out/'0001-pilot-final-output.png').write_bytes(b'old screenshot')
            with self.assertRaisesRegex(RuntimeError,'reused observation path'):
                smoke.fresh_pixels('pilot-final-output')

    def test_far_apart_words_and_wrong_baselines_are_not_phrases(self):
        for x,y in [(1000,160),(340,180),(310,160)]:
            rows=words();rows[1]['left']=str(x);rows[1]['top']=str(y)
            with self.assertRaises(RuntimeError):p.pixel_claim(rows,'raw',capture(),100)

    def test_latest_positive_baseline_and_final_exclusion(self):
        live=[word('LATEST',300,160,1),word('CEDAR',340,160,1)]
        p.live_baseline(live)
        for rows in [[],words(),live+words()]:
            with self.assertRaises(RuntimeError):p.live_baseline(rows)
        with self.assertRaises(RuntimeError):p.pixel_claim(words()+live,'raw',capture(),100)

    def test_baseline_is_bound_and_consumed_once(self):
        smoke=object.__new__(p.PilotSmoke)
        smoke.app=SimpleNamespace(pid=42)
        smoke.public=SimpleNamespace(window='owned',expected='wrong')
        smoke.pilot_baseline=dict(query='E03EARLYHIT',mode='raw',pid=42,window='owned')
        with self.assertRaisesRegex(RuntimeError,'query changed'):
            smoke.navigate('E03EARLYHIT',16,pointer=True,raw=True)
        self.assertIsNone(smoke.pilot_baseline)
        with self.assertRaisesRegex(RuntimeError,'negative control'):
            smoke.navigate('E03EARLYHIT',16,pointer=True,raw=True)
        for key,value in [('query','other'),('mode','snapshot'),('pid',43),('window','other')]:
            smoke.pilot_baseline=dict(query='E03EARLYHIT',mode='raw',pid=42,window='owned')
            smoke.pilot_baseline[key]=value
            with self.assertRaisesRegex(RuntimeError,'negative control'):
                smoke.navigate('E03EARLYHIT',16,pointer=True,raw=True)

    def test_no_casefold_or_character_repair(self):
        for changed in ['TARGET papaya','TARGET PAPAVA','TARGETPAPAYA']:
            rows=words();rows[1]['text']=changed.split()[-1]
            with self.assertRaises(RuntimeError):p.pixel_claim(rows,'raw',capture(),100)
    def test_closed_tree_ownership_and_editor_exclusion(self):
        p.closed_picker(tree(),':1.2',42)
        for field,value in [('pid',43),('owner',':1.3')]:
            t=tree();t[field]=value
            with self.assertRaises(RuntimeError):p.closed_picker(t,':1.2',42)
        t=tree();t['nodes'][0]['role']=79
        with self.assertRaises(RuntimeError):p.closed_picker(t,':1.2',42)
    def test_fixed_stream_size_legacy_bytes_and_unique_middle_markers(self):
        for mode,count in [('raw',100000),('snapshot',2000),('flush',256)]:
            for index in range(count):
                old=p.base.fixture.line(mode,index,0)
                new=p.base.fixture.line(mode,index,0,True)
                self.assertEqual(len(old),97);self.assertEqual(len(new),97)
                if mode in ['raw','snapshot'] and index in (16,17):
                    marker=p.base.fixture.PILOT_MARKERS[mode][index-16].encode()
                    self.assertEqual(new.index(marker),36)
                    self.assertIn(f'ROW{index:06d}'.encode(),new)
                    self.assertNotIn(marker,old)
                    self.assertTrue(new.rstrip(b' \n').endswith(marker))
                elif mode == 'flush' and index == p.base.fixture.PILOT_LIVE_ROW:
                    self.assertEqual(new.index(p.base.fixture.PILOT_LIVE_MARKER.encode()),36)
                    self.assertTrue(new.rstrip(b' \n').endswith(p.base.fixture.PILOT_LIVE_MARKER.encode()))
                elif mode == 'flush' and abs(index - p.base.fixture.PILOT_LIVE_ROW) == 1:
                    # Quiet neighbors isolate the live positive control without
                    # changing source row identity, width, or byte budgets.
                    self.assertEqual(new.rstrip(b' \n'), f'E03FLUSH ROW{index:06d}'.encode())
                    self.assertNotIn(p.base.fixture.PILOT_LIVE_MARKER.encode(), new)
                else:self.assertEqual(old,new)
        for record in range(1,64):
            for index in [16,17]:self.assertEqual(p.base.fixture.line('snapshot',index,record),p.base.fixture.line('snapshot',index,record,True))
    def test_legacy_stream_digests_are_unchanged_from_official_artifact(self):
        for mode,count,expected in [
            ('raw',100000,'de9494ca792b9ecd5fc549eb389ed40f10d603ebe6f6e144c740fe00e5cbb7b9'),
            ('snapshot',2000,'94e67b8da15568af3a95c56552287fab1e58d1bc6f94384ac2ab53049e24ac8b')]:
            h=hashlib.sha256()
            for index in range(count):h.update(p.base.fixture.line(mode,index,0))
            self.assertEqual(h.hexdigest(),expected)

    def test_manifest_rejects_cross_variant(self):
        with tempfile.TemporaryDirectory() as tmp:
            result=p.base.fixture.emit('snapshot',0,io.BytesIO(),Path(tmp)/'m.json',False,True)
            result.update(shell_pid=42,tty='/dev/test',columns=100)
            p.base.validate_manifest(result,'snapshot',0,'42:/dev/test',True)
            with self.assertRaises(RuntimeError):p.base.validate_manifest(result,'snapshot',0,'42:/dev/test')
    def test_original_native_gate_and_ocr_unchanged(self):
        source=Path(p.base.native.__file__).read_text()
        self.assertIn('def words(self, label, psm=11, capture=True):',source)
        pilot=Path(p.__file__).read_text()
        self.assertNotIn('psm=',pilot);self.assertNotIn('.replace(',pilot)
        self.assertIn("png.exists() or tsv.exists()",pilot)
        self.assertIn("self.click(x + width // 2, y + height // 2)",pilot)

if __name__=='__main__':unittest.main()
