#!/usr/bin/env python3
"""Positive and adversarial integration-source cases for Files font ownership."""
from pathlib import Path
import unittest
from production_wiring import verify_files_font_group_wiring, verify_workspace_font_group_wiring

ROOT = Path(__file__).resolve().parent.parent
MAIN = (ROOT / 'src/main.rs').read_text()
WORKSPACE = (ROOT / 'src/reference_workspace.rs').read_text()
REGISTRATION = '''self.font_zoom.register(
            panel_rect,
            panel_response.response.layer_id,
            font_zoom::FontGroup::Ui,
        );'''


class FilesFontWiring(unittest.TestCase):
    def rejected(self, main=MAIN, workspace=WORKSPACE):
        with self.assertRaises(AssertionError):
            verify_files_font_group_wiring(main, workspace)

    def test_actual_production_dock_and_drawer(self):
        actual = verify_files_font_group_wiring(MAIN, WORKSPACE)
        self.assertEqual(actual['layer'], 'owner response')
        self.assertEqual(actual['font_group'], 'Ui')

    def test_whitespace_and_identifier_changes_keep_same_semantics(self):
        source = MAIN.replace('panel_response', 'surface_reply').replace('panel_rect', 'surface_bounds')
        self.assertEqual(verify_files_font_group_wiring(source, WORKSPACE)['owner_response'], 'surface_reply')

    def test_parent_layer_does_not_satisfy_guard(self):
        self.rejected(MAIN.replace('panel_response.response.layer_id,\n            font_zoom::FontGroup::Ui', 'root_ui.layer_id(),\n            font_zoom::FontGroup::Ui'))

    def test_wrong_font_group_rejected(self):
        self.rejected(MAIN.replace(REGISTRATION, REGISTRATION.replace('FontGroup::Ui', 'FontGroup::Main')))

    def test_unrelated_rectangle_rejected(self):
        self.rejected(MAIN.replace('let panel_rect = panel_response.response.rect;', 'let panel_rect = root_ui.max_rect();'))

    def test_different_drawer_renderer_rejected(self):
        self.rejected(MAIN.replace('"reference-files-inspector-drawer",\n                frame,\n                &mut render_body,', '"reference-files-inspector-drawer",\n                frame,\n                &mut other_body,'))

    def test_comment_registration_is_not_executable_wiring(self):
        self.rejected(MAIN.replace(REGISTRATION, '/* ' + REGISTRATION + ' */'))

    def test_string_registration_is_not_executable_wiring(self):
        self.rejected(MAIN.replace(REGISTRATION, 'let example = r#"' + REGISTRATION + '"#;'))

    def test_dead_block_registration_is_not_top_level_wiring(self):
        self.rejected(MAIN.replace(REGISTRATION, 'if false { ' + REGISTRATION + ' }'))

    def test_duplicate_registration_rejected(self):
        self.rejected(MAIN.replace(REGISTRATION, REGISTRATION + '\n' + REGISTRATION))

    def test_wrong_drawer_order_rejected(self):
        self.rejected(workspace=WORKSPACE.replace('.order(egui::Order::Middle)', '.order(egui::Order::Background)'))

    def test_missing_drawer_owner_rejected(self):
        self.rejected(MAIN.replace('reference_workspace::show_inspector_drawer(', 'reference_workspace::other_helper('))


class WorkspaceFontWiring(unittest.TestCase):
    def rejected(self, source):
        self.assertNotEqual(source, MAIN, 'Mutation must change production source')
        with self.assertRaises(AssertionError):
            verify_workspace_font_group_wiring(source)

    def test_actual_three_mode_workspace(self):
        actual = verify_workspace_font_group_wiring(MAIN)
        self.assertEqual(actual['routes'], ('api', 'task', 'terminal'))
        self.assertEqual(actual['font_group'], 'Main')
        self.assertTrue(actual['exclusive'])

    def test_comments_and_whitespace_keep_same_semantics(self):
        source = MAIN.replace('self.render_reference_rich_canvas(ui);',
                              '/* canvas */ self . render_reference_rich_canvas ( ui ) ;')
        self.assertTrue(verify_workspace_font_group_wiring(source)['exclusive'])

    def test_ui_font_cannot_replace_main_font(self):
        self.rejected(MAIN.replace('theme::configure_workspace_text(ui.style_mut(), self.config.font_size);',
                                   'theme::configure_workspace_text(ui.style_mut(), self.config.effective_ui_font_size());'))

    def test_unclipped_content_rejected(self):
        self.rejected(MAIN.replace('ui.set_clip_rect(content.intersect(ui.clip_rect()));',
                                   'ui.set_clip_rect(ui.clip_rect());'))

    def test_wrong_api_surface_rejected(self):
        self.rejected(MAIN.replace('self.ai_chat_panel.show_in_workspace(ui, &self.config);',
                                   'self.ai_chat_panel.show_floating(ui, &self.config);'))

    def test_missing_task_render_rejected(self):
        self.rejected(MAIN.replace('.show(ui, |ui| self.render_sidebar_tasks(ui));',
                                   '.show(ui, |ui| {});'))

    def test_terminal_cannot_render_into_api_view(self):
        self.rejected(MAIN.replace('self.ai_chat_panel.show_in_workspace(ui, &self.config);',
                                   'self.ai_chat_panel.show_in_workspace(ui, &self.config); self.render_reference_rich_canvas(ui);'))

    def test_canvas_in_comment_cannot_satisfy_guard(self):
        self.rejected(MAIN.replace('self.render_reference_rich_canvas(ui);',
                                   '/* self.render_reference_rich_canvas(ui); */'))

    def test_content_must_be_registered_after_render(self):
        self.rejected(MAIN.replace('.register_workspace(workspace_rect, main_panel.inner, root_ui.layer_id());',
                                   '.register_workspace(workspace_rect, workspace_rect, root_ui.layer_id());'))


if __name__ == '__main__':
    unittest.main()
