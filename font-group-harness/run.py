#!/usr/bin/env python3
"""Run font-group-only tests without building eframe/wgpu or opening a window."""
from pathlib import Path
import json
import os
import subprocess
import sys

from production_wiring import (verify_files_font_group_wiring,
                               verify_workspace_font_group_wiring,
                               verify_overlay_font_zoom_order)

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent

# Pin integration edges not compiled by the deliberately small fixture.
main = (ROOT / "src/main.rs").read_text()
tabs = (ROOT / "src/app/tabs.rs").read_text()
input_source = (ROOT / "src/app/input.rs").read_text()
state = (ROOT / "src/app/state.rs").read_text()
workspace = (ROOT / "src/reference_workspace.rs").read_text()
assert "self.font_zoom\n            .collect(&mut raw_input.events, raw_input.focused);" in main
assert "self.font_zoom.begin_layout();" in main
# The native reference shell replaces the former top_bar widget. Its measured
# content excludes the local header, while the full current-frame workspace
# keeps the top/rail/navigation/inspector in the UI font group. The production
# router's real-egui tests exercise this registration and responsive geometry.
assert "let workspace_rect = root_ui.max_rect();" in main
assert ".register_workspace(workspace_rect, main_panel.inner, root_ui.layer_id());" in main
assert "self.render_reference_canvas_header(ui);" in main
assert "ui.advance_cursor_after_rect(rect);" in workspace
# The semantic guard below checks the exact renderer and both input gates,
# independently of rustfmt line wrapping.
verify_workspace_font_group_wiring(main)
# Files moves between a background dock and a modeless Area. Its measured
# response, not the old root layer, owns the current-frame UI font region.
verify_files_font_group_wiring(main, workspace)
assert "theme::configure_workspace_text(ui.style_mut(), self.config.font_size);" in main
assert "theme::configure_workspace_text(root_ui.style_mut(), self.config.effective_ui_font_size());" in main
# Every overlay must establish its current-frame input owner before zoom is
# drained. Pin the real executable tail, including Vim, CLI session deletion and
# whole-window close confirmation, without allowing comments or dead blocks to satisfy it.
verify_overlay_font_zoom_order(main)
assert "self.config.font_size" not in tabs
assert "ctrl_wheel_zoom_delta" not in input_source
assert "font_size_accumulator" not in state
assert "renderer.font_size = self.config.font_size;" in main
assert "for renderer in &mut self.pane_renderers" in main
print("PASS: production raw-input, layout, styles and renderer wiring checks", flush=True)
subprocess.run([sys.executable, str(HERE / "test_production_wiring.py")], check=True)

result = subprocess.run([sys.executable, str(HERE / "generate_config_harness.py")], check=True, capture_output=True, text=True)
manifest = json.loads(result.stdout)
(HERE / "extraction-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
print("PASS: regenerated exact production extracts", flush=True)

cargo = os.environ.get("CARGO", "cargo")
base = [cargo, "test", "--locked", "--manifest-path", str(HERE / "Cargo.toml")]
# Do not run the unrelated full config/persistence suites against the explicit
# compile-only stubs in config_harness.rs.
for args in (["--lib"], ["--test", "config_harness", "font_"], ["--test", "config_harness", "config_panel::tests::"]):
    subprocess.run(base + args, check=True)

