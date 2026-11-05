#!/usr/bin/env python3
"""Pure contracts only; no bus, display or app is started."""
import importlib.util
from pathlib import Path
import unittest
import copy
import json
import hashlib
import tempfile
import subprocess
from unittest.mock import Mock, patch
spec=importlib.util.spec_from_file_location('probe',Path(__file__).with_name('native-block-search-atspi-probe.py'))
p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)

class Tests(unittest.TestCase):
    def test_environment_drops_all_host_routes(self):
        keys=('HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_STATE_HOME','XDG_CACHE_HOME',
              'XDG_RUNTIME_DIR','EMBER_SMOKE_SHELL_TRACE','EMBER_SMOKE_CAPTURE','EMBER_SMOKE_RAW_ROOT','EMBER_SMOKE_SENTINELS')
        src={k:'/synthetic/'+k for k in keys}
        src.update(DISPLAY=':0',WAYLAND_DISPLAY='host',DBUS_SESSION_BUS_ADDRESS='host',
                   AT_SPI_BUS_ADDRESS='host',LD_PRELOAD='host',PYTHONPATH='host',
                   WGPU_BACKEND='host',GITHUB_TOKEN='host',XAUTHORITY='host',GTK_MODULES='host',
                   VK_ICD_FILENAMES='host',VK_DRIVER_FILES='host',WGPU_ADAPTER_NAME='host')
        out=p.child_environment(src)
        for k in ('DISPLAY','WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS','AT_SPI_BUS_ADDRESS',
                  'LD_PRELOAD','PYTHONPATH','GITHUB_TOKEN','XAUTHORITY','GTK_MODULES',
                  'VK_ICD_FILENAMES','VK_DRIVER_FILES','WGPU_ADAPTER_NAME'):
            self.assertNotIn(k,out)
        self.assertEqual(out['WGPU_BACKEND'],'vulkan')
        self.assertEqual(out['PATH'],'/usr/bin:/bin')

    def test_only_explicit_verified_installed_software_icd_is_retained(self):
        payload = b'unit fixture, never loaded as a driver'
        request = dict(EMBER_SMOKE_VULKAN_ICD=p.SOFTWARE_VULKAN_ICD,
                       EMBER_SMOKE_VULKAN_ICD_SHA256=hashlib.sha256(payload).hexdigest())
        with patch.object(p.Path, 'resolve', return_value=Path(p.SOFTWARE_VULKAN_ICD)), \
                patch.object(p.Path, 'read_bytes', return_value=payload):
            self.assertEqual(p.task_software_vulkan_environment(request),
                dict(VK_DRIVER_FILES=p.SOFTWARE_VULKAN_ICD, VK_ICD_FILENAMES=p.SOFTWARE_VULKAN_ICD))
            bad = dict(request, EMBER_SMOKE_VULKAN_ICD_SHA256='0' * 64)
            with self.assertRaisesRegex(RuntimeError, 'identity changed'):
                p.task_software_vulkan_environment(bad)

    def test_foreign_unpaired_or_malformed_icd_stops_before_file_read(self):
        for request in [dict(EMBER_SMOKE_VULKAN_ICD='/tmp/foreign.json',
                             EMBER_SMOKE_VULKAN_ICD_SHA256='0' * 64),
                        dict(EMBER_SMOKE_VULKAN_ICD=p.SOFTWARE_VULKAN_ICD),
                        dict(EMBER_SMOKE_VULKAN_ICD_SHA256='0' * 64),
                        dict(EMBER_SMOKE_VULKAN_ICD=p.SOFTWARE_VULKAN_ICD,
                             EMBER_SMOKE_VULKAN_ICD_SHA256='not-a-digest')]:
            with self.subTest(request=request), patch.object(p.Path, 'read_bytes') as read:
                with self.assertRaises(RuntimeError): p.task_software_vulkan_environment(request)
                read.assert_not_called()

    def test_generated_sentinel_survives_isolation_and_shell_writes_owned_file(self):
        spec=importlib.util.spec_from_file_location('isolated_desktop',Path(__file__).with_name('native-desktop-smoke.py'))
        desktop=importlib.util.module_from_spec(spec);spec.loader.exec_module(desktop)
        with tempfile.TemporaryDirectory() as directory:
            with patch.dict('os.environ',{'EMBER_SMOKE_SENTINELS':'/untrusted-host-destination'}):
                smoke=desktop.Smoke(Path('/bin/true'),'x11',Path(directory))
            smoke.env.update(GITHUB_TOKEN='must-not-pass',LD_PRELOAD='must-not-pass')
            child=p.child_environment(smoke.env)
            self.assertEqual(child['EMBER_SMOKE_SENTINELS'],str(smoke.transcript))
            self.assertNotIn('GITHUB_TOKEN',child);self.assertNotIn('LD_PRELOAD',child)
            script='tty() { printf /dev/pts/fixture; }; '+desktop.sentinel_command('isolated')+'; printf %s "$$"'
            result=subprocess.run(['bash','-c',script],env=child,check=True,capture_output=True,text=True)
            self.assertEqual(smoke.transcript.read_text(),'isolated:'+result.stdout+':/dev/pts/fixture\n')

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
        window, picker = p.ROOT+'/window', p.ROOT+'/picker'
        return [node(p.ROOT, parent=None, children=[window]),
                node(window, role=23, children=[picker]),
                node(picker, role=23, name='Block Search', parent=window, children=children)] + [
            node(children[i], parent=picker, name='Search scope: '+x) for i, x in enumerate(('All','Cmd','Out'))
        ] + [node(children[3], parent=picker, role=79, states=[1 << 7, 0],
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
        bad = copy.deepcopy(nodes); bad[3]['name'] = ''
        with self.assertRaises(RuntimeError): self.validate(bad)

    def test_empty_sidebar_entry_cannot_shadow_the_owned_picker_editor(self):
        nodes = self.tree(); nodes[-1]['text'] = ''
        sidebar = copy.deepcopy(nodes[-1]); sidebar.update(path=p.ROOT+'/sidebar', parent=p.ROOT+'/window')
        nodes[1]['children'].append(sidebar['path']); nodes.append(sidebar)
        self.assertEqual(p.validate_tree(nodes,p.ROOT,':1.2',42,''),p.ROOT+'/editor')
        # Matching an outside entry cannot excuse a missing/wrong picker entry.
        nodes[-2]['text'] = 'different'
        with self.assertRaisesRegex(RuntimeError,'exact editable Entry'):
            p.validate_tree(nodes,p.ROOT,':1.2',42,'')
        nodes[-2]['text'] = ''
        duplicate = copy.deepcopy(nodes[-2]); duplicate['path'] += '-duplicate'
        nodes[2]['children'].append(duplicate['path']); nodes.append(duplicate)
        with self.assertRaisesRegex(RuntimeError,'exact editable Entry'):
            p.validate_tree(nodes,p.ROOT,':1.2',42,'')

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
            self.assertEqual(saved['nodes'][2]['children'][-1], saved['editor_path'])
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

    def test_end_check_rejects_removed_added_reordered_or_foreign_children(self):
        nodes = self.tree(); owned = {n['path']: n for n in nodes}
        picker = nodes[2]
        for change in ('removed', 'added', 'reordered', 'foreign'):
            def read(path):
                children = [(n['owner'], x) for n in [owned[path]] for x in n['children']]
                if path == picker['path']:
                    if change == 'removed': children.pop()
                    elif change == 'added': children.append((':1.2', p.ROOT+'/new'))
                    elif change == 'reordered': children.reverse()
                    else: children[0] = (':9.9', children[0][1])
                return children
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                p.confirm_children(nodes, ':1.2', read)
        p.confirm_children(nodes, ':1.2', lambda path:
            [(':1.2', child) for child in owned[path]['children']])

    def changing_bus(self):
        # Deterministic public-bus model of the hosted 0049 order: Stop is
        # collected before status, then completion removes Stop from parent.
        nodes = self.tree(); picker = nodes[2]
        stop = copy.deepcopy(nodes[3]); stop.update(path=p.ROOT+'/stop', name='Stop search', role=43)
        latest = copy.deepcopy(stop); latest.update(path=p.ROOT+'/latest', name='Latest')
        status = copy.deepcopy(nodes[-1]); status.update(path=p.ROOT+'/status', role=29,
            name='Complete for this retained output only', text='Complete for this retained output only')
        nodes += [stop, latest, status]; picker['children'] += [stop['path'], latest['path'], status['path']]
        by_path = {node['path']: node for node in nodes}; reads = []; state = {'changed': False}
        broker = Mock(); broker.GetNameOwner.return_value = ':1.2'
        broker.GetConnectionUnixProcessID.return_value = 42
        registry = Mock(); registry.GetChildren.return_value = [(':1.2', p.ROOT)]
        service = Mock(); service.GetAddress.return_value = 'inert-only'
        bus = Mock(); bus.get_object.side_effect = lambda owner, path: (owner, path)
        def interface(obj, name):
            if name == 'org.a11y.Bus': return service
            if name == 'org.freedesktop.DBus': return broker
            if obj[0] == 'org.a11y.atspi.Registry': return registry
            node = by_path[obj[1]]
            proxy = Mock(spec=['GetInterfaces', 'GetRole', 'GetState', 'GetChildren', 'Get', 'GetText'])
            def get(method, *args, **kwargs):
                reads.append((method, node['path']))
                if method == 'GetInterfaces': return node['interfaces']
                if method == 'GetRole': return node['role']
                if method == 'GetState': return node['states']
                if method == 'GetChildren': return [(node['owner'], x) for x in node['children']]
                if method == 'Get':
                    return node['name'] if args[1] == 'Name' else '' if args[1] == 'Description' else len(node['text'])
                if method == 'GetText':
                    if node is status and not state['changed']:
                        # egui reuses Stop's auto-ID for Latest; the old Latest
                        # child disappears, exactly as hosted 0046/0049 show.
                        stop['name'] = 'Latest'
                        picker['children'].remove(latest['path']); state['changed'] = True
                    return node.get('text', '')
                raise AssertionError('Unexpected operation: '+method)
            for method in ('GetInterfaces', 'GetRole', 'GetState', 'GetChildren', 'Get', 'GetText'):
                getattr(proxy, method).side_effect = lambda *a, m=method, **kw: get(m, *a, **kw)
            return proxy
        dbus = Mock(); dbus.SessionBus.return_value = bus; dbus.bus.BusConnection.return_value = bus
        dbus.Interface.side_effect = interface; dbus.DBusException = type('FakeDBusError', (Exception,), {})
        return dbus, registry, reads, stop, latest, picker

    def test_actual_discovery_discards_proven_torn_tree_and_only_rereads(self):
        dbus, registry, reads, stop, latest, picker = self.changing_bus()
        with tempfile.TemporaryDirectory() as directory, patch.dict('sys.modules', dbus=dbus), patch.object(p.time, 'sleep'):
            out = Path(directory)/'tree.json'; result = p.discovery(42, out)
            diagnostic = json.loads(out.with_name('tree-discovery-error.json').read_text())
            self.assertEqual(registry.GetChildren.call_count, 2)
            self.assertNotIn(latest['path'], [node['path'] for node in result['nodes']])
            self.assertEqual(next(node['name'] for node in result['nodes'] if node['path'] == stop['path']), 'Latest')
            self.assertEqual(diagnostic['status'], 'unvalidated')
            self.assertEqual(diagnostic['topology_change']['path'], picker['path'])
            self.assertIn(latest['path'], diagnostic['topology_change']['before'])
            self.assertNotIn(latest['path'], diagnostic['topology_change']['after'])
            # The failed traversal is retained, not silently rewritten.
            self.assertIn('Stop search', [node['name'] for node in diagnostic['partial_nodes']])
            self.assertIn('Complete for this retained output only', [node['name'] for node in diagnostic['partial_nodes']])
            self.assertEqual(result, json.loads(out.read_text()))
        self.assertTrue(reads)
        self.assertTrue(all(method.startswith('Get') for method, path in reads))

    def test_topology_recheck_uses_original_discovery_deadline(self):
        dbus, registry, reads, stop, latest, picker = self.changing_bus()
        clock = [0.0]
        def expire(nodes, owner, read_children):
            clock[0] = 20.0
            read_children(nodes[0]['path'])
        with tempfile.TemporaryDirectory() as directory, patch.dict('sys.modules', dbus=dbus), \
                patch.object(p, 'confirm_children', side_effect=expire), \
                patch.object(p.time, 'monotonic', side_effect=lambda: clock[0]), patch.object(p.time, 'sleep'):
            out = Path(directory)/'tree.json'
            with self.assertRaisesRegex(RuntimeError, 'Bounded accessibility discovery exhausted'):
                p.discovery(42, out)
            self.assertFalse(out.exists())
            self.assertEqual(registry.GetChildren.call_count, 1)

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

    def test_unvalidated_accessibility_tree_is_diagnostic_only(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)/'tree.json'
            with self.assertRaises(RuntimeError):
                p.save_tree([], p.ROOT, ':1.0', 42, output, '')
            self.assertFalse(output.exists())
            self.assertIn('"status": "unvalidated"', output.with_name('tree-unvalidated.json').read_text())

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
