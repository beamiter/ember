#!/usr/bin/env python3
"""XTest Enter -> first captured native RGB pixels (X11/Xvfb, not photons).

Uses a disposable real Vim and GUI, an isolated configuration and deliberately
non-cterm syntax colors. Captures a small sentinel region; records timing bounds
and capture overhead. No OCR, mocked paint, sleeps substituted for measurements,
or user configuration changes. Run under xvfb-run with --binary and --app.
"""
from __future__ import annotations

import argparse
import ctypes
import ctypes.util
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import time

from PIL import ImageGrab


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--app', choices=('ember', 'frost'), required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--samples', type=int, default=16)
    args = parser.parse_args()
    if not 4 <= args.samples <= 200:
        parser.error('--samples must be between 4 and 200')
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    for folder in ('config', 'state', 'data', 'cache', 'runtime', 'project'):
        (out / folder).mkdir(mode=0o700)
    config = out / 'config' / args.app
    config.mkdir()
    (config / 'config.toml').write_text(
        'font_size = 20.0\ninitial_width = 1400.0\ninitial_height = 950.0\n'
        'terminal_mode = "normal"\nrestore_session = false\n'
        + ('app_renderer = "wgpu"\nui_font_size = 14.0\njsh_update_check = "never"\n' if args.app == 'ember' else ''),
        encoding='utf-8')
    source = out / 'project' / 'wake.txt'
    original = 'GUI_WAKE_SENTINEL_19C3A7\nsource must stay unchanged\n'
    source.write_text(original, encoding='utf-8')
    fixture = out / 'fixture.vim'
    fixture.write_text(
        'set nocompatible viminfofile=NONE noswapfile nomore notermguicolors\n'
        'set runtimepath=$VIMRUNTIME packpath= noloadplugins\n'
        'set nowrap mouse=a laststatus=0 nonumber norelativenumber\n'
        'set nospell nolist conceallevel=0\nsyntax on\n'
        'highlight Normal guifg=#ccddee guibg=#101820 ctermfg=15 ctermbg=1\n'
        'highlight NativeWake guifg=#19c3a7 guibg=#112233 ctermfg=196 ctermbg=21 gui=bold\n'
        'augroup native_wake_fixture\nautocmd!\n'
        'autocmd BufEnter * syntax match NativeWake /^GUI_WAKE.*/\naugroup END\n', encoding='utf-8')
    env = os.environ.copy()
    for key in ('WAYLAND_DISPLAY', 'GVIMINIT', 'JTERM_VIM9_BIN'):
        env.pop(key, None)
    env.update({
        'XDG_CONFIG_HOME': str(out / 'config'), 'XDG_STATE_HOME': str(out / 'state'),
        'XDG_DATA_HOME': str(out / 'data'), 'XDG_CACHE_HOME': str(out / 'cache'),
        'XDG_RUNTIME_DIR': str(out / 'runtime'),
        'VIMINIT': "execute 'source ' . fnameescape('" + str(fixture).replace("'", "''") + "')",
        'WGPU_BACKEND': 'gl', 'LIBGL_ALWAYS_SOFTWARE': '1', 'WINIT_UNIX_BACKEND': 'x11',
    })
    display_name = env.get('DISPLAY')
    if not display_name:
        parser.error('Run this probe inside an isolated X11 display, e.g. xvfb-run')
    x11 = ctypes.CDLL(ctypes.util.find_library('X11') or 'libX11.so.6')
    xtst = ctypes.CDLL(ctypes.util.find_library('Xtst') or 'libXtst.so.6')
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]; x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XStringToKeysym.argtypes = [ctypes.c_char_p]; x11.XStringToKeysym.restype = ctypes.c_ulong
    x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]; x11.XKeysymToKeycode.restype = ctypes.c_uint
    x11.XFlush.argtypes = [ctypes.c_void_p]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
    xtst.XTestFakeKeyEvent.restype = ctypes.c_int
    display = x11.XOpenDisplay(display_name.encode())
    if not display:
        raise RuntimeError('XOpenDisplay failed')
    keycode = x11.XKeysymToKeycode(display, x11.XStringToKeysym(b'Return'))
    if not keycode:
        x11.XCloseDisplay(display)
        raise RuntimeError('Return has no X11 keycode')
    log = (out / 'application.log').open('w')
    app = subprocess.Popen([str(args.binary.resolve()), '--ide', str(source)], env=env, stdout=log, stderr=log)

    def xdo(*parts, search=False):
        result = subprocess.run(['xdotool', *map(str, parts)], env=env, text=True,
                                capture_output=True, timeout=5)
        if result.returncode and not search:
            raise RuntimeError(result.stderr)
        return result.stdout.strip()

    def alive():
        if app.poll() is not None:
            raise RuntimeError(f'application exited {app.returncode}')

    def enter():
        start = time.perf_counter_ns()
        if not xtst.XTestFakeKeyEvent(display, keycode, 1, 0):
            raise RuntimeError('XTest press failed')
        if not xtst.XTestFakeKeyEvent(display, keycode, 0, 0):
            raise RuntimeError('XTest release failed')
        x11.XFlush(display)
        return start, time.perf_counter_ns()

    def grab(box):
        return ImageGrab.grab(bbox=box, xdisplay=display_name).convert('RGB')

    def pixels(image, rgb):
        return sum(count for count, color in image.getcolors(image.width * image.height) or [] if color == rgb)

    try:
        deadline = time.monotonic() + 30
        window = None
        while time.monotonic() < deadline:
            alive()
            found = xdo('search', '--onlyvisible', '--pid', app.pid, search=True)
            if found:
                window = found.splitlines()[0]
                break
            time.sleep(0.05)
        if not window:
            raise RuntimeError('No mapped application window')
        xdo('windowfocus', '--sync', window)
        geometry = dict(line.split('=', 1) for line in xdo('getwindowgeometry', '--shell', window).splitlines() if '=' in line)
        left, top = int(geometry['X']), int(geometry['Y'])
        box = (left, top, left + int(geometry['WIDTH']), top + int(geometry['HEIGHT']))
        green = (0x19, 0xc3, 0xa7)
        magenta = (0xc3, 0x49, 0x83)
        deadline = time.monotonic() + 30
        while True:
            alive()
            image = grab(box)
            if pixels(image, green) > 30:
                break
            if time.monotonic() >= deadline:
                image.save(out / 'no-native-pixels.png')
                raise RuntimeError('No GUI-only sentinel color; cannot measure native paint')
            time.sleep(0.02)
        image.save(out / 'initial.png')
        positions = [(index % image.width, index // image.width)
                     for index, color in enumerate(image.getdata()) if color == green]
        roi = (left + min(x for x, _ in positions) - 2, top + min(y for _, y in positions) - 2,
               left + max(x for x, _ in positions) + 3, top + max(y for _, y in positions) + 3)
        samples = []
        for index in range(args.samples):
            target = magenta if index % 2 == 0 else green
            color = '#%02x%02x%02x' % target
            xdo('key', 'Escape')
            xdo('type', '--clearmodifiers', '--delay', '2', ':highlight NativeWake guifg=' + color)
            # Command text preparation is deliberately outside the timed region.
            time.sleep(0.01)
            if pixels(grab(roi), target) > 15:
                raise AssertionError('Target pixels already visible before the measured Enter')
            start, emitted = enter()
            absent_lower = start
            overhead = []
            deadline = time.monotonic() + 5
            while True:
                alive()
                capture_start = time.perf_counter_ns()
                image = grab(roi)
                capture_end = time.perf_counter_ns()
                overhead.append((capture_end - capture_start) / 1e6)
                count = pixels(image, target)
                if count > 30:
                    break
                absent_lower = capture_start
                if time.monotonic() >= deadline:
                    grab(box).save(out / 'failed.png')
                    raise RuntimeError('Timed out waiting for the exact new native RGB pixels')
                time.sleep(0.002)
            samples.append({'sample': index, 'target': color,
                'visible_lower_bound_ms': (absent_lower - start) / 1e6,
                'visible_upper_bound_ms': (capture_end - start) / 1e6,
                'xtest_enqueue_ms': (emitted - start) / 1e6,
                'capture_max_ms': max(overhead), 'captures': len(overhead), 'exact_pixels': count})
            assert source.read_text(encoding='utf-8') == original
        grab(box).save(out / 'final.png')
        values = sorted(sample['visible_upper_bound_ms'] for sample in samples)
        report = {'app': args.app, 'backend': 'X11 software rendering / capture',
            'metric': 'XTest Return enqueue to first captured exact native RGB pixels; upper bounds, not physical input-to-photon',
            'workload': 'real Vim GUI highlight changes while notermguicolors; source file unchanged',
            'samples': samples, 'summary': {'count': len(values), 'median_ms': statistics.median(values),
                'p95_ms': values[math.ceil(.95 * len(values)) - 1], 'max_ms': max(values)},
            'region': roi, 'source_unchanged': True}
        xdo('key', 'Escape')
        xdo('type', '--clearmodifiers', '--delay', '2', ':qa')
        enter()
        app.wait(timeout=10)
        assert app.returncode == 0, app.returncode
        report['normal_exit'] = True
        (out / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report['summary']))
    finally:
        if app.poll() is None:
            app.terminate()
            try:
                app.wait(timeout=5)
            except subprocess.TimeoutExpired:
                app.kill(); app.wait(timeout=5)
        x11.XCloseDisplay(display)
        log.close()


if __name__ == '__main__':
    main()
