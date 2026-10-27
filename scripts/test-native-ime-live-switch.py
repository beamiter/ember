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


desktop = live.load('mode_desktop', Path(__file__).with_name('native-desktop-smoke.py'))


class RawCollectorReadinessTests(unittest.TestCase):
    def test_dead_app_is_not_ready_even_if_stale_marker_exists(self):
        for status in [0, 1, None]:
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                out=Path(directory); ready=out/'raw-pty'/'case'/'ready'
                ready.parent.mkdir(parents=True); ready.touch(); actions=[]
                smoke=SimpleNamespace(out=out, app=SimpleNamespace(poll=lambda:status),
                    text=lambda value:actions.append(value), key=lambda value:actions.append(value))
                with patch.object(desktop,'wait_for',side_effect=lambda check,label:check()):
                    if status is None:
                        self.assertEqual(desktop.Smoke.raw_start(smoke,'case'),ready.parent)
                    else:
                        with self.assertRaisesRegex(RuntimeError,'Ember exited'):
                            desktop.Smoke.raw_start(smoke,'case')
                self.assertEqual(actions,['rawcheck case','Return'])


class SettingsModeTargetTests(unittest.TestCase):
    def rows(self):
        def word(text, x, y, w, h=12):
            return dict(text=text, left=str(x), top=str(y), width=str(w), height=str(h))
        return [word('Advanced', 363, 226, 60), word('Settings', 428, 227, 50),
                word('Terminal', 363, 552, 69), word('mode', 437, 552, 32),
                word('Chat', 483, 552, 32), word('Chat', 1067, 35, 32)]

    def test_selector_is_exact_value_beside_terminal_mode_not_title(self):
        self.assertEqual(desktop.settings_mode_selector(self.rows(), 'Chat')['top'], '552')

    def test_missing_label_ambiguous_value_and_wrong_geometry_fail(self):
        import copy
        for case in ['title-only', 'duplicate', 'label-missing', 'wrong-word', 'far', 'wrong-row', 'heading-row', 'heading-duplicate', 'heading-word', 'heading-gap', 'label-column', 'label-above', 'label-below', 'duplicate-label', 'heading-reverse', 'heading-title', 'two-dialogs']:
            with self.subTest(case=case):
                rows = copy.deepcopy(self.rows())
                if case == 'title-only': rows.pop(4)
                elif case == 'duplicate': rows.append(copy.deepcopy(rows[4]))
                elif case == 'label-missing': rows[2]['text'] = 'Other'
                elif case == 'wrong-word': rows[4]['text'] = 'Chatty'
                elif case == 'far': rows[4]['left'] = '800'
                elif case == 'wrong-row': rows[4]['top'] = '580'
                elif case == 'heading-row': rows[1]['top'] = '250'
                elif case == 'heading-duplicate': rows.extend(copy.deepcopy(rows[:2]))
                elif case == 'heading-word': rows[0]['text'] = 'Advance'
                elif case == 'heading-gap': rows[1]['left'] = '500'
                elif case == 'label-column':
                    for row in rows[2:5]:row['left']=str(int(row['left'])+100)
                elif case == 'label-above': rows[2]['top'] = rows[3]['top'] = rows[4]['top'] = '210'
                elif case == 'label-below': rows[2]['top'] = rows[3]['top'] = rows[4]['top'] = '1000'
                elif case == 'duplicate-label': rows.extend(copy.deepcopy(rows[2:4]))
                elif case == 'heading-reverse': rows[0]['text'],rows[1]['text']='Settings','Advanced'
                elif case == 'heading-title': rows[1]['top']='150'
                elif case == 'two-dialogs':
                    other=copy.deepcopy(rows[:5])
                    for row in other:row['left']=str(int(row['left'])+400)
                    rows.extend(other)
                with self.assertRaises(RuntimeError): desktop.settings_mode_selector(rows, 'Chat')

    def test_unrelated_tabs_are_not_mode_field_anchors(self):
        rows=self.rows()
        rows.extend([dict(text='Advanced',left='500',top='193',width='60',height='11'),
                     dict(text='poearance',left='412',top='193',width='74',height='13'),
                     dict(text='Remote',left='609',top='193',width='48',height='10')])
        self.assertEqual(desktop.settings_mode_selector(rows,'Chat')['left'],'483')

    def test_exact_failed_artifact_geometry_selects_normal_not_title(self):
        rows=[dict(text=t,left=str(x),top=str(y),width=str(w),height=str(h)) for t,x,y,w,h in [
            ('Advanced',363,226,60,11),('Settings',428,227,50,13),
            ('Terminal',363,522,54,11),('mode',421,522,35,11),('Normal',469,522,45,11),
            ('Terminal',1060,35,54,11),('Normal',1140,35,45,11)]]
        self.assertEqual(desktop.settings_mode_selector(rows,'Normal')['top'],'522')


class LiveTests(unittest.TestCase):
    def test_live_segments_switch_once_and_keep_one_shell(self):
        with tempfile.TemporaryDirectory() as directory:
            events=[];lines=[];out=Path(directory);transcript=out/'sentinels'
            def command(label):
                events.append(('command',label));lines.append(label+':42:/dev/pts/0');transcript.write_text('\n'.join(lines)+'\n');return '42:/dev/pts/0'
            smoke=SimpleNamespace(app=SimpleNamespace(poll=lambda:None),transcript=transcript,command=command,
                                  switch_mode=lambda old,new:events.append(('switch',old,new)),
                                  find_clipboard_ownership=lambda ident,label:events.append(('find',ident,label)))
            def cycle(smoke,module,keyboard,visual,helper,identity,label,seat,**kwargs):
                events.append(('ime',label));events.append(('layout',label,kwargs));smoke.command(label+'-submitted');return {'status':'pass'}
            with patch.object(live,'composition_cycle',side_effect=cycle):
                result=live.exercise(smoke,None,None,None,None,SimpleNamespace(poll=lambda:None))
            self.assertEqual([x for x in events if x[0]=='switch'], [('switch','normal','block'),('switch','block','chat'),('switch','chat','normal')])
            self.assertEqual([s['mode'] for s in result['segments']],list(live.MODES))
            self.assertEqual(result['sentinels'],5)
            for index,mode in enumerate(live.MODES):
                label=f'live-{index}-{mode}'
                self.assertLess(events.index(('find','42:/dev/pts/0',label)),events.index(('ime',label)))
                self.assertIn(('layout',label,{'preedit_psm':11 if mode=='chat' else 6,
                                             'health_psm':11,'health_footer':True}),events)

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
                def cycle(smoke,module,keyboard,visual,helper,identity,label,seat,**kwargs):
                    smoke.command(label+'-submitted')
                    if label=='live-3-normal':transcript.write_bytes(mutate(transcript.read_bytes()))
                    return {'status':'pass'}
                with patch.object(live,'composition_cycle',side_effect=cycle):
                    with self.assertRaisesRegex(AssertionError,'Exact sentinel transcript mismatch'):
                        live.exercise(smoke,None,None,None,None,SimpleNamespace(poll=lambda:None))

    def cycle_fixture(self,directory,fail_preedit=False,wrong_identity=False,fail_after=False,helper_wait=None):
        out=Path(directory);events=[];capture=out/'raw';capture.mkdir();(capture/'live.bin').write_bytes(b'')
        state={'status':None};seat=SimpleNamespace(poll=lambda:None)
        class Process:
            returncode=None
            def poll(self):return state['status']
            def wait(self,timeout):
                events.append('helper-wait')
                if helper_wait=='error':raise RuntimeError('helper wait failed')
                state['status']=1 if helper_wait=='nonzero' else 0
                self.returncode=state['status']
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
        def snapshot(view,label,baseline,present,**kwargs):
            events.append(label)
            if kwargs.get('after_capture') is not None:
                kwargs['after_capture']()
                events.append('post-commit-ocr')
                if fail_after:raise RuntimeError('post-commit visual analysis failed')
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
            self.assertLess(events.index('helper-wait'),events.index('raw-finish'))
            self.assertLess(events.index('raw-finish'),events.index('post-commit-ocr'))
            self.assertLess(events.index('post-commit-ocr'),events.index('submit'))
            self.assertLess(events.index('keyboard-stop'),events.index('submit'))
            self.assertEqual(json.loads((Path(directory)/'segment/result.json').read_text())['status'],'pass')

    def test_post_commit_analysis_failure_never_submits_after_draining(self):
        with tempfile.TemporaryDirectory() as directory,patch.object(live.time,'sleep'):
            smoke,module,keyboard,visual,seat,events=self.cycle_fixture(directory,fail_after=True)
            with self.assertRaisesRegex(RuntimeError,'post-commit visual'):
                live.composition_cycle(smoke,module,keyboard,visual,Path('/synthetic/helper'),'42:/dev/pts/0','segment',seat)
            self.assertIn('raw-finish',events)
            self.assertNotIn('submit',events)
            self.assertEqual(json.loads((Path(directory)/'segment/result.json').read_text())['status'],'fail')

    def test_failed_helper_finalization_prevents_drain_ocr_and_submit(self):
        for failure in ['error','nonzero']:
            with self.subTest(failure=failure),tempfile.TemporaryDirectory() as directory,patch.object(live.time,'sleep'):
                smoke,module,keyboard,visual,seat,events=self.cycle_fixture(directory,helper_wait=failure)
                with self.assertRaises((RuntimeError,AssertionError)):
                    live.composition_cycle(smoke,module,keyboard,visual,Path('/synthetic/helper'),'42:/dev/pts/0','segment',seat)
                for event in ['raw-finish','post-commit-ocr','submit']:self.assertNotIn(event,events)
                self.assertEqual(json.loads((Path(directory)/'segment/result.json').read_text())['status'],'fail')

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
            self.assertNotIn('post-commit-ocr',events)
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
