#!/usr/bin/env python3
"""Pure/inert public control contracts. No D-Bus, display, PTY or app started."""
import copy
import ast
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch
spec = importlib.util.spec_from_file_location('interaction', Path(__file__).with_name('native-block-search-atspi-interaction.py'))
p = importlib.util.module_from_spec(spec); spec.loader.exec_module(p)


class Tests(unittest.TestCase):
    def tree(self):
        root = p.probe.ROOT
        def node(suffix, parent, **kw):
            n = dict(path=root+suffix, parent=parent, children=[], owner=':1.0', pid=42,
                     name='', role=23, interfaces=[p.COMPONENT], states=[(1<<25)|(1<<30), 0], screen_extents=[0,0,100,100])
            n.update(kw); return n
        nodes = [node('',None), node('/window',root), node('/picker',root+'/window',name='Block Search')]
        for key in ('All','Cmd','Out'):
            nodes.append(node('/'+key,root+'/picker',name='Search scope: '+key,role=62,
                              interfaces=[p.COMPONENT,p.ACTION],actions=[['click','','']],
                              states=[(1<<11)|(1<<25)|(1<<30)|((1<<20) if key=='All' else 0),0]))
        nodes.append(node('/editor',root+'/picker',role=79,text='E03DISCOVERY',
                          interfaces=[p.COMPONENT,p.ACTION,p.TEXT],actions=[['click','','']],
                          states=[(1<<7)|(1<<11)|(1<<12)|(1<<25)|(1<<30),0]))
        by_path={n['path']:n for n in nodes}
        for n in nodes:
            if n['parent']:by_path[n['parent']]['children'].append(n['path'])
        return dict(root=root,owner=':1.0',pid=42,nodes=nodes,editor_path=root+'/editor')

    def test_owned_picker_focus_and_pressed_contract(self):
        found=p.controls(self.tree(),'E03DISCOVERY')
        self.assertTrue(p.selected(found,'All')); self.assertTrue(p.focus_is(found,'editor'))
        self.assertFalse(p.state(found['All'],23))
        self.assertEqual(p.click_index(found['All']),0)

    def test_reject_wrong_query_role_owner_and_geometry(self):
        for key,value in [('text','wrong'),('role',0),('owner',':1.99'),('pid',99),
                          ('screen_extents',[99,99,10,10]),('states',[1<<7,0]),('states',[-1,0])]:
            tree=self.tree();tree['nodes'][-1][key]=value
            with self.subTest(key=key), self.assertRaises(RuntimeError):p.controls(tree,'E03DISCOVERY')

    def test_scopes_in_another_window_do_not_count(self):
        tree=self.tree();nodes=tree['nodes'];scope=nodes[3]
        nodes[2]['children'].remove(scope['path']);nodes[1]['children'].append(scope['path']);scope['parent']=nodes[1]['path']
        with self.assertRaisesRegex(RuntimeError,'outside picker'):p.controls(tree,'E03DISCOVERY')

    def test_generic_find_or_duplicate_picker_rejected(self):
        tree=self.tree();tree['nodes'][2]['name']='Find'
        with self.assertRaises(RuntimeError):p.controls(tree,'E03DISCOVERY')
        tree=self.tree();tree['nodes'][1]['name']='Block Search'
        with self.assertRaises(RuntimeError):p.controls(tree,'E03DISCOVERY')

    def test_click_index_resolved_not_hardcoded(self):
        node=p.controls(self.tree(),'E03DISCOVERY')['Out']
        node['actions']=[['other','',''],['click','','']]
        self.assertEqual(p.click_index(node),1)
        for actions in ([],[['other','','']],[['click','',''],['click','','']]):
            node['actions']=actions
            with self.assertRaises(RuntimeError):p.click_index(node)

    def dbus(self, pid=42, ok=True):
        dbus=Mock();bus=Mock();broker=Mock();proxy=Mock();action=Mock();component=Mock()
        broker.GetConnectionUnixProcessID.return_value=pid
        proxy.GetAddress.return_value='inert'
        action.GetActions.return_value=[['click','','']];action.DoAction.return_value=ok
        component.GrabFocus.return_value=ok
        dbus.SessionBus.return_value=bus;dbus.bus.BusConnection.return_value=bus
        dbus.Interface.side_effect=lambda obj,name: {'org.a11y.Bus':proxy,'org.freedesktop.DBus':broker,p.ACTION:action,p.COMPONENT:component}[name]
        return dbus,broker,action,component

    def test_actual_public_calls_with_inert_dbus(self):
        for op in ('click','focus'):
            dbus,broker,action,component=self.dbus()
            with patch.dict('sys.modules',dbus=dbus):result=p.mutation(self.tree(),'E03DISCOVERY','Out',op)
            self.assertTrue(result['returned']);self.assertEqual(broker.GetConnectionUnixProcessID.call_count,2)
            if op=='click':action.DoAction.assert_called_once_with(0,timeout=3)
            else:component.GrabFocus.assert_called_once_with(timeout=3)

    def test_foreign_owner_stops_before_mutation(self):
        dbus,_,action,component=self.dbus(pid=43)
        with patch.dict('sys.modules',dbus=dbus),self.assertRaises(RuntimeError):p.mutation(self.tree(),'E03DISCOVERY','Out','click')
        action.DoAction.assert_not_called();component.GrabFocus.assert_not_called()

    def test_false_response_and_changed_actions_fail(self):
        dbus,_,action,_=self.dbus(ok=False)
        with patch.dict('sys.modules',dbus=dbus),self.assertRaisesRegex(RuntimeError,'returned false'):p.mutation(self.tree(),'E03DISCOVERY','Out','click')
        dbus,_,action,_=self.dbus();action.GetActions.return_value=[['other','','']]
        with patch.dict('sys.modules',dbus=dbus),self.assertRaisesRegex(RuntimeError,'Actions changed'):p.mutation(self.tree(),'E03DISCOVERY','Out','click')
        action.DoAction.assert_not_called()

    def test_true_action_without_state_change_is_not_pass(self):
        with tempfile.TemporaryDirectory() as td:
            proof=p.Proof(Mock(),Mock(),Path(td));tree=self.tree();found=p.controls(tree,'E03DISCOVERY')
            proof.read=Mock(return_value=(tree,found))
            with patch.object(p,'mutation',return_value={'returned':True}),patch.object(p.time,'monotonic',side_effect=[0,0,0,9]):
                with self.assertRaisesRegex(RuntimeError,'response unproven'):proof.act('Out','click',lambda f:p.selected(f,'All'),lambda f:p.selected(f,'Out'),'no-op')
            self.assertNotIn('response_verified',proof.trace[0])

    def test_already_target_noop_rejected_before_mutation(self):
        for operation in ('focus', 'click'):
            with self.subTest(operation=operation), tempfile.TemporaryDirectory() as td:
                proof=p.Proof(Mock(),Mock(),Path(td));tree=self.tree();found=p.controls(tree,'E03DISCOVERY')
                if operation == 'focus':
                    found['editor']['states'][0] &= ~(1<<12)
                    found['Out']['states'][0] |= 1<<12
                    pre=lambda f:p.focus_is(f,'editor')
                    post=lambda f:p.focus_is(f,'Out')
                else:
                    found['All']['states'][0] &= ~(1<<20)
                    found['Out']['states'][0] |= 1<<20
                    pre=lambda f:p.selected(f,'Cmd')
                    post=lambda f:p.selected(f,'Out')
                proof.read=Mock(return_value=(tree,found))
                with patch.object(p,'mutation',return_value={'returned':True}) as mutate:
                    with self.assertRaisesRegex(RuntimeError,'Fresh opposing precondition'):
                        proof.act('Out',operation,pre,post,'already-target')
                mutate.assert_not_called();self.assertEqual(proof.trace,[])

    def test_fresh_wrong_source_rejects_previously_valid_observation(self):
        with tempfile.TemporaryDirectory() as td:
            proof=p.Proof(Mock(),Mock(),Path(td));tree=self.tree();old=p.controls(tree,'E03DISCOVERY')
            self.assertTrue(p.focus_is(old,'editor'))
            fresh=copy.deepcopy(old)
            fresh['editor']['states'][0] &= ~(1<<12)
            fresh['Cmd']['states'][0] |= 1<<12
            proof.read=Mock(return_value=(tree,fresh))
            with patch.object(p,'mutation') as mutate:
                with self.assertRaisesRegex(RuntimeError,'Fresh opposing precondition'):
                    proof.act('Out','focus',lambda f:p.focus_is(f,'editor'),lambda f:p.focus_is(f,'Out'),'fresh-wrong-source')
            proof.read.assert_called_once_with('fresh-wrong-source-before')
            mutate.assert_not_called()

    def test_fresh_prestate_and_changed_poststate_required(self):
        with tempfile.TemporaryDirectory() as td:
            proof=p.Proof(Mock(),Mock(),Path(td));tree=self.tree();before=p.controls(tree,'E03DISCOVERY')
            after=copy.deepcopy(before);after['All']['states'][0] &= ~(1<<20);after['Cmd']['states'][0] |= 1<<20
            proof.read=Mock(side_effect=[(tree,before),(tree,after)])
            with patch.object(p,'mutation',return_value={'returned':True}) as mutate:
                proof.act('Cmd','click',lambda f:p.selected(f,'All'),lambda f:p.selected(f,'Cmd'),'real-transition')
            mutate.assert_called_once();self.assertTrue(proof.trace[0]['response_verified'])

    def test_equivalent_replacement_control_is_not_transition(self):
        with tempfile.TemporaryDirectory() as td:
            proof=p.Proof(Mock(),Mock(),Path(td));tree=self.tree();before=p.controls(tree,'E03DISCOVERY')
            after=copy.deepcopy(before);after['All']['states'][0] &= ~(1<<20);after['Cmd']['states'][0] |= 1<<20
            after['Cmd']['path'] += '-replacement'
            proof.read=Mock(side_effect=[(tree,before),(tree,after)])
            with patch.object(p,'mutation',return_value={'returned':True}):
                with self.assertRaisesRegex(RuntimeError,'Control identity changed'):
                    proof.act('Cmd','click',lambda f:p.selected(f,'All'),lambda f:p.selected(f,'Cmd'),'replacement')
            self.assertNotIn('response_verified',proof.trace[0])

    def run_inert_collector(self, chunks, restore_error=False):
        source=Path(p.__file__).with_name('native-block-search-pty-capture.py')
        module=ast.parse(source.read_text())
        module.body=[n for n in module.body if not isinstance(n,(ast.Import,ast.ImportFrom))]
        with tempfile.TemporaryDirectory() as td:
            out=Path(td)/'capture';fake_os=Mock();fake_time=Mock();fake_termios=Mock();fake_select=Mock()
            fake_os.environ={'EMBER_SMOKE_RAW_OUT':str(out)}
            fake_os.getpid.return_value=100;fake_os.getppid.return_value=99;fake_os.ttyname.return_value='/dev/pts/inert'
            fake_time.monotonic.return_value=0
            pending=list(chunks)
            def select(*args):
                (out/'finish').touch()
                return ([0],[],[]) if pending else ([],[],[])
            fake_select.select.side_effect=select
            fake_os.read.side_effect=lambda *args:pending.pop(0)
            if restore_error:fake_termios.tcsetattr.side_effect=RuntimeError('restore failed')
            def dumps(value,**kw):
                self.assertTrue((out/'bytes.bin').exists())
                return json.dumps(value,**kw)
            env=dict(Path=Path,json=Mock(dumps=dumps),os=fake_os,select=fake_select,
                     termios=fake_termios,tty=Mock(),time=fake_time)
            if restore_error:
                with self.assertRaisesRegex(RuntimeError,'restore failed'):exec(compile(module,str(source),'exec'),env)
            else:exec(compile(module,str(source),'exec'),env)
            result=json.loads((out/'result.json').read_text())
            self.assertEqual((out/'bytes.bin').read_bytes(),b''.join(chunks))
            self.assertEqual((out/'live.bin').read_bytes(),b''.join(chunks))
            self.assertFalse((out/'result.tmp').exists())
            fake_termios.tcsetattr.assert_called_once()
            return result

    def test_inert_collector_finish_and_final_drain(self):
        result=self.run_inert_collector([b'first',b'final'])
        self.assertEqual(result['status'],'pass');self.assertEqual(result['bytes'],10)
        self.assertEqual(result['shell_pid'],99)
        self.assertEqual(self.run_inert_collector([])['bytes'],0)

    def test_inert_collector_restore_error_is_atomic_failure(self):
        result=self.run_inert_collector([],restore_error=True)
        self.assertEqual(result['status'],'fail');self.assertEqual(result['restore_error'],'restore failed')

    def test_focus_already_editor_does_not_prove_away(self):
        found=p.controls(self.tree(),'E03DISCOVERY')
        self.assertFalse(p.focus_is(found,'Out'))
        found['Out']['states'][0] |= 1<<12
        self.assertFalse(p.focus_is(found,'Out')) # duplicate focus must fail

    def test_idle_guard_stops_before_keys_on_leak_or_completion(self):
        with tempfile.TemporaryDirectory() as td:
            smoke=Mock();proof=p.Proof(smoke,Mock(),Path(td));proof.idle=Path(td)
            (Path(td)/'live.bin').write_bytes(b'leak')
            with self.assertRaises(RuntimeError):proof.key('a',('ctrl',))
            smoke.key.assert_not_called()
            (Path(td)/'live.bin').write_bytes(b'')
            (Path(td)/'result.json').write_text('{}')
            with self.assertRaises(RuntimeError):proof.text('query')
            smoke.text.assert_not_called()

    def test_no_search_mutation_or_query_text_injection(self):
        source=Path(p.__file__).read_text()
        self.assertNotIn('SetText',source);self.assertNotIn('refresh_block_search_hits',source)
        self.assertEqual(source.count("self.key('g', ('ctrl', 'shift'))"),1)
        self.assertIn("smoke.raw_finish(idle, identity, b'')",source)
        self.assertIn("'PTYCONTROL'",source)


if __name__=='__main__':unittest.main()
