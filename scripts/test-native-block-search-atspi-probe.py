#!/usr/bin/env python3
"""Pure contracts only; no bus, display or app is started."""
import importlib.util
from pathlib import Path
import unittest
import copy
import json
import tempfile
from unittest.mock import Mock, patch
spec=importlib.util.spec_from_file_location('probe',Path(__file__).with_name('native-block-search-atspi-probe.py'))
p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)

class Tests(unittest.TestCase):
    def test_environment_drops_all_host_routes(self):
        keys=('HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_STATE_HOME','XDG_CACHE_HOME',
              'XDG_RUNTIME_DIR','EMBER_SMOKE_SHELL_TRACE','EMBER_SMOKE_CAPTURE','EMBER_SMOKE_RAW_ROOT')
        src={k:'/synthetic/'+k for k in keys}
        src.update(DISPLAY=':0',WAYLAND_DISPLAY='host',DBUS_SESSION_BUS_ADDRESS='host',
                   AT_SPI_BUS_ADDRESS='host',LD_PRELOAD='host',PYTHONPATH='host',
                   WGPU_BACKEND='host',GITHUB_TOKEN='host',XAUTHORITY='host',GTK_MODULES='host')
        out=p.child_environment(src)
        for k in ('DISPLAY','WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS','AT_SPI_BUS_ADDRESS',
                  'LD_PRELOAD','PYTHONPATH','GITHUB_TOKEN','XAUTHORITY','GTK_MODULES'):
            self.assertNotIn(k,out)
        self.assertEqual(out['WGPU_BACKEND'],'vulkan')
        self.assertEqual(out['PATH'],'/usr/bin:/bin')

    def test_foreign_node_rejected(self):
        self.assertEqual(p.validate_address((':1.2',p.ROOT),':1.2'),(':1.2',p.ROOT))
        for address in [(':1.3',p.ROOT),(':1.2','/other')]:
            with self.assertRaises(RuntimeError):p.validate_address(address,':1.2')

    def tree(self):
        def node(path, **kw):
            result = dict(path=path, name='', description='', role=0, states=[0, 0],
                          interfaces=[], parent=p.ROOT, children=[], owner=':1.2', pid=42)
            result.update(kw)
            return result
        children = [p.ROOT+'/'+x for x in ('All', 'Cmd', 'Out', 'editor')]
        return [node(p.ROOT, parent=None, children=children)] + [
            node(children[i], name='Search scope: '+x) for i, x in enumerate(('All','Cmd','Out'))
        ] + [node(children[3], role=79, states=[1 << 7, 0],
                  interfaces=['org.a11y.atspi.Text'], text='E03DISCOVERY')]

    def validate(self, nodes):
        return p.validate_tree(nodes, p.ROOT, ':1.2', 42)

    def test_closed_picker_diagnostic_still_requires_owned_connected_tree(self):
        nodes = self.tree()[:1]
        nodes[0]['children'] = []
        self.assertIsNone(p.validate_tree(nodes, p.ROOT, ':1.2', 42, None))
        for field, value in [('pid', 43), ('owner', ':1.3'), ('parent', 'foreign')]:
            bad = copy.deepcopy(nodes); bad[0][field] = value
            with self.assertRaises(RuntimeError):
                p.validate_tree(bad, p.ROOT, ':1.2', 42, None)
        with self.assertRaises(RuntimeError): self.validate(nodes)

    def test_exact_tree_editor_and_scope_required(self):
        nodes = self.tree()
        self.assertEqual(self.validate(nodes), p.ROOT+'/editor')
        for field, value in [('role', 0), ('states', [0, 0]), ('states', [7]),
                             ('states', [0, 1 << 7]), ('interfaces', []),
                             ('text', 'e03discovery'), ('pid', 43), ('owner', ':1.3')]:
            with self.subTest(field=field, value=value):
                bad = copy.deepcopy(nodes); bad[-1][field] = value
                with self.assertRaises(RuntimeError): self.validate(bad)
        bad = copy.deepcopy(nodes); bad[1]['name'] = ''
        with self.assertRaises(RuntimeError): self.validate(bad)

    def test_static_exact_text_is_not_editor(self):
        nodes = self.tree(); nodes[-1].update(role=0, states=[0, 0], name='Static label')
        with self.assertRaises(RuntimeError): self.validate(nodes)

    def test_missing_duplicate_disconnected_and_cyclic_edges_rejected(self):
        original = self.tree()
        variants = []
        bad = copy.deepcopy(original); bad[0]['children'].append('/missing'); variants.append(bad)
        bad = copy.deepcopy(original); bad[0]['children'].append(bad[-1]['path']); variants.append(bad)
        bad = copy.deepcopy(original); bad[0]['children'].pop(); variants.append(bad)
        bad = copy.deepcopy(original); bad[-1]['children'] = [p.ROOT]; variants.append(bad)
        bad = copy.deepcopy(original); bad[-1]['parent'] = '/wrong'; variants.append(bad)
        variants.append(original[1:]); variants.append(original+[original[-1]])
        for bad in variants:
            with self.assertRaises(RuntimeError): self.validate(bad)

    def test_validated_editor_and_hierarchy_serialized(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)/'tree.json'
            result = p.save_tree(self.tree(), p.ROOT, ':1.2', 42, out)
            saved = json.loads(out.read_text())
            self.assertEqual(saved, result)
            self.assertEqual(saved['editor_path'], p.ROOT+'/editor')
            self.assertEqual(saved['root'], p.ROOT)
            self.assertEqual(saved['nodes'][0]['children'][-1], saved['editor_path'])
            self.assertEqual(saved['picker_common_ancestor'], 'unverified')
            bad = self.tree(); bad[-1]['role'] = 0
            absent = Path(directory)/'invalid.json'
            with self.assertRaises(RuntimeError): p.save_tree(bad, p.ROOT, ':1.2', 42, absent)
            self.assertFalse(absent.exists())

    def test_actual_discovery_with_inert_dbus_preserves_edges_and_owner(self):
        nodes = {n['path']: n for n in self.tree()}
        broker = Mock()
        broker.GetNameOwner.return_value = ':1.2'
        broker.GetConnectionUnixProcessID.return_value = 42
        registry = Mock(); registry.GetChildren.return_value = [(':1.2', p.ROOT)]
        service = Mock(); service.GetAddress.return_value = 'inert-only'
        bus = Mock(); bus.get_object.side_effect = lambda owner, path: (owner, path)
        def interface(obj, name):
            if name == 'org.a11y.Bus': return service
            if name == 'org.freedesktop.DBus': return broker
            if obj[0] == 'org.a11y.atspi.Registry': return registry
            node = nodes[obj[1]]
            proxy = Mock()
            proxy.GetInterfaces.return_value = node['interfaces']
            proxy.GetRole.return_value = node['role']
            proxy.GetState.return_value = node['states']
            proxy.GetChildren.return_value = [(node['owner'], x) for x in node['children']]
            proxy.Get.side_effect = lambda iface, key, **kw: node['name'] if key == 'Name' else '' if key == 'Description' else len(node['text'])
            proxy.GetText.return_value = node.get('text', '')
            return proxy
        dbus = Mock(); dbus.SessionBus.return_value = bus
        dbus.bus.BusConnection.return_value = bus; dbus.Interface.side_effect = interface
        dbus.DBusException = type('FakeDBusError', (Exception,), {})
        with tempfile.TemporaryDirectory() as directory, patch.dict('sys.modules', dbus=dbus):
            out = Path(directory)/'tree.json'
            result = p.discovery(42, out)
            self.assertEqual(result, json.loads(out.read_text()))
            self.assertEqual(result['nodes'], list(nodes.values()))
            self.assertEqual(result['editor_path'], p.ROOT+'/editor')
        self.assertEqual(broker.GetConnectionUnixProcessID.call_count, 2)

    def test_enable_status_uses_bus_object_and_verifies_readback(self):
        for enabled in (True, False):
            with self.subTest(enabled=enabled):
                session = Mock(); obj = object(); props = Mock()
                def get_object(service, path):
                    self.assertEqual((service, path), ('org.a11y.Bus', '/org/a11y/bus'))
                    return obj
                session.get_object.side_effect = get_object
                props.Get.return_value = enabled
                dbus = Mock(); dbus.SessionBus.return_value = session
                dbus.Interface.return_value = props; dbus.Boolean.side_effect = bool
                with patch.dict('sys.modules', dbus=dbus):
                    if enabled:
                        p.enable_accessibility()
                    else:
                        with self.assertRaisesRegex(RuntimeError, 'activation failed'):
                            p.enable_accessibility()
                dbus.Interface.assert_called_once_with(obj, 'org.freedesktop.DBus.Properties')
                props.Set.assert_called_once_with('org.a11y.Status', 'ScreenReaderEnabled', True, timeout=3)
                props.Get.assert_called_once_with('org.a11y.Status', 'ScreenReaderEnabled', timeout=3)

    def test_cleanup_group_exit_race_always_closes_smoke(self):
        daemon = Mock(pid=123); daemon.poll.return_value = None
        smoke = Mock()
        with patch.object(p.os, 'killpg', side_effect=ProcessLookupError('race')):
            p.close_resources(daemon, smoke)
        smoke.close.assert_called_once_with()

    def test_cleanup_reports_both_errors_and_still_closes(self):
        daemon = Mock(pid=123); daemon.poll.return_value = None
        smoke = Mock(); smoke.close.side_effect = ValueError('smoke failed')
        with patch.object(p.os, 'killpg', side_effect=PermissionError('signal failed')):
            with self.assertRaisesRegex(RuntimeError, 'signal failed.*smoke failed'):
                p.close_resources(daemon, smoke)
        smoke.close.assert_called_once_with()

    def test_cleanup_handles_absent_exited_or_unpollable_daemon(self):
        for daemon in (None, Mock(), Mock()):
            smoke = Mock()
            if daemon is not None: daemon.poll.return_value = 0
            with patch.object(p.os, 'killpg') as kill:
                p.close_resources(daemon, smoke)
            kill.assert_not_called(); smoke.close.assert_called_once_with()
        daemon = Mock(); daemon.poll.side_effect = RuntimeError('poll failed')
        smoke = Mock()
        with self.assertRaisesRegex(RuntimeError, 'poll failed'): p.close_resources(daemon, smoke)
        smoke.close.assert_called_once_with()

    def test_no_private_controller_or_action_dispatch(self):
        source=Path(p.__file__).read_text()
        self.assertNotIn('DoAction(',source)
        self.assertNotIn('GrabFocus(',source)
        self.assertNotIn('refresh_block_search_hits',source)
        self.assertIn('start_new_session=True',source)
        self.assertIn('GetConnectionUnixProcessID',source)
        self.assertIn('len(seen) >= LIMIT',source)
        self.assertIn("smoke.key('g', ('ctrl', 'shift'))",source)

if __name__=='__main__':unittest.main()
