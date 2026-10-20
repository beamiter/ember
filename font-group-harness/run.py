#!/usr/bin/env python3
"""Run font-group-only tests without building eframe/wgpu or opening a window."""
from pathlib import Path
import json
import os
import re
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent

# Pin integration edges not compiled by the deliberately small fixture.
main = (ROOT / "src/main.rs").read_text()
tabs = (ROOT / "src/app/tabs.rs").read_text()
input_source = (ROOT / "src/app/input.rs").read_text()
state = (ROOT / "src/app/state.rs").read_text()
assert "self.font_zoom\n            .collect(&mut raw_input.events, raw_input.focused);" in main
assert "self.font_zoom.begin_layout();" in main
assert "top_bar.response.rect," in main
assert "main_panel.response.rect," in main
assert ".register(panel_rect, root_ui.layer_id(), font_zoom::FontGroup::Ui);" in main
assert "theme::configure_workspace_text(ui.style_mut(), self.config.font_size);" in main
assert "theme::configure_workspace_text(root_ui.style_mut(), self.config.effective_ui_font_size());" in main
# Every overlay must establish its current-frame input owner before zoom is
# drained. Keep the real render_ui tail pinned, including the Vim dialog that
# already existed when this harness still expected only two adjacent calls.
render_ui = main.split("    fn render_ui(", 1)[1].split("\n    }", 1)[0]
zoom_tail = ("render_floating_panels", "render_vim9_dialog", "handle_font_zoom")
for method in zoom_tail:
    assert render_ui.count(f"self.{method}(ctx);") == 1, method
assert re.search(
    r"\s*".join(re.escape(f"self.{method}(ctx);") for method in zoom_tail) + r"\s*$",
    render_ui,
), "render_ui must finish overlays and the Vim dialog before draining font zoom"
assert "self.config.font_size" not in tabs
assert "ctrl_wheel_zoom_delta" not in input_source
assert "font_size_accumulator" not in state
assert "renderer.font_size = self.config.font_size;" in main
assert "for renderer in &mut self.pane_renderers" in main
print("PASS: production raw-input, layout, styles and renderer wiring checks", flush=True)

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
