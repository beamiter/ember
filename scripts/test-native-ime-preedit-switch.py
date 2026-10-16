"""Pure fail-closed sequencing tests. No display, bus, socket, or subprocess."""
import importlib.util
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
        def snapshot(view,label,baseline,present):
            events.append(label)
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
                with patch.object(m,'transition',side_effect=transition):
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

if __name__=='__main__':unittest.main()
