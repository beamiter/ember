#!/usr/bin/env python3
"""Guard frontend adapter wiring; behavioral/race tests live in jterm_core."""
from pathlib import Path
root = Path(__file__).resolve().parents[1]
source = (root / "src/vim9_ide.rs").read_text()
section = source[source.index("struct RenderWake"):source.index("#[derive(Clone, Copy, Debug", source.index("struct RenderWake"))]
assert "watch_render()" in section
assert "updates.recv()" in section
assert "cancel.cancel()" in section
assert "park_timeout" not in section
assert "session.render_snapshot()" not in section
assert "thread.join" not in section
assert "RenderWake::start(&editor.session" in source
print("render wake adapter wiring passed")
