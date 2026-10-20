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
        "function! NativeFixtureState(timer) abort\n"
        f"call writefile([json_encode({{'termguicolors': &termguicolors, 'mode': mode(), 'windows': winnr('$'), 'line': line('.'), 'column': col('.'), 'text': getline('.'), 'modified': &modified}})], {quoted(state_file)})\n"
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
    def color_count(name):
        image = out / f"{name}.png"
        subprocess.run(["import", "-window", str(window), str(image)], env=env, check=True, timeout=5)
        counts = Image.open(image).convert("RGB").getcolors(1400 * 950 * 2) or []
        return sum(count for count, color in counts if color == (0x19, 0xc3, 0xa7))
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
        wait_for(lambda: state().get("windows") == 2, "Vim split", app)
        wait_for(lambda: color_count("compatible") == 0, "compatible split removed native RGB", app)
        report["fallback_rgb_pixels"] = 0
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
