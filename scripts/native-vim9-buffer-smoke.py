#!/usr/bin/env python3
"""Verify native buffer RGB pixels and real Vim input in an isolated X11 window.

Run with xvfb-run, passing --binary, --app ember|frost, and --output.
Requires Vim, xdotool, ImageMagick import and Pillow. No user config is read.
The GUI color deliberately differs from ctermfg while termguicolors is off.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

from PIL import Image, ImageChops, ImageGrab


def wait_for(probe, label, process, timeout=25):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = probe()
        if value:
            return value
        if process.poll() is not None:
            raise RuntimeError(f"{label}: application exited {process.returncode}")
        time.sleep(0.08)
    raise RuntimeError("Timed out: " + label)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--app", choices=("ember", "frost"), required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--scenario", choices=("all", "palette"), default="all",
                        help="Run the full native suite or the independent modal input gate")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    for folder in ("config", "state", "data", "cache", "runtime", "project"):
        (out / folder).mkdir(mode=0o700)
    config = out / "config" / args.app
    config.mkdir(mode=0o700)
    (config / "config.toml").write_text(
        'font_size = 20.0\ninitial_width = 1400.0\ninitial_height = 950.0\n'
        'terminal_mode = "normal"\nrestore_session = false\n'
        + ('app_renderer = "wgpu"\nui_font_size = 14.0\njsh_update_check = "never"\n' if args.app == "ember" else ''),
        encoding="utf-8",
    )
    (config / "config.toml").chmod(0o600)
    # Keep project discovery inside the fixture even when --output lives
    # below a real repository (or a sandbox's ancestor .git placeholder).
    # Search must never discover this script's own fixed no-match query.
    (out / "project" / "Makefile").write_text("", encoding="utf-8")
    source = out / "project" / "native-buffer.txt"
    original = "GUI_TRUECOLOR_19C3A7\nASCII -> != ==\n中文与组合字 e\u0301\nStyled_underlines\n"
    source.write_text(original, encoding="utf-8")
    state_file = out / "vim-state.json"
    vimrc = out / "fixture.vim"
    quoted = lambda value: "'" + str(value).replace("'", "''") + "'"
    vimrc.write_text(
        "set nocompatible viminfofile=NONE noswapfile nomore notermguicolors\n"
        "set runtimepath=$VIMRUNTIME packpath= noloadplugins\n"
        "set nowrap mouse=a virtualedit=block laststatus=0 nonumber norelativenumber\n"
        "set nospell nolist conceallevel=0\n"
        f"nnoremap <silent> <C-S> <Cmd>call writefile(['raw_ctrl_s'], {quoted(out / 'raw-control-s.txt')}, 'a')<CR>\n"
        "syntax on\n"
        "highlight Normal guifg=#ccddee guibg=#101820 ctermfg=15 ctermbg=1\n"
        "highlight NativeRgb guifg=#19c3a7 guibg=#112233 ctermfg=196 ctermbg=21 gui=bold\n"
        "highlight NativeDecoration guifg=#e9c46a guibg=#101820 guisp=#e83967 gui=italic,undercurl ctermfg=196\n"
        "augroup native_buffer_fixture\n"
        "autocmd!\n"
        "autocmd BufEnter * syntax match NativeRgb /^GUI_TRUECOLOR.*/\n"
        "autocmd BufEnter * syntax match NativeDecoration /^Styled_underlines/\n"
        "augroup END\n"
        "function! NativeFixtureRender() abort\n"
        "let scripts = getscriptinfo({'name': 'vim9-ide-bridge.vim$'})\n"
        "if empty(scripts) | return {} | endif\n"
        "let info = getscriptinfo({'sid': scripts[-1].sid})\n"
        "let encoded = get(get(info[0], 'variables', {}), 'previousRender', '')\n"
        "if empty(encoded) | return {} | endif\n"
        "let frame = json_decode(encoded)\n"
        "return {'cursor': get(frame, 'cursor', {}), 'completion': get(frame, 'completion', v:null), 'views': map(copy(get(frame, 'windows', [])), {_, w -> w.view}), 'supported': frame.supported, 'reason': frame.fallback_reason, 'rows': frame.rows, 'columns': frame.columns, 'leftcol': get(frame, 'leftcol', 0), 'native_pointer': get(get(get(frame, 'windows', []), 0, {}), 'native_pointer', 0), 'native_scroll': get(get(get(frame, 'windows', []), 0, {}), 'native_scroll', 0), 'scroll_ack': get(get(get(frame, 'windows', []), 0, {}), 'scroll_ack', 0)}\n"
        "endfunction\n"
        "function! NativeFixtureState(timer) abort\n"
        f"call writefile([json_encode({{'termguicolors': &termguicolors, 'mode': mode(), 'windows': winnr('$'), 'screen_columns': &columns, 'screen_rows': &lines, 'line': line('.'), 'column': col('.'), 'topline': line('w0'), 'text': getline('.'), 'modified': &modified, 'leftcol': winsaveview().leftcol, 'completion': complete_info(['selected', 'pum_visible']), 'render': NativeFixtureRender()}})], {quoted(str(state_file) + '.tmp')})\n"
        f"call rename({quoted(str(state_file) + '.tmp')}, {quoted(state_file)})\n"
        "endfunction\n"
        "call timer_start(60, function('NativeFixtureState'), {'repeat': -1})\n",
        encoding="utf-8",
    )
    # A test artifact directory may be longer than the Unix socket path limit.
    # Keep the real Vim transport in a private short runtime, not the user home.
    private_runtime = tempfile.TemporaryDirectory(prefix=args.app + '-vim-', dir='/tmp')
    runtime_path = Path(private_runtime.name)
    runtime_record = dict(path=str(runtime_path), path_bytes=len(os.fsencode(runtime_path)),
                          reason='private short native Vim Unix-domain socket directory')
    (out / 'native-runtime.json').write_text(json.dumps(runtime_record, indent=2) + '\n')
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.update({
        "XDG_CONFIG_HOME": str(out / "config"), "XDG_STATE_HOME": str(out / "state"),
        "XDG_DATA_HOME": str(out / "data"), "XDG_CACHE_HOME": str(out / "cache"),
        "XDG_RUNTIME_DIR": str(runtime_path),
        "VIMINIT": "execute 'source ' . fnameescape(" + quoted(vimrc) + ")",
        "WGPU_BACKEND": "gl", "LIBGL_ALWAYS_SOFTWARE": "1", "WINIT_UNIX_BACKEND": "x11",
    })
    env.pop("GVIMINIT", None)
    env.pop("JTERM_VIM9_BIN", None)
    log = (out / "application.log").open("w")
    app = subprocess.Popen([str(args.binary.resolve()), "--ide", str(source)], env=env, stdout=log, stderr=log)
    def xdo(*parts):
        result = subprocess.run(["xdotool", *map(str, parts)], env=env, text=True, capture_output=True, timeout=4)
        if result.returncode and parts[0] != "search":
            raise RuntimeError("xdotool " + " ".join(map(str, parts)) + ": " + result.stderr)
        return result
    def state():
        try:
            return json.loads(state_file.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            return {}
    def capture(name):
        image = out / f"{name}.png"
        subprocess.run(["import", "-window", str(window), str(image)], env=env, check=True, timeout=5)
        return Image.open(image).convert("RGB")
    def color_count(name, rgb=(0x19, 0xc3, 0xa7)):
        image = capture(name)
        counts = image.getcolors(image.width * image.height) or []
        return sum(count for count, color in counts if color == rgb)
    def color_regions(name):
        image = capture(name)
        pixels = image.get_flattened_data() if hasattr(image, "get_flattened_data") else image.getdata()
        spots = [(i % image.width, i // image.width) for i, color in enumerate(pixels)
                 if color == (0x19, 0xc3, 0xa7)]
        def groups(axis, gap):
            values = sorted({point[axis] for point in spots})
            result = []
            for value in values:
                if not result or value - result[-1][1] > gap:
                    result.append([value, value])
                else:
                    result[-1][1] = value
            return result
        ys = groups(1, 35)
        # Narrow native panes can put adjacent source-label ink less than35px
        # apart. One measured glyph-height separates the actual pane gutter
        # without splitting the fixed ASCII label's within-character gaps.
        glyph_height = max((hi - lo + 1 for lo, hi in ys), default=0)
        xs = groups(0, max(1, glyph_height))
        panes = {}
        for x, y in spots:
            xi = next(i for i, (lo, hi) in enumerate(xs) if lo <= x <= hi)
            yi = next(i for i, (lo, hi) in enumerate(ys) if lo <= y <= hi)
            key = f"{xi},{yi}"
            panes[key] = panes.get(key, 0) + 1
        return {"pixels": len(spots), "x_regions": xs, "y_regions": ys, "panes": panes,
                "horizontal_group_gap": glyph_height}
    def native_splits(name, horizontal_regions, vertical_regions):
        regions = color_regions(name)
        expected_panes = horizontal_regions + vertical_regions - 1
        return regions if (len(regions["x_regions"]) == horizontal_regions
                           and len(regions["y_regions"]) == vertical_regions
                           and len(regions["panes"]) == expected_panes
                           and all(count > 30 for count in regions["panes"].values())) else None
    def command(value):
        xdo("key", "Escape")
        xdo("type", "--clearmodifiers", "--delay", "8", ":" + value)
        xdo("key", "Return")
    report = {}
    try:
        windows = wait_for(lambda: xdo("search", "--onlyvisible", "--pid", app.pid).stdout.strip(), "mapped window", app)
        window = windows.splitlines()[0]
        xdo("windowfocus", "--sync", window)
        wait_for(lambda: state().get("termguicolors") == 0, "Vim notermguicolors fixture", app)
        report["native_rgb_pixels"] = wait_for(lambda: color_count("native") > 30 and color_count("native"), "GUI RGB pixels", app)
        # These are real plugin decorations, not syntax rules or ANSI colors.
        command("highlight NativePlugin guifg=#f1a234 guibg=#304050 guisp=#ee3344 gui=italic,undercurl ctermfg=196 ctermbg=21")
        plugin_rgb = (0xf1, 0xa2, 0x34)
        for kind, expression in (("regex", "matchadd('NativePlugin', 'ASCII')"),
                                 ("position", "matchaddpos('NativePlugin', [[3,1,6]])")):
            command("let g:native_plugin = " + expression)
            report["plugin_" + kind + "_rgb_pixels"] = wait_for(
                lambda: state().get("render", {}).get("supported")
                and color_count("plugin-" + kind, plugin_rgb) > 15
                and color_count("plugin-" + kind, plugin_rgb),
                "native plugin " + kind + " GUI pixels", app)
            assert source.read_text(encoding="utf-8") == original
            assert state().get("modified") == 0
            command("call matchdelete(g:native_plugin)")
            wait_for(lambda: state().get("render", {}).get("supported")
                     and color_count("plugin-" + kind + "-removed", plugin_rgb) == 0,
                     "removed plugin " + kind + " paint", app)
        # The fixture has no language service: Enter on >format must retain
        # the modal. Replacing its query and immediately confirming a real
        # split proves both unavailable ownership and latest-query dispatch.
        xdo("key", "ctrl+shift+p")
        xdo("type", "--clearmodifiers", "--delay", "0", ">format")
        xdo("key", "Return")
        time.sleep(0.15)
        capture("palette-unavailable-format")
        xdo("key", "ctrl+a")
        xdo("type", "--clearmodifiers", "--delay", "0", ">vertical split")
        xdo("key", "Return")
        wait_for(lambda: state().get("windows") == 2, "typed command palette vertical split", app)
        capture("palette-split-focus")
        assert state().get("modified") == 0
        assert source.read_text(encoding="utf-8") == original
        report["command_palette_unavailable_and_same_batch_filter"] = "passed"
        command("only")
        wait_for(lambda: state().get("windows") == 1, "return from command palette split", app)
        xdo("key", "ctrl+shift+p")
        xdo("type", "--clearmodifiers", "--delay", "0", ">save")
        xdo("key", "Escape")
        xdo("key", "g", "g", "0", "i")
        xdo("type", "--clearmodifiers", "--delay", "0", "palette-focus:")
        xdo("key", "Escape")
        wait_for(lambda: state().get("text") == "palette-focus:" + original.splitlines()[0],
                 "palette Escape returns keyboard ownership to Vim", app)
        command("undo")
        wait_for(lambda: state().get("text") == original.splitlines()[0] and state().get("modified") == 0,
                 "palette focus probe undo", app)
        report["command_palette_escape_focus"] = "passed"
        if args.scenario == "palette":
            # Cold/reopened modal input must not require a pointer click. Both
            # editor dialogs share the focus helper, but own distinct queries.
            for line, column in ((2, 3), (3, 1), (1, 1)):
                xdo("key", "ctrl+shift+g")
                xdo("type", "--clearmodifiers", "--delay", "0", f"{line}:{column}")
                xdo("key", "Return")
                wait_for(lambda: state().get("line") == line and state().get("column") == column,
                         f"keyboard-only Go to {line}:{column}", app)
                capture(f"goto-{line}-{column}")
                assert state().get("modified") == 0
                assert source.read_text(encoding="utf-8") == original
            report["goto_reopen_without_click"] = "passed"
            report["vim_state"] = state()
            command("qa")
            app.wait(timeout=10)
            assert app.returncode == 0
            report["normal_quit"] = "passed"
            (out / "result.json").write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report, indent=2))
            return
        # Trace a real wheel notch through window pixels in a disposable Vim
        # buffer. A distinctive source line supplies the visual position oracle.
        command("enew | call setline(1, map(range(1,200), {_, n -> printf('WHEEL_%03d native scroll', n)})) | setlocal nowrap scrolloff=0")
        command("syntax match NativePlugin /^WHEEL_065.*/ | nnoremap <LeftMouse> <Nop> | normal! 50Gzt")
        wait_for(lambda: state().get("topline") == 50 and state().get("render", {}).get("supported")
                 and state().get("render", {}).get("native_scroll")
                 and not state().get("render", {}).get("native_pointer"),
                 "wheel source viewport", app)
        geometry = dict(line.split("=", 1) for line in xdo("getwindowgeometry", "--shell", window).stdout.splitlines() if "=" in line)
        box = (int(geometry["X"]), int(geometry["Y"]),
               int(geometry["X"]) + int(geometry["WIDTH"]), int(geometry["Y"]) + int(geometry["HEIGHT"]))
        wheel_images = []
        def wheel_bounds(name):
            # XCB memory capture avoids ImageMagick process/PNG latency inside
            # the 100 ms gesture. Encode artifacts after sampling has finished.
            image = ImageGrab.grab(bbox=box, xdisplay=env["DISPLAY"]).convert("RGB")
            wheel_images.append((name, image))
            mask = None
            for channel, value in zip(image.split(), (0x30, 0x40, 0x50)):
                part = channel.point([255 if v == value else 0 for v in range(256)])
                mask = part if mask is None else ImageChops.multiply(mask, part)
            bounds = mask.getbbox()
            return bounds
        def wheel_y(name):
            bounds = wheel_bounds(name)
            return bounds[1] if bounds is not None else None
        def trace_notch(prefix, mode):
            # The workspace navigation now occupies x0..292. Use the actual
            # painted fixture line, never a stale whole-window coordinate.
            source_bounds = wait_for(lambda: wheel_bounds(prefix + "-pointer-source"),
                                     "native wheel pointer source pixels", app)
            pointer = [(source_bounds[0] + source_bounds[2]) // 2,
                       (source_bounds[1] + source_bounds[3]) // 2]
            xdo("mousemove", "--window", window, *pointer)
            start_y = wait_for(lambda: wheel_y(prefix + "-before"), "painted wheel source highlight", app)
            before_ack = state().get("render", {}).get("scroll_ack", 0)
            started = time.monotonic()
            xdo("click", "--delay", "0", "5")
            trace = []
            for index in range(8):
                trace.append([round(time.monotonic() - started, 4), wheel_y(prefix + "-step-" + str(index))])
                time.sleep(0.008)
            report["wheel_attempt_" + mode] = {"start_y": start_y, "trace": trace,
                                               "source_rgb_bounds": list(source_bounds), "pointer": pointer,
                                               "before_ack": before_ack, "after_state": state()}
            for name, image in wheel_images:
                image.save(out / (name + ".png"))
            wait_for(lambda: state().get("topline") == 53 and state().get("mode") == mode,
                     "one wheel notch moves exactly three Vim rows in " + mode + " mode", app)
            end_y = wheel_y(prefix + "-after")
            assert end_y < start_y, (start_y, trace, end_y)
            row_height = (start_y - end_y) / 3.0
            fractional = [y for _, y in trace if y is not None and end_y < y < start_y
                          and abs((start_y - y) / row_height - round((start_y - y) / row_height)) > 0.08]
            assert fractional, ("wheel moved only by complete Vim rows", mode, start_y, trace, end_y, row_height)
            return {"source_topline": [50, 53], "start_y": start_y,
                    "end_y": end_y, "row_height": row_height,
                    "fractional_positions": fractional, "trace": trace,
                    "virtualedit": "block", "native_pointer": False, "mode": mode,
                    "ack": [before_ack, state().get("render", {}).get("scroll_ack", 0)]}
        report["wheel_notch"] = trace_notch("wheel", "n")
        command("normal! 50Gzt")
        xdo("key", "i")
        wait_for(lambda: state().get("topline") == 50 and state().get("mode") == "i"
                 and state().get("render", {}).get("native_scroll"),
                 "Insert wheel source viewport", app)
        wait_for(lambda: wheel_y("wheel-insert-ready") == report["wheel_notch"]["start_y"],
                 "Insert wheel source is actually painted at the restored viewport", app)
        report["wheel_insert"] = trace_notch("wheel-insert", "i")
        for name, image in wheel_images:
            image.save(out / (name + ".png"))
        command("nunmap <LeftMouse>")
        command("bwipeout!")
        wait_for(lambda: state().get("text") == original.splitlines()[0] and state().get("modified") == 0,
                 "return from disposable wheel buffer", app)
        # Exercise the actual GUI caret, not the PTY's DECSCUSR drawing.
        command("let g:saved_gui_cursor = &guicursor | enew | call setline(1, ['M' . nr2char(30028) . 'e' . nr2char(769), ''])")
        command("highlight GuiTestCursor guifg=#102030 guibg=#e1a273 gui=NONE | highlight GuiTestInsert guifg=#102030 guibg=#73a2e1 gui=NONE | highlight GuiTestReplace guifg=#102030 guibg=#aa44dd gui=NONE")
        command("set guicursor=n-v:block-GuiTestCursor,i:ver25-GuiTestInsert,r:hor30-GuiTestReplace,a:blinkon0 | normal! gg0")
        def cursor_region(name, rgb):
            image = capture(name)
            pixels = image.getdata()
            spots = [(i % image.width, i // image.width) for i, color in enumerate(pixels) if color == rgb]
            if not spots:
                return None
            xs, ys = zip(*spots)
            return {"x": min(xs), "y": min(ys), "width": max(xs)-min(xs)+1,
                    "height": max(ys)-min(ys)+1, "pixels": len(spots)}
        def cursor_shape(shape):
            return state().get("render", {}).get("cursor", {}).get("appearance", {}).get("shape") == shape
        block_rgb, insert_rgb, replace_rgb = (0xe1, 0xa2, 0x73), (0x73, 0xa2, 0xe1), (0xaa, 0x44, 0xdd)
        block_cursor = wait_for(lambda: cursor_shape("block") and cursor_region("cursor-block", block_rgb), "native block cursor RGB", app)
        report["cursor_foreground_pixels"] = color_count("cursor-readable-ink", (0x10, 0x20, 0x30))
        assert report["cursor_foreground_pixels"] > 0, "solid block must repaint its glyph in the cursor foreground"
        xdo("key", "Right")
        wide = wait_for(lambda: state().get("render", {}).get("cursor", {}).get("width") == 2
                        and cursor_region("cursor-wide", block_rgb), "wide native cursor", app)
        assert wide["width"] >= block_cursor["width"] * 1.8, (block_cursor, wide)
        xdo("key", "Left", "i")
        insert_cursor = wait_for(lambda: cursor_shape("vertical") and cursor_region("cursor-insert", insert_rgb), "native insertion bar RGB", app)
        assert insert_cursor["width"] < block_cursor["width"] * 0.6, (block_cursor, insert_cursor)
        assert abs(insert_cursor["height"] - block_cursor["height"]) <= 1, (block_cursor, insert_cursor)
        xdo("key", "Escape", "R")
        replace_cursor = wait_for(lambda: cursor_shape("horizontal") and cursor_region("cursor-replace", replace_rgb), "native replacement underline RGB", app)
        assert replace_cursor["height"] <= block_cursor["height"] * 0.5, (block_cursor, replace_cursor)
        report["native_cursor"] = {"block": block_cursor, "wide": wide, "insert": insert_cursor, "replace": replace_cursor}
        command("set guicursor=a:block-GuiTestCursor-blinkwait250-blinkoff180-blinkon180")
        wait_for(lambda: cursor_shape("block") and state().get("render", {}).get("cursor", {}).get("appearance", {}).get("blinkwait") == 250,
                 "native blink policy", app)
        blink_trace = []
        for _ in range(16):
            image = ImageGrab.grab(bbox=box, xdisplay=env["DISPLAY"]).convert("RGB")
            count = sum(count for count, color in (image.getcolors(image.width*image.height) or []) if color == block_rgb)
            blink_trace.append(count)
            time.sleep(0.04)
        assert any(blink_trace) and 0 in blink_trace, ("native blink never alternated", blink_trace)
        report["native_cursor"]["blink_pixels"] = blink_trace
        command("let &guicursor = g:saved_gui_cursor | bwipeout!")
        wait_for(lambda: state().get("text") == original.splitlines()[0] and state().get("modified") == 0,
                 "return from disposable cursor buffer", app)
        # The ordinary Insert menu stays above the native buffers. Its GUI
        # colors deliberately differ from cterm, with termguicolors disabled.
        command("let g:saved_completeopt = &completeopt | let g:saved_pumheight = &pumheight | enew | call setline(1, ['GUI_TRUECOLOR_19C3A7', '', '']) | set completeopt=menuone,noinsert,noselect pumheight=20")
        command("highlight Pmenu guifg=#d4e5f6 guibg=#284763 gui=NONE ctermfg=196 ctermbg=21 | highlight PmenuSel guifg=#172635 guibg=#c28a49 gui=NONE ctermfg=15 ctermbg=1 | highlight! link PmenuMatch Pmenu | highlight! link PmenuMatchSel PmenuSel | highlight! link PmenuKind Pmenu | highlight! link PmenuKindSel PmenuSel | highlight! link PmenuExtra Pmenu | highlight! link PmenuExtraSel PmenuSel")
        command("inoremap <silent> <F5> <Cmd>call complete(col('.'), [{'word':'alpha', 'abbr':'Alpha_Complete', 'kind':'f', 'menu':'[function]'}, {'word':'bravo', 'abbr':'Bravo_Complete', 'kind':'v', 'menu':'[value]'}, {'word':'charlie', 'abbr':nr2char(20013).nr2char(25991), 'kind':'t', 'menu':'[type]'}])<CR>")
        command("normal! 2G0")
        menu_bg, selected_bg = (0x28, 0x47, 0x63), (0xc2, 0x8a, 0x49)
        def native_completion(selected):
            current = state()
            return (current if current.get("completion", {}).get("pum_visible")
                    and current.get("completion", {}).get("selected") == selected
                    and current.get("render", {}).get("supported")
                    and current.get("render", {}).get("completion") else None)
        xdo("key", "A", "F5")
        wait_for(lambda: native_completion(-1), "unselected native completion menu", app)
        report["completion_menu_gui_pixels"] = wait_for(
            lambda: color_count("completion-menu", menu_bg) > 100 and color_count("completion-menu", menu_bg),
            "native Pmenu GUI background", app)
        assert color_count("completion-foreground", (0xd4, 0xe5, 0xf6)) > 10
        assert color_count("completion-native-buffer") > 30, "completion must retain native buffer RGB"
        xdo("key", "ctrl+n")
        wait_for(lambda: native_completion(0), "Ctrl-N selects first Vim candidate", app)
        report["completion_selected_gui_pixels"] = wait_for(
            lambda: color_count("completion-selected", selected_bg) > 100 and color_count("completion-selected", selected_bg),
            "native PmenuSel GUI background", app)
        xdo("key", "ctrl+n")
        wait_for(lambda: native_completion(1), "Ctrl-N selects second Vim candidate", app)
        xdo("key", "ctrl+p")
        wait_for(lambda: native_completion(0), "Ctrl-P returns to first Vim candidate", app)
        xdo("key", "ctrl+y")
        wait_for(lambda: state().get("text") == "alpha"
                 and not state().get("completion", {}).get("pum_visible")
                 and not state().get("render", {}).get("completion"),
                 "Ctrl-Y commits the real Vim completion", app)
        command("call setline(2, '') | normal! 2G0")
        xdo("key", "A", "F5", "ctrl+n", "ctrl+e")
        wait_for(lambda: state().get("text") == ""
                 and not state().get("completion", {}).get("pum_visible")
                 and state().get("render", {}).get("supported")
                 and not state().get("render", {}).get("completion"),
                 "Ctrl-E cancels without changing Vim text", app)
        wait_for(lambda: color_count("completion-closed", menu_bg) == 0
                 and color_count("completion-selection-closed", selected_bg) == 0,
                 "completion pixels disappear after cancellation", app)
        command("set pumheight=2 | normal! 2G0")
        xdo("key", "A", "F5")
        wait_for(lambda: state().get("completion", {}).get("pum_visible")
                 and not state().get("render", {}).get("supported", True),
                 "scrolling completion menu explicitly uses compatibility", app)
        assert color_count("completion-scrolling-fallback", menu_bg) == 0
        assert color_count("completion-buffer-fallback") == 0
        xdo("key", "ctrl+e")
        command("set pumheight=20")
        wait_for(lambda: state().get("render", {}).get("supported")
                 and color_count("completion-fallback-recovered") > 30,
                 "native buffers recover after unsupported completion closes", app)
        # The menu can cover a sibling split and its separator without being
        # clipped to the source window's native buffer rectangle.
        command("vsplit | wincmd h | call setline(2, repeat(' ', max([1, winwidth(0)-8]))) | normal! 2G$")
        xdo("key", "A", "F5")
        def crossing_completion():
            current = native_completion(-1)
            if not current:
                return None
            render = current["render"]
            menu = render["completion"]["view"]
            views = sorted(render["views"], key=lambda view: view["column"])
            boundary = views[0]["column"] + views[0]["columns"]
            return menu if menu["column"] < boundary < menu["column"] + menu["columns"] else None
        report["completion_cross_split_view"] = wait_for(crossing_completion, "completion crosses the Vim split separator", app)
        report["completion_split_buffers"] = wait_for(
            lambda: native_splits("completion-cross-split-buffers", 2, 1),
            "both split buffers retain native RGB under completion", app)
        assert color_count("completion-cross-split", menu_bg) > 100
        xdo("key", "ctrl+e")
        command("only")
        # :iunmap consumes bars as mapping text; keep it a separate command.
        command("iunmap <F5>")
        command("let &completeopt = g:saved_completeopt | let &pumheight = g:saved_pumheight | bwipeout!")
        wait_for(lambda: state().get("text") == original.splitlines()[0] and state().get("modified") == 0
                 and state().get("render", {}).get("supported")
                 and not state().get("render", {}).get("completion"),
                 "return from disposable completion buffer", app)
        report["completion_navigation_confirm_cancel"] = "passed"
        # No [vim9] settings are supplied: the text must begin near the normal
        # tab bar, without an automatically opened project tree or IDE rows.
        def focused_native():
            regions = color_regions("focused-buffer-defaults")
            return regions if regions["x_regions"] and regions["y_regions"] else None
        focused = wait_for(focused_native, "native buffer repainted after cursor fixture", app)
        # The native workspace adds a navigation column, not a Vim project
        # tree. Observe its independent light header before checking the same
        # small source inset; a second IDE/tree inside the canvas still fails.
        image = capture("focused-workspace-header")
        header_y = 42 + 49 - 3
        light = [x for x in range(image.width)
                 if min(image.getpixel((x, header_y))) > 200]
        assert light and light == list(range(light[0], light[-1] + 1)), light
        canvas_left, canvas_right = light[0], light[-1] + 1
        assert canvas_left == 55 + 237, canvas_left
        # Native panel-stroke ownership may consume up to two raster pixels.
        # This does not alter the 80px source-inset bound being tested.
        assert abs(canvas_right - (image.width - 306)) <= 2, canvas_right
        assert canvas_left <= focused["x_regions"][0][0] < canvas_left + 80, focused
        assert focused["y_regions"][0][0] < 120, focused
        report["focused_buffer_defaults"] = dict(focused, canvas_header_span=[canvas_left, canvas_right],
                                                   header_observation_y=header_y)
        xdo("key", "Escape")
        xdo("type", "--clearmodifiers", "--delay", "20", "ggIsaved_native_")
        wait_for(lambda: state().get("text", "").startswith("saved_native_"), "Vim received insert text", app)
        if args.app == "ember":
            # Pause only this fixture's GUI, not Vim. Escape, native Save and
            # a later delete must share a pending OS batch. Save must neither
            # leak raw Ctrl+S nor be overtaken by that following edit.
            app.send_signal(signal.SIGSTOP)
            try:
                xdo("key", "Escape")
                xdo("key", "ctrl+s")
                xdo("key", "x")
            finally:
                app.send_signal(signal.SIGCONT)
        else:
            xdo("key", "Escape")
            wait_for(lambda: state().get("mode") == "n", "Vim finished Escape", app)
            xdo("key", "ctrl+s")
        wait_for(lambda: source.read_text(encoding="utf-8").startswith("saved_native_"), "real Vim edit and Ctrl+S", app)
        assert not (out / "raw-control-s.txt").exists(), "Native Save leaked raw Ctrl+S into Vim"
        if args.app == "ember":
            wait_for(lambda: state().get("text", "").startswith("saved_nativeGUI_") and state().get("modified") == 1, "same-batch edit follows completed Save", app)
            assert source.read_text(encoding="utf-8").startswith("saved_native_GUI_")
            report["same_batch_save_and_following_input"] = "passed"
            xdo("key", "u")
            wait_for(lambda: state().get("text", "").startswith("saved_native_GUI_"), "undo following delete", app)
        xdo("key", "u")
        wait_for(lambda: state().get("text") == original.splitlines()[0], "Vim undo restored original text", app)
        xdo("key", "ctrl+s")
        wait_for(lambda: source.read_text(encoding="utf-8") == original, "real Vim undo and save", app)
        command("vsplit")
        wait_for(lambda: state().get("windows") == 2, "Vim vertical split", app)
        report["vertical_split_rgb"] = wait_for(
            lambda: native_splits("vertical-splits", 2, 1), "native RGB in both Vim splits", app)
        command("split")
        wait_for(lambda: state().get("windows") == 3, "Vim mixed splits", app)
        report["mixed_split_rgb"] = wait_for(
            lambda: native_splits("mixed-splits", 2, 2), "native RGB in three mixed splits", app)
        # Native input still goes to Vim's active window after a real split move.
        command("wincmd l")
        xdo("type", "--clearmodifiers", "--delay", "20", "ggA_split_edit")
        wait_for(lambda: state().get("text", "").endswith("_split_edit"), "edit in right Vim split", app)
        xdo("key", "Escape")
        wait_for(lambda: state().get("mode") == "n", "right split normal mode", app)
        xdo("key", "ctrl+s")
        wait_for(lambda: source.read_text(encoding="utf-8").splitlines()[0].endswith("_split_edit"),
                 "save from right Vim split", app)
        xdo("key", "u")
        wait_for(lambda: state().get("text") == original.splitlines()[0], "undo split edit", app)
        xdo("key", "ctrl+s")
        wait_for(lambda: source.read_text(encoding="utf-8") == original, "save split undo", app)
        report["split_edit_save_undo"] = "passed"
        before_resize = (state()["screen_columns"], state()["screen_rows"])
        xdo("windowsize", window, 1100, 820)
        resized = wait_for(lambda: state() if (state().get("screen_columns"), state().get("screen_rows")) != before_resize else None,
                           "Vim acknowledged resized PTY geometry", app)
        report["window_resize"] = {"before": list(before_resize),
                                   "after": [resized["screen_columns"], resized["screen_rows"]]}
        report["resized_split_rgb"] = wait_for(
            lambda: native_splits("resized-splits", 2, 2), "native splits after window resize", app)
        # Balance through real Vim before checking zoom. Keeping the old
        # left pane width can leave a narrow right pane horizontally scrolled
        # through half a CJK character, which intentionally uses fallback.
        command("wincmd =")
        command("normal! 0")
        report["balanced_split_rgb"] = wait_for(
            lambda: native_splits("balanced-splits", 2, 2), "balanced native Vim splits", app)
        before_zoom = state()["screen_columns"]
        first_region = report["balanced_split_rgb"]
        xdo("mousemove", "--window", window,
            first_region["x_regions"][0][0] + 12, first_region["y_regions"][0][0] + 30)
        xdo("keydown", "ctrl")
        try:
            xdo("click", 4)
        finally:
            xdo("keyup", "ctrl")
        zoomed = wait_for(lambda: state() if state().get("screen_columns", before_zoom) < before_zoom else None,
                          "editor font zoom changed Vim geometry", app)
        report["font_zoom"] = {"before_columns": before_zoom, "after_columns": zoomed["screen_columns"]}
        report["zoomed_split_rgb"] = wait_for(
            lambda: native_splits("zoomed-splits", 2, 2), "native splits at enlarged font", app)
        assert source.read_text(encoding="utf-8") == original, "font zoom must not change the Vim document"
        # A real Vim horizontal scroll can cut through a CJK character.
        # Assert its explicit compatibility reason and that the same windows
        # recover after real Vim input returns the viewport to column zero.
        command("normal! gg0zl")
        clipped = wait_for(lambda: state().get("render")
                           if not state().get("render", {}).get("supported", True)
                           and "clipped or zero-width" in state().get("render", {}).get("reason", "")
                           else None, "explicit clipped-CJK compatibility reason", app)
        wait_for(lambda: color_count("clipped-cjk") == 0, "clipped CJK uses compatibility pixels", app)
        report["clipped_cjk_fallback"] = clipped
        assert state()["leftcol"] == 1
        command("normal! 0")
        report["clipped_cjk_recovery_rgb"] = wait_for(
            lambda: native_splits("clipped-cjk-recovered", 2, 2), "native RGB restored after CJK boundary scroll", app)
        assert state()["leftcol"] == 0
        # A supported split layout must still fail closed for unsupported
        # decorations in any participating window and recover afterwards.
        command("setlocal list")
        wait_for(lambda: color_count("compatible") == 0, "unsupported list view removed native RGB", app)
        report["fallback_rgb_pixels"] = 0
        command("setlocal nolist")
        report["restored_split_rgb"] = wait_for(
            lambda: native_splits("restored-splits", 2, 2), "native splits restored after fallback", app)
        command("only")
        wait_for(lambda: state().get("windows") == 1, "single Vim window", app)
        report["restored_rgb_pixels"] = wait_for(lambda: color_count("restored") > 30 and color_count("restored"), "native RGB restored", app)
        report["vim_state"] = state()
        assert report["vim_state"]["termguicolors"] == 0
        command("qa")
        app.wait(timeout=10)
        assert app.returncode == 0
        report["edit_save_undo"] = "passed"
        report["normal_quit"] = "passed"
        (out / "result.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report, indent=2))
    except BaseException:
        if app.poll() is None:
            app.send_signal(signal.SIGCONT)
            if "window" in locals():
                try:
                    color_count("failure")
                except (OSError, subprocess.SubprocessError) as error:
                    report["failure_screenshot_error"] = str(error)
        report["vim_state"] = state()
        (out / "failure-state.json").write_text(json.dumps(report, indent=2) + "\n")
        raise
    finally:
        if app.poll() is None:
            app.send_signal(signal.SIGCONT)
            app.terminate()
            try:
                app.wait(timeout=5)
            except subprocess.TimeoutExpired:
                app.kill()
                app.wait(timeout=5)
        log.close()
        private_runtime.cleanup()
        runtime_record['removed_after_cleanup'] = not runtime_path.exists()
        (out / 'native-runtime.json').write_text(json.dumps(runtime_record, indent=2) + '\n')


if __name__ == "__main__":
    main()
