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
import time

from PIL import Image


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
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    for folder in ("config", "state", "data", "cache", "runtime", "project"):
        (out / folder).mkdir(mode=0o700)
    config = out / "config" / args.app
    config.mkdir()
    (config / "config.toml").write_text(
        'font_size = 20.0\ninitial_width = 1400.0\ninitial_height = 950.0\n'
        'terminal_mode = "normal"\nrestore_session = false\n'
        + ('app_renderer = "wgpu"\nui_font_size = 14.0\njsh_update_check = "never"\n' if args.app == "ember" else ''),
        encoding="utf-8",
    )
    source = out / "project" / "native-buffer.txt"
    original = "GUI_TRUECOLOR_19C3A7\nASCII -> != ==\n中文与组合字 e\u0301\nStyled_underlines\n"
    source.write_text(original, encoding="utf-8")
    state_file = out / "vim-state.json"
    vimrc = out / "fixture.vim"
    quoted = lambda value: "'" + str(value).replace("'", "''") + "'"
    vimrc.write_text(
        "set nocompatible viminfofile=NONE noswapfile nomore notermguicolors\n"
        "set runtimepath=$VIMRUNTIME packpath=\n"
        "set nowrap mouse=a laststatus=0 nonumber norelativenumber\n"
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
        "return {'supported': frame.supported, 'reason': frame.fallback_reason, 'rows': frame.rows, 'columns': frame.columns, 'leftcol': get(frame, 'leftcol', 0)}\n"
        "endfunction\n"
        "function! NativeFixtureState(timer) abort\n"
        f"call writefile([json_encode({{'termguicolors': &termguicolors, 'mode': mode(), 'windows': winnr('$'), 'screen_columns': &columns, 'screen_rows': &lines, 'line': line('.'), 'column': col('.'), 'text': getline('.'), 'modified': &modified, 'leftcol': winsaveview().leftcol, 'render': NativeFixtureRender()}})], {quoted(str(state_file) + '.tmp')})\n"
        f"call rename({quoted(str(state_file) + '.tmp')}, {quoted(state_file)})\n"
        "endfunction\n"
        "call timer_start(60, function('NativeFixtureState'), {'repeat': -1})\n",
        encoding="utf-8",
    )
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.update({
        "XDG_CONFIG_HOME": str(out / "config"), "XDG_STATE_HOME": str(out / "state"),
        "XDG_DATA_HOME": str(out / "data"), "XDG_CACHE_HOME": str(out / "cache"),
        "XDG_RUNTIME_DIR": str(out / "runtime"),
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
    def color_count(name):
        image = capture(name)
        counts = image.getcolors(image.width * image.height) or []
        return sum(count for count, color in counts if color == (0x19, 0xc3, 0xa7))
    def color_regions(name):
        image = capture(name)
        pixels = image.get_flattened_data() if hasattr(image, "get_flattened_data") else image.getdata()
        spots = [(i % image.width, i // image.width) for i, color in enumerate(pixels)
                 if color == (0x19, 0xc3, 0xa7)]
        def groups(axis):
            values = sorted({point[axis] for point in spots})
            result = []
            for value in values:
                if not result or value - result[-1][1] > 35:
                    result.append([value, value])
                else:
                    result[-1][1] = value
            return result
        xs, ys = groups(0), groups(1)
        panes = {}
        for x, y in spots:
            xi = next(i for i, (lo, hi) in enumerate(xs) if lo <= x <= hi)
            yi = next(i for i, (lo, hi) in enumerate(ys) if lo <= y <= hi)
            key = f"{xi},{yi}"
            panes[key] = panes.get(key, 0) + 1
        return {"pixels": len(spots), "x_regions": xs, "y_regions": ys, "panes": panes}
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


if __name__ == "__main__":
    main()
