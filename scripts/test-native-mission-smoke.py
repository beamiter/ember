#!/usr/bin/env python3
"""Pure negative target-selection contracts; these are not native acceptance."""
import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location('mission', Path(__file__).with_name('native-mission-smoke.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class Targets(unittest.TestCase):
    def tree(self):
        root = m.probe.ROOT
        def node(suffix, parent, **kwargs):
            value = dict(path=root + suffix, parent=parent, owner=':1.2', pid=123,
                         children=[], name='', role=23, states=[(1 << 25) | (1 << 30), 0],
                         interfaces=[m.public.COMPONENT], screen_extents=[0, 0, 1200, 850])
            value.update(kwargs)
            return value
        nodes = [node('', None), node('/window', root),
                 node('/mission', root + '/window', name=m.TITLE, screen_extents=[100, 100, 700, 700]),
                 node('/freeze', root + '/mission', name=m.FREEZE, role=43,
                      interfaces=[m.public.COMPONENT, m.public.ACTION], actions=[['click', '', '']],
                      states=[sum(1 << bit for bit in (8, 24, 25, 30)), 0], screen_extents=[110, 500, 200, 24]),
                 node('/label', root + '/mission', name='Title', role=29, screen_extents=[110, 150, 50, 15]),
                 node('/editor', root + '/mission', role=79, text='fixture',
                      interfaces=[m.public.COMPONENT, m.public.TEXT],
                      states=[sum(1 << bit for bit in (7, 8, 12, 24, 25, 30)), 0],
                      screen_extents=[110, 170, 200, 25]),
                 node('/titlebar', root + '/mission', role=67, screen_extents=[125, 100, 650, 30])]
        by_path = {node['path']: node for node in nodes}
        for value in nodes:
            if value['parent']:
                by_path[value['parent']]['children'].append(value['path'])
        return dict(root=root, owner=':1.2', pid=123, nodes=nodes)

    def test_exact_owned_control_and_label(self):
        self.assertEqual(m.button(self.tree(), m.FREEZE)['name'], m.FREEZE)
        self.assertEqual(m.editor(self.tree(), 'Title')['text'], 'fixture')
        self.assertEqual(m.visible_label(self.tree(), 'Title')['name'], 'Title')

    def test_reason_text_requires_visible_owned_label(self):
        for key, value in [('role', 43), ('name', 'Other reason'),
                           ('screen_extents', [900, 500, 200, 24]), ('states', [1 << 25, 0])]:
            tree = self.tree()
            tree['nodes'][4][key] = value
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                m.visible_label(tree, 'Title')

    def test_duplicate_reason_text_is_rejected(self):
        tree = self.tree()
        node = copy.deepcopy(tree['nodes'][4])
        node['path'] += '-duplicate'
        tree['nodes'][2]['children'].append(node['path'])
        tree['nodes'].append(node)
        with self.assertRaises(RuntimeError):
            m.visible_label(tree, 'Title')

    def test_disabled_is_only_admitted_for_negative_click(self):
        tree = self.tree()
        tree['nodes'][3]['states'] = [(1 << 25) | (1 << 30), 0]
        with self.assertRaises(RuntimeError):
            m.button(tree, m.FREEZE)
        self.assertEqual(m.button(tree, m.FREEZE, enabled=False)['name'], m.FREEZE)
        with self.assertRaises(RuntimeError):
            m.button(self.tree(), m.FREEZE, enabled=False)

    def test_button_wrong_identity_role_geometry_or_state_fails(self):
        for key, value in [('owner', ':1.99'), ('pid', 99), ('name', 'Freeze previous candidates'),
                           ('role', 29), ('screen_extents', [900, 500, 200, 24]),
                           ('states', [1 << 8, 0]), ('states', [-1, 0]), ('actions', [])]:
            tree = self.tree()
            tree['nodes'][3][key] = value
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                m.button(tree, m.FREEZE)

    def test_duplicate_button_or_editor_fails(self):
        for index, operation in [(3, lambda tree: m.button(tree, m.FREEZE)),
                                 (5, lambda tree: m.editor(tree, 'Title'))]:
            tree = self.tree()
            node = copy.deepcopy(tree['nodes'][index])
            node['path'] += '-duplicate'
            tree['nodes'][2]['children'].append(node['path'])
            tree['nodes'].append(node)
            with self.assertRaises(RuntimeError):
                operation(tree)

    def test_editor_requires_exact_label_and_nearby_visible_editable_input(self):
        for key, value in [('role', 29), ('screen_extents', [110, 300, 200, 25]),
                           ('screen_extents', [200, 170, 200, 25]), ('states', [1 << 25, 0])]:
            tree = self.tree()
            tree['nodes'][5][key] = value
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                m.editor(tree, 'Title')
        with self.assertRaises(RuntimeError):
            m.editor(self.tree(), 'Goal')

    def test_frame_outside_owned_window_fails(self):
        tree = self.tree()
        tree['nodes'][2]['screen_extents'] = [100, 100, 1400, 900]
        with self.assertRaises(RuntimeError):
            m.button(tree, m.FREEZE)

    def test_offscreen_button_and_title_bar_occlusion_cannot_be_clicked(self):
        for bounds in ([110, 810, 200, 24], [110, 105, 200, 24], [110, 780, 200, 24]):
            tree = self.tree()
            tree['nodes'][3]['screen_extents'] = bounds
            with self.subTest(bounds=bounds), self.assertRaises(RuntimeError):
                m.button(tree, m.FREEZE)

    def test_scroll_uses_owned_gutter_then_requires_fresh_visible_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            smoke = Mock(out=Path(directory))
            driver = m.Driver(smoke)
            clipped = self.tree()
            clipped['nodes'][3]['screen_extents'] = [110, 810, 200, 24]
            visible = self.tree()
            driver.read = Mock(side_effect=[clipped, visible])
            with patch.object(m.time, 'sleep'):
                self.assertEqual(driver.locate(m.FREEZE, (43, 62)), visible)
            smoke.run.assert_called_once_with(['xdotool', 'mousemove', '788', '461',
                                               'click', '--repeat', '3', '--delay', '60', '5'])

    def test_missing_exact_target_is_not_a_license_to_scroll_or_click(self):
        with tempfile.TemporaryDirectory() as directory:
            smoke = Mock(out=Path(directory))
            driver = m.Driver(smoke)
            driver.read = Mock(return_value=self.tree())
            with self.assertRaises(RuntimeError):
                driver.locate('Unrelated target', (43, 62))
            smoke.run.assert_not_called()

    def test_draft_readback_scrolls_back_to_the_visible_label_and_editor(self):
        with tempfile.TemporaryDirectory() as directory:
            smoke = Mock(out=Path(directory))
            driver = m.Driver(smoke)
            clipped = self.tree()
            clipped['nodes'][4]['screen_extents'] = [110, 20, 50, 15]
            clipped['nodes'][5]['screen_extents'] = [110, 40, 200, 25]
            driver.read = Mock(side_effect=[clipped, self.tree()])
            with patch.object(m.time, 'sleep'):
                value = m.editor(driver.locate('Title', (29,), below=100), 'Title')['text']
            self.assertEqual(value, 'fixture')
            self.assertEqual(smoke.run.call_args.args[0][-1], '4')

    def test_close_confirmation_requires_the_exact_owned_dialog(self):
        tree = self.tree()
        tree['nodes'][3]['name'] = 'Close anyway'
        with self.assertRaises(RuntimeError):
            m.button(tree, 'Close anyway', title='Closing this window')
        tree['nodes'][2]['name'] = 'Closing this window'
        self.assertEqual(m.button(tree, 'Close anyway', title='Closing this window')['name'], 'Close anyway')
        with self.assertRaises(RuntimeError):
            m.button(tree, 'Close anyway')

    def test_close_confirmation_is_not_admitted_when_disabled_or_outside_dialog(self):
        for key, value in [('states', [(1 << 25) | (1 << 30), 0]),
                           ('screen_extents', [900, 500, 200, 24])]:
            tree = self.tree()
            tree['nodes'][2]['name'] = 'Closing this window'
            tree['nodes'][3]['name'] = 'Close anyway'
            tree['nodes'][3][key] = value
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                m.button(tree, 'Close anyway', title='Closing this window')

    def test_no_database_means_no_success(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertIsNone(m.stored(Path(directory)))


if __name__ == '__main__':
    unittest.main()
