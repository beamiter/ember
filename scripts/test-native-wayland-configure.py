"""Exact protocol identity/order and bounded read-only setup barrier regressions."""
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('configure', Path(__file__).with_name('native-wayland-configure.py'))
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)
PREFIX = '''[0] -> wl_compositor@4.create_surface(new id wl_surface@26)
[1] -> xdg_wm_base@13.get_xdg_surface(new id xdg_surface@27, wl_surface@26)
[2] -> xdg_surface@27.get_toplevel(new id xdg_toplevel@28)
[3] -> xdg_toplevel@28.set_app_id("io.github.beamiter.ember")
'''
EVENTS = ['xdg_toplevel@28.configure(720, 720, array[4])',
          'xdg_surface@27.configure(11)', '-> xdg_surface@27.ack_configure(11)',
          '-> xdg_surface@27.set_window_geometry(0, 0, 720, 720)', '-> wl_surface@26.commit()']


def trace(events=EVENTS):
    return PREFIX + ''.join(f'[{i+4}] {event}\n' for i, event in enumerate(events))


class ConfigureTests(unittest.TestCase):
    def test_exact_identity_configure_ack_geometry_commit(self):
        state = c.committed_geometry(trace())
        self.assertEqual((state['serial'], state['width'], state['height']), (11, 720, 720))
        self.assertEqual((state['wl_surface'], state['xdg_surface'], state['toplevel']), ('26', '27', '28'))
        self.assertLess(state['configure_line'], state['ack_line'])
        self.assertLess(state['ack_line'], state['commit_line'])
        self.assertEqual(c.committed_geometry(trace().replace('@', '#')), state)

    def test_no_ack_no_commit_and_partial_lines_rejected(self):
        for omit in range(len(EVENTS)):
            with self.subTest(omit=omit):
                self.assertIsNone(c.committed_geometry(trace(EVENTS[:omit]+EVENTS[omit+1:])))
        self.assertIsNone(c.committed_geometry(trace().rstrip('\n')))
        self.assertIsNone(c.committed_geometry(trace(), since=5))
        self.assertIsNotNone(c.committed_geometry(trace(), since=4))

    def test_wrong_serial_surface_and_order_rejected(self):
        for index, value in [(2, EVENTS[2].replace('11', '10')),
                             (2, EVENTS[2].replace('@27', '@99')),
                             (3, EVENTS[3].replace('@27', '@99')),
                             (4, EVENTS[4].replace('@26', '@99'))]:
            events=EVENTS.copy();events[index]=value
            self.assertIsNone(c.committed_geometry(trace(events)))
        events=EVENTS.copy();events[2],events[3]=events[3],events[2]
        self.assertIsNone(c.committed_geometry(trace(events)))
        events=EVENTS.copy();events[3],events[4]=events[4],events[3]
        self.assertIsNone(c.committed_geometry(trace(events)))

    def test_new_pending_configure_invalidates_old_committed_state(self):
        for suffix in [['xdg_toplevel@28.configure(1200, 900, array[4])'],
                       ['xdg_surface@27.configure(12)'],
                       ['-> xdg_surface@27.set_window_geometry(0, 0, 1200, 900)']]:
            self.assertIsNone(c.committed_geometry(trace(EVENTS+suffix)))
        later=[e.replace('720, 720','1200, 900').replace('(11)', '(12)') for e in EVENTS]
        state=c.committed_geometry(trace(EVENTS+later))
        self.assertEqual((state['serial'],state['width'],state['height']),(12,1200,900))

    def test_old_ack_and_mismatched_geometry_do_not_satisfy_new_serial(self):
        events=EVENTS+['xdg_toplevel@28.configure(1200, 900, array[4])',
                       'xdg_surface@27.configure(12)']+EVENTS[2:]
        self.assertIsNone(c.committed_geometry(trace(events)))
        events=EVENTS.copy();events[3]=events[3].replace('720, 720','1200, 900')
        self.assertIsNone(c.committed_geometry(trace(events)))

    def test_repeated_render_commit_does_not_fake_new_handshake(self):
        self.assertEqual(c.committed_geometry(trace()), c.committed_geometry(trace(EVENTS+[EVENTS[-1]])))
        self.assertIsNone(c.committed_geometry(trace(EVENTS+[EVENTS[-1]]), since=9))

    def test_missing_and_ambiguous_app_identity(self):
        self.assertIsNone(c.committed_geometry(trace().replace('io.github.beamiter.ember','other')))
        with self.assertRaises(RuntimeError):
            c.committed_geometry(trace()+'[9] -> xdg_toplevel@88.set_app_id("io.github.beamiter.ember")\n')
        with self.assertRaises(RuntimeError):
            c.complete_lines('a'*(16*1024*1024+1))

    def test_destroy_rebind_and_app_id_mutation_fail_closed(self):
        for event in ['-> xdg_toplevel@28.destroy()', '-> xdg_surface@27.destroy()',
                      '-> wl_surface@26.destroy()', 'wl_display@1.delete_id(28)',
                      '-> xdg_toplevel@28.set_app_id("other")',
                      '-> xdg_surface@27.get_toplevel(new id xdg_toplevel@28)',
                      '-> xdg_wm_base@13.get_xdg_surface(new id xdg_surface@27, wl_surface@26)',
                      '-> wl_compositor@4.create_surface(new id wl_surface@26)']:
            with self.subTest(event=event), self.assertRaises(RuntimeError):
                c.committed_geometry(trace()+event+'\n')
        self.assertIsNotNone(c.committed_geometry(trace()+'-> wl_surface@99.destroy()\n'))

    def test_future_identity_does_not_retroactively_own_earlier_events(self):
        prefix=PREFIX.replace('[3] -> xdg_toplevel@28.set_app_id("io.github.beamiter.ember")\n','')
        late=prefix+''.join(event+'\n' for event in EVENTS)+'-> xdg_toplevel@28.set_app_id("io.github.beamiter.ember")\n'
        self.assertIsNone(c.committed_geometry(late))
        with self.assertRaises(RuntimeError):
            c.committed_geometry('-> wl_surface@26.destroy()\n'+trace())

    def test_initial_zero_size_uses_committed_positive_geometry(self):
        events=EVENTS.copy();events[0]=events[0].replace('720, 720','0, 0')
        self.assertEqual(c.committed_geometry(trace(events))['width'],720)

    def setup_smoke(self, directory):
        out=Path(directory);(out/'ember.log').write_text(trace())
        node={'app_id':'io.github.beamiter.ember','pid':42,'id':5,'focused':True,'border':'none',
              'rect':{'x':30,'y':40,'width':720,'height':720},
              'window_rect':{'x':0,'y':0,'width':720,'height':720}}
        return SimpleNamespace(out=out,env={},app=SimpleNamespace(pid=42,poll=lambda:None)), {'floating_nodes':[node]}

    def test_barrier_requires_two_stable_protocol_and_tree_samples(self):
        with tempfile.TemporaryDirectory() as directory:
            smoke,tree=self.setup_smoke(directory)
            with patch.object(c.subprocess,'run',return_value=SimpleNamespace(stdout=json.dumps(tree))) as run, patch.object(c.time,'sleep'):
                result=c.wait_committed(smoke,'test',c.time.monotonic()+2,0,True,'none',(720,720),(30,40))
                self.assertEqual(run.call_count,2)
                self.assertEqual(result['protocol']['serial'],11)
                self.assertTrue(all(call.args[0]==['swaymsg','-t','get_tree','-r'] for call in run.call_args_list))

    def test_pending_ipc_geometry_without_client_commit_times_out(self):
        with tempfile.TemporaryDirectory() as directory:
            smoke,tree=self.setup_smoke(directory)
            (smoke.out/'ember.log').write_text(trace(EVENTS[:-1]))
            with patch.object(c.subprocess,'run',return_value=SimpleNamespace(stdout=json.dumps(tree))), patch.object(c.time,'sleep'):
                with self.assertRaisesRegex(RuntimeError,'Timed out'):
                    c.wait_committed(smoke,'test',c.time.monotonic()+0.01,0,True,'none',(720,720))

    def test_wrong_owned_pid_or_child_exit_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            smoke,tree=self.setup_smoke(directory);tree['floating_nodes'][0]['pid']=43
            with patch.object(c.subprocess,'run',return_value=SimpleNamespace(stdout=json.dumps(tree))):
                with self.assertRaisesRegex(RuntimeError,'ownership'):
                    c.wait_committed(smoke,'test',c.time.monotonic()+2,0,True)
            smoke.app.poll=lambda:1
            with self.assertRaisesRegex(RuntimeError,'exited'):
                c.wait_committed(smoke,'test',c.time.monotonic()+2,0,True)

    def test_driver_has_one_resize_and_one_shared_deadline(self):
        source=Path(__file__).with_name('native-wayland-geometry-smoke.py').read_text()
        self.assertEqual(source.count('resize set width 720 px height 720 px'),1)
        self.assertEqual(source.count('deadline = time.monotonic() + 30'),1)
        self.assertLess(source.index("'floating-committed'"),source.index('resize set width'))
        self.assertLess(source.index("'resize-committed'"),source.index('move position'))


if __name__=='__main__':
    unittest.main()
