"""Pure sequencing/ownership tests; no local display or helper processes."""
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('live', Path(__file__).with_name('native-ime-live-switch.py'))
live = importlib.util.module_from_spec(spec)
spec.loader.exec_module(live)


class LiveTests(unittest.TestCase):
    def test_live_segments_switch_once_and_keep_one_shell(self):
        with tempfile.TemporaryDirectory() as directory:
            events=[];lines=[];out=Path(directory);transcript=out/'sentinels'
            def command(label):
                events.append(('command',label));lines.append(label+':42:/dev/pts/0');transcript.write_text('\n'.join(lines)+'\n');return '42:/dev/pts/0'
            smoke=SimpleNamespace(app=SimpleNamespace(poll=lambda:None),transcript=transcript,command=command,
                                  switch_mode=lambda old,new:events.append(('switch',old,new)),
                                  find_clipboard_ownership=lambda ident,label:events.append(('find',ident,label)))
            def cycle(smoke,module,keyboard,visual,helper,identity,label,seat):
                events.append(('ime',label));smoke.command(label+'-submitted');return {'status':'pass'}
            with patch.object(live,'composition_cycle',side_effect=cycle):
                result=live.exercise(smoke,None,None,None,None,SimpleNamespace(poll=lambda:None))
            self.assertEqual([x for x in events if x[0]=='switch'], [('switch','normal','block'),('switch','block','chat'),('switch','chat','normal')])
            self.assertEqual([s['mode'] for s in result['segments']],list(live.MODES))
            self.assertEqual(result['sentinels'],5)
            for index,mode in enumerate(live.MODES):
                label=f'live-{index}-{mode}'
                self.assertLess(events.index(('find','42:/dev/pts/0',label)),events.index(('ime',label)))

    def test_transcript_requires_exact_order_identity_and_lf_bytes(self):
        mutations = {
            'crlf': lambda b: b.replace(b'\n',b'\r\n'),
            'cr': lambda b: b.replace(b'\n',b'\r'),
            'missing-final-lf': lambda b: b[:-1],
            'extra-empty': lambda b: b+b'\n',
            'duplicate': lambda b: b+b.splitlines(keepends=True)[-1],
            'reordered': lambda b: b''.join(reversed(b.splitlines(keepends=True))),
            'wrong-identity': lambda b: b.replace(b'42:/dev/pts/0',b'43:/dev/pts/1',1),
        }
        for name,mutate in mutations.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                transcript=Path(directory)/'sentinels';lines=[]
                def command(label):
                    lines.append(label+':42:/dev/pts/0\n');transcript.write_bytes(''.join(lines).encode());return '42:/dev/pts/0'
                smoke=SimpleNamespace(app=SimpleNamespace(poll=lambda:None),transcript=transcript,
                                      command=command,switch_mode=lambda *args:None,
                                      find_clipboard_ownership=lambda *args:None)
                def cycle(smoke,module,keyboard,visual,helper,identity,label,seat):
                    smoke.command(label+'-submitted')
                    if label=='live-3-normal':transcript.write_bytes(mutate(transcript.read_bytes()))
                    return {'status':'pass'}
                with patch.object(live,'composition_cycle',side_effect=cycle):
                    with self.assertRaisesRegex(AssertionError,'Exact sentinel transcript mismatch'):
                        live.exercise(smoke,None,None,None,None,SimpleNamespace(poll=lambda:None))

    def cycle_fixture(self,directory,fail_preedit=False,wrong_identity=False):
        out=Path(directory);events=[];capture=out/'raw';capture.mkdir();(capture/'live.bin').write_bytes(b'')
        state={'status':None};seat=SimpleNamespace(poll=lambda:None)
        class Process:
            returncode=None
            def poll(self):return state['status']
            def wait(self,timeout):events.append('helper-wait');state['status']=0;self.returncode=0
            def terminate(self):events.append('helper-terminate')
        helper=Process()
        class Smoke:
            def __init__(self):self.out=out;self.logs=[];self.processes=[];self.env={};self.app=object()
            def spawn(self,args,label):
                events.append('helper-spawn');self.control=Path(args[1]);(self.control/'connected').touch();return helper
            def raw_start(self,label):events.append('raw-start');return capture
            def raw_finish(self,path,identity,expected):
                events.append('raw-finish');self.asserted_identity=identity;assert path==capture and expected=='你'.encode()
                if wrong_identity:raise AssertionError('wrong collector shell identity')
            def command(self,label):events.append('submit');return '42:/dev/pts/0'
        smoke=Smoke()
        class Keyboard:
            def __init__(self,view):
                self.view=view;self.process=SimpleNamespace(poll=lambda:None);self.released=False;events.append('keyboard-start')
            def ready(self):events.append('keyboard-ready');return True
            def release_return(self,marker,path):
                assert marker.exists() and (path/'live.bin').read_bytes()==b''
                events.append('release-return');self.released=True;(path/'live.bin').write_bytes('你'.encode());(marker.parent/'committed').touch()
            def stop(self):events.append('keyboard-stop')
        def wait(test,label):
            for control in out.glob('*/ime-control'):
                if (control/'start').exists():(control/'preedit').touch()
            assert test(),label
        def snapshot(view,label,baseline,present):
            events.append(label)
            if present and fail_preedit:raise RuntimeError('preedit observation failed')
            return {'image':label}
        keyboard=SimpleNamespace(CompositionKeyboard=Keyboard,KEYBOARD_ARGS=['owned-barrier'])
        visual=SimpleNamespace(snapshot=snapshot,verify_restoration=lambda *args:{'verified':True})
        return smoke,SimpleNamespace(wait_for=wait),keyboard,visual,seat,events

    def test_cycle_exact_commit_cleanup_then_one_submit(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(live.time,'sleep'):
            smoke,module,keyboard,visual,seat,events=self.cycle_fixture(directory)
            result=live.composition_cycle(smoke,module,keyboard,visual,Path('/synthetic/helper'),'42:/dev/pts/0','segment',seat)
            self.assertEqual(result['status'],'pass');self.assertEqual(result['committed_hex'],'e4bda0')
            self.assertEqual(events.count('release-return'),1);self.assertEqual(events.count('submit'),1)
            self.assertLess(events.index('ime-preedit-zero-bytes'),events.index('release-return'))
            self.assertLess(events.index('helper-wait'),events.index('submit'))
            self.assertLess(events.index('keyboard-stop'),events.index('submit'))
            self.assertEqual(json.loads((Path(directory)/'segment/result.json').read_text())['status'],'pass')

    def test_preedit_failure_never_releases_return_or_submits(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(live.time,'sleep'):
            smoke,module,keyboard,visual,seat,events=self.cycle_fixture(directory,fail_preedit=True)
            with self.assertRaisesRegex(RuntimeError,'preedit'):
                live.composition_cycle(smoke,module,keyboard,visual,Path('/synthetic/helper'),'42:/dev/pts/0','segment',seat)
            self.assertNotIn('release-return',events);self.assertNotIn('submit',events)
            self.assertLess(events.index('keyboard-stop'),events.index('helper-terminate'))
            self.assertEqual(json.loads((Path(directory)/'segment/result.json').read_text())['status'],'fail')

    def test_wrong_collector_shell_prevents_explicit_submission(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(live.time,'sleep'):
            smoke,module,keyboard,visual,seat,events=self.cycle_fixture(directory,wrong_identity=True)
            with self.assertRaisesRegex(AssertionError,'identity'):
                live.composition_cycle(smoke,module,keyboard,visual,Path('/synthetic/helper'),'42:/dev/pts/0','segment',seat)
            self.assertNotIn('submit',events)

    def test_dead_persistent_seat_prevents_helper_or_input(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(live.time,'sleep'):
            smoke,module,keyboard,visual,seat,events=self.cycle_fixture(directory)
            seat.poll=lambda:1
            with self.assertRaisesRegex(AssertionError,'seat keyboard'):
                live.composition_cycle(smoke,module,keyboard,visual,Path('/synthetic/helper'),'42:/dev/pts/0','segment',seat)
            self.assertEqual(events,[])

    def test_cleanup_error_cannot_leave_pass_result(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(live.time,'sleep'):
            smoke,module,keyboard,visual,seat,events=self.cycle_fixture(directory,fail_preedit=True)
            def failed_stop(self):raise RuntimeError('keyboard cleanup failed')
            with patch.object(keyboard.CompositionKeyboard,'stop',failed_stop):
                with self.assertRaisesRegex(RuntimeError,'cleanup failed'):
                    live.composition_cycle(smoke,module,keyboard,visual,Path('/synthetic/helper'),'42:/dev/pts/0','segment',seat)
            result=json.loads((Path(directory)/'segment/result.json').read_text())
            self.assertEqual(result['status'],'fail');self.assertIn('cleanup_error',result)
            self.assertIn('helper-terminate',events)

    def test_artifact_view_shares_resources_and_rejects_reuse(self):
        with tempfile.TemporaryDirectory() as directory:
            smoke=SimpleNamespace(out=Path(directory),app=object(),env={},processes=[],logs=[])
            view=live.artifact_view(smoke,'one')
            for attr in ['app','env','processes','logs']:self.assertIs(getattr(view,attr),getattr(smoke,attr))
            self.assertNotEqual(view.out,smoke.out)
            with self.assertRaises(FileExistsError):live.artifact_view(smoke,'one')


if __name__=='__main__':unittest.main()
