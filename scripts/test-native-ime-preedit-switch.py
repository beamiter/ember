"""Pure fail-closed sequencing tests. No display, bus, socket, or subprocess."""
import importlib.util
import copy
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('switch', Path(__file__).with_name('native-ime-preedit-switch.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class Tests(unittest.TestCase):
    def fixture(self, directory, inject=None, outcome='context-abandoned', cleanup_fail=False):
        out=Path(directory);events=[];capture=out/'capture';capture.mkdir();(capture/'live.bin').write_bytes(b'')
        control=out/'segment'/'ime-control';state={'exit':None}
        class Process:
            returncode=None
            def poll(self):return state['exit']
            def wait(self,timeout):events.append('wait');state['exit']=0;self.returncode=0
            def terminate(self):
                events.append('terminate')
                if cleanup_fail:raise RuntimeError('cleanup failed')
        process=Process()
        def artifact(smoke,label):
            (out/'segment').mkdir();return SimpleNamespace(out=out/'segment',spawn=spawn)
        def spawn(*args):events.append('spawn');(control/'connected').touch();return process
        def wait(test,label):
            if (control/'start').exists():(control/'preedit').touch()
            if (control/'challenge').exists():
                events.append('challenge');(control/'challenged').touch();(control/outcome).touch()
                if inject=='challenge':(capture/'live.bin').write_bytes('你'.encode())
            assert test(),label
        def switch(old,new):
            events.append('switch');assert (control/'preedit').exists()
            if inject=='switch':(capture/'live.bin').write_bytes(b'\r')
        def snapshot(view,label,baseline,present,**kwargs):
            events.append(label)
            assert kwargs == ({'health_fallback': True} if label == 'after-challenge' else {})
            if inject==label:raise RuntimeError('visual failure')
            return {'rectangle':{'id':1,'pid':42,'rect':{}},'anchor_box':{},'observed_token_count':int(present)}
        def finish(path,identity,expected):
            events.append('finish');assert state['exit']==0 and expected==b'' and identity=='42:/dev/pts/0'
            if inject=='identity':raise AssertionError('wrong collector identity')
        smoke=SimpleNamespace(app=SimpleNamespace(poll=lambda:None),raw_start=lambda label:capture,
            switch_mode=switch,raw_finish=finish)
        args=(smoke,SimpleNamespace(wait_for=wait),SimpleNamespace(artifact_view=artifact),
              SimpleNamespace(snapshot=snapshot),Path('/synthetic/observer'),'42:/dev/pts/0',
              'normal','block',SimpleNamespace(poll=lambda:None))
        return args,events,out/'segment/result.json'

    def test_both_native_outcomes_cancel_then_reap_before_drain(self):
        for outcome in ['context-abandoned','late-commit-sent']:
            with self.subTest(outcome=outcome),tempfile.TemporaryDirectory() as d,patch.object(m.time,'sleep'):
                args,events,result=self.fixture(d,outcome=outcome)
                got=m.transition(*args)
                self.assertEqual(got['native_outcome'],outcome)
                order=['active-preedit','switch','cancelled-preedit','challenge','after-challenge','wait','finish']
                self.assertEqual([x for x in events if x in order],order)
                self.assertEqual(json.loads(result.read_text())['status'],'pass')

    def test_failure_never_finishes_capture_or_reports_pass(self):
        for failure in ['active-preedit','switch','cancelled-preedit','challenge','after-challenge']:
            with self.subTest(failure=failure),tempfile.TemporaryDirectory() as d,patch.object(m.time,'sleep'):
                args,events,result=self.fixture(d,inject=failure)
                with self.assertRaises((AssertionError,RuntimeError)):m.transition(*args)
                self.assertNotIn('finish',events)
                self.assertIn('terminate',events)
                self.assertEqual(json.loads(result.read_text())['status'],'fail')
                if failure in ['active-preedit','switch','cancelled-preedit']:self.assertNotIn('challenge',events)

    def test_cleanup_error_overrides_status(self):
        with tempfile.TemporaryDirectory() as d,patch.object(m.time,'sleep'):
            args,events,result=self.fixture(d,inject='active-preedit',cleanup_fail=True)
            with self.assertRaisesRegex(RuntimeError,'cleanup failed'):m.transition(*args)
            self.assertIn('cleanup_error',json.loads(result.read_text()))

    def test_wrong_collector_identity_fails(self):
        with tempfile.TemporaryDirectory() as d,patch.object(m.time,'sleep'):
            args,events,result=self.fixture(d,inject='identity')
            with self.assertRaisesRegex(AssertionError,'identity'):m.transition(*args)
            self.assertEqual(json.loads(result.read_text())['status'],'fail')

    def test_dead_seat_never_starts_helper(self):
        with tempfile.TemporaryDirectory() as d:
            args,events,result=self.fixture(d)
            args[-1].poll=lambda:1
            with self.assertRaises(AssertionError):m.transition(*args)
            self.assertNotIn('spawn',events)

    def test_exact_transcript_and_fresh_composition_after_each_switch(self):
        for corrupt in [False,True]:
            with self.subTest(corrupt=corrupt),tempfile.TemporaryDirectory() as d:
                transcript=Path(d)/'transcript';events=[];lines=[]
                def command(label):
                    lines.append(label+':42:/dev/pts/0\n');transcript.write_bytes(''.join(lines).encode());return '42:/dev/pts/0'
                smoke=SimpleNamespace(command=command,transcript=transcript)
                def transition(*args):events.append(('switch',args[-3],args[-2]));return {'status':'pass'}
                def cycle(*args):
                    events.append(('fresh',args[-2]));command(args[-2]+'-submitted')
                    if corrupt:transcript.write_bytes(transcript.read_bytes()+b'\n')
                    return {'status':'pass'}
                live=SimpleNamespace(MODES=('normal','block','chat','normal'),composition_cycle=cycle)
                with patch.object(m,'transition',side_effect=transition), patch.object(m,'FreshVisual',return_value=None):
                    if corrupt:
                        with self.assertRaisesRegex(AssertionError,'sentinel'):m.exercise(smoke,None,live,None,None,None,None,None)
                    else:
                        result=m.exercise(smoke,None,live,None,None,None,None,None)
                        self.assertEqual(result['sentinels'],4)
                        self.assertEqual([x[0] for x in events],['switch','fresh']*3)

    def test_unavailable_roundtrip_and_success_exit_fail_closed(self):
        source=Path(__file__).with_name('native-ime-preedit-switch.c').read_text()
        self.assertIn('!s->failed && !s->unavailable && monotonic() < deadline',source)
        self.assertIn('return s->synced && !s->failed && !s->unavailable;',source)
        finish=source.split('if(s.released&&marker(&s,"finish",false)){',1)[1].split('if(!pump',1)[0]
        self.assertLess(finish.index('if(!sync_bounded(&s,deadline))goto cleanup;'),finish.index('result=0;'))

    def test_helper_has_no_keyboard_grab_and_abandons_old_context(self):
        source=Path(__file__).with_name('native-ime-preedit-switch.c').read_text()
        self.assertNotIn('zwp_input_method_v2_grab_keyboard(',source)
        self.assertIn('if(!s.deactivated && s.active)',source)
        self.assertIn('s->deactivated = true',source)
        self.assertEqual(source.count('zwp_input_method_v2_commit_string('),1)
        self.assertIn('origin_serial=%u current_serial=%u',source)



class HandoffTests(unittest.TestCase):
    def fixture(self, directory):
        fixture = m.load('visual_fixture', Path(__file__).with_name('test-native-ime-visual.py'))
        smoke = fixture.Smoke(directory)
        smoke.app.poll = lambda: None
        smoke.config = Path(directory) / 'config.toml'
        smoke.config.write_text('terminal_mode = "normal"\n')
        source = copy.copy(smoke)
        source.out = smoke.out / 'preedit-chat-to-normal'
        source.out.mkdir()
        prior = fixture.visual.snapshot(source, 'after-challenge', None, False)
        segment = {'status':'pass', 'identity':'42:/dev/pts/0', 'from':'chat', 'to':'normal',
                   'pty_bytes':0, 'after_challenge_visual':prior}
        view = copy.copy(smoke)
        view.out = smoke.out / 'fresh-after-chat-to-normal'
        view.out.mkdir()
        view.captures=[];view.word_calls=[];view.tree_calls=0
        adapter=m.FreshVisual(fixture.visual,smoke,segment,'42:/dev/pts/0','normal')
        return fixture,smoke,source,view,segment,adapter

    def test_success_preserves_current_cycle_baseline_and_single_use(self):
        with tempfile.TemporaryDirectory() as d:
            fixture,smoke,source,view,segment,adapter=self.fixture(d)
            before=adapter.snapshot(view,'ime-visual-before',None,False)
            self.assertEqual(before['health_ocr_psm'],6)
            self.assertEqual(view.captures,['ime-visual-before'])
            proof=json.loads((view.out/'ime-health-handoff.json').read_text())
            self.assertEqual(proof['source_image_sha256'],segment['after_challenge_visual']['image_sha256'])
            self.assertEqual(proof['mode'],'normal')
            with self.assertRaisesRegex(RuntimeError,'one initial'):
                adapter.snapshot(view,'ime-visual-before',None,False)
            view.rows=[view.anchor,fixture.word(fixture.visual.PREEDIT_TOKEN)]
            during=adapter.snapshot(view,'during',before,True)
            self.assertEqual(during['observed_token_count'],1)
            self.assertEqual(view.word_calls[-1][1],{'psm':6,'capture':False})

    def test_validated_baseline_allows_same_png_psm11_only_after_zero_primary(self):
        with tempfile.TemporaryDirectory() as d:
            fixture,smoke,source,view,segment,adapter=self.fixture(d)
            def words(label,**kwargs):
                view.word_calls.append((label,kwargs))
                return [fixture.word('other')] if kwargs['psm']==6 else [view.anchor]
            view.words=words
            result=adapter.snapshot(view,'ime-visual-before',None,False)
            self.assertEqual(result['health_ocr_psm'],11)
            self.assertEqual(result['anchor_box'],segment['after_challenge_visual']['anchor_box'])
            self.assertEqual(view.captures,['ime-visual-before'])
            self.assertEqual([v[1]['psm'] for v in view.word_calls],[6,11])

    def test_unverified_status_mode_identity_or_nonzero_capture_rejected(self):
        for key,value in [('status','fail'),('to','block'),('identity','99:/dev/pts/1'),('pty_bytes',3)]:
            with self.subTest(key=key),tempfile.TemporaryDirectory() as d:
                fixture,smoke,source,view,segment,adapter=self.fixture(d)
                segment[key]=value
                with self.assertRaises(RuntimeError):m.FreshVisual(fixture.visual,smoke,segment,'42:/dev/pts/0','normal')

    def test_source_hash_metadata_path_and_absence_provenance_required(self):
        for case in ['image','metadata','path','token-count','token-matches']:
            with self.subTest(case=case),tempfile.TemporaryDirectory() as d:
                fixture,smoke,source,view,segment,adapter=self.fixture(d)
                if case=='image':adapter.image.write_bytes(b'changed')
                elif case=='metadata':adapter.image.with_name('after-challenge-visual.json').write_text('{}')
                elif case=='path':adapter.prior['image']=str(smoke.out/'stale.png')
                elif case=='token-count':adapter.prior['observed_token_count']=1
                else:adapter.prior['matches']=[{}]
                with self.assertRaises(RuntimeError):adapter.snapshot(view,'ime-visual-before',None,False)
                self.assertEqual(view.captures,[])

    def test_changed_mode_pid_window_geometry_or_dead_app_rejected(self):
        for case in ['mode','pid','window','geometry','dead','view-app']:
            with self.subTest(case=case),tempfile.TemporaryDirectory() as d:
                fixture,smoke,source,view,segment,adapter=self.fixture(d)
                if case=='mode':smoke.config.write_text('terminal_mode = "block"\n')
                elif case=='pid':smoke.app.pid=43
                elif case=='window':view.node['id']=6
                elif case=='geometry':view.node['rect']['width']=401
                elif case=='dead':smoke.app.poll=lambda:1
                else:view.app=SimpleNamespace(pid=42,poll=lambda:None)
                with self.assertRaises(RuntimeError):adapter.snapshot(view,'ime-visual-before',None,False)
                self.assertFalse((view.out/'ime-health-handoff.json').exists())

    def test_missing_duplicate_anchor_or_primary_token_still_rejected(self):
        for case in ['missing','duplicate','wrong-box','token']:
            with self.subTest(case=case),tempfile.TemporaryDirectory() as d:
                fixture,smoke,source,view,segment,adapter=self.fixture(d)
                if case=='missing':view.rows=[fixture.word('other')]
                elif case=='duplicate':view.rows=[view.anchor]*2
                elif case=='wrong-box':view.rows=[dict(view.anchor,left='16')]
                else:view.rows=[view.anchor,fixture.word(fixture.visual.PREEDIT_TOKEN)]
                with self.assertRaises(RuntimeError):adapter.snapshot(view,'ime-visual-before',None,False)
                self.assertFalse((view.out/'ime-health-handoff.json').exists())

if __name__=='__main__':unittest.main()
