#!/usr/bin/env python3
"""Exercise real Ember/Vim input and native buttons on an isolated X server.

Requires Xvfb, libX11, libXtst, ImageMagick import, Tesseract and Vim 9. The
fixture uses private XDG paths and synthetic files. It captures the mapped
client window, reads rendered labels with OCR, and sends only native XTest
keyboard/mouse input. The file, build output, modified-close refusal, clean
exit and untouched shell-layout file are independent acceptance oracles.
Screenshots, OCR and logs remain in --output on success or failure. WGPU uses
GLES by default for software Xvfb environments; WGPU_BACKEND can override it.
"""
import argparse
import csv
import ctypes as C
import io
import json
import os
from pathlib import Path
import select
import subprocess
import time


def wait_for(probe, label, timeout=20):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = probe()
        if value:
            return value
        time.sleep(0.08)
    raise RuntimeError("Timed out: " + label)


class WindowAttributes(C.Structure):
    _fields_ = [("x", C.c_int), ("y", C.c_int), ("width", C.c_int), ("height", C.c_int),
                ("border_width", C.c_int), ("depth", C.c_int), ("visual", C.c_void_p), ("root", C.c_ulong),
                ("window_class", C.c_int), ("bit_gravity", C.c_int), ("win_gravity", C.c_int), ("backing_store", C.c_int),
                ("backing_planes", C.c_ulong), ("backing_pixel", C.c_ulong), ("save_under", C.c_int), ("colormap", C.c_ulong),
                ("map_installed", C.c_int), ("map_state", C.c_int), ("all_event_masks", C.c_long),
                ("your_event_mask", C.c_long), ("do_not_propagate_mask", C.c_long), ("override_redirect", C.c_int), ("screen", C.c_void_p)]


class Input:
    def __init__(self, display):
        self.x = C.CDLL("libX11.so.6")
        self.t = C.CDLL("libXtst.so.6")
        self.x.XOpenDisplay.argtypes = [C.c_char_p]
        self.x.XOpenDisplay.restype = C.c_void_p
        self.d = self.x.XOpenDisplay(display.encode())
        assert self.d, "X display could not open"
        self.x.XDefaultRootWindow.argtypes = [C.c_void_p]
        self.x.XDefaultRootWindow.restype = C.c_ulong
        self.root = self.x.XDefaultRootWindow(self.d)
        self.x.XQueryTree.argtypes = [C.c_void_p, C.c_ulong, C.POINTER(C.c_ulong), C.POINTER(C.c_ulong), C.POINTER(C.POINTER(C.c_ulong)), C.POINTER(C.c_uint)]
        self.x.XGetWindowAttributes.argtypes = [C.c_void_p, C.c_ulong, C.POINTER(WindowAttributes)]
        self.x.XFetchName.argtypes = [C.c_void_p, C.c_ulong, C.POINTER(C.c_char_p)]
        self.x.XFree.argtypes = [C.c_void_p]
        self.x.XSetInputFocus.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_ulong]
        self.x.XKeysymToKeycode.argtypes = [C.c_void_p, C.c_ulong]
        self.x.XKeysymToKeycode.restype = C.c_uint
        self.x.XkbKeycodeToKeysym.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_int]
        self.x.XkbKeycodeToKeysym.restype = C.c_ulong
        self.x.XSync.argtypes = [C.c_void_p, C.c_int]
        self.x.XCloseDisplay.argtypes = [C.c_void_p]
        self.t.XTestFakeKeyEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
        self.t.XTestFakeMotionEvent.argtypes = [C.c_void_p, C.c_int, C.c_int, C.c_int, C.c_ulong]
        self.t.XTestFakeButtonEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]

    def window(self):
        root, parent, count = C.c_ulong(), C.c_ulong(), C.c_uint()
        children = C.POINTER(C.c_ulong)()
        self.x.XQueryTree(self.d, self.root, C.byref(root), C.byref(parent), C.byref(children), C.byref(count))
        found = []
        try:
            for i in range(count.value):
                title = C.c_char_p()
                if self.x.XFetchName(self.d, children[i], C.byref(title)) and title.value:
                    attributes = WindowAttributes()
                    if b"Ember" in title.value and self.x.XGetWindowAttributes(self.d, children[i], C.byref(attributes)) and attributes.map_state == 2:
                        found.append(children[i])
                    self.x.XFree(title)
        finally:
            if children:
                self.x.XFree(children)
        return found[0] if len(found) == 1 else None

    def click(self, x, y):
        self.t.XTestFakeMotionEvent(self.d, -1, int(x), int(y), 0)
        self.t.XTestFakeButtonEvent(self.d, 1, True, 0)
        self.t.XTestFakeButtonEvent(self.d, 1, False, 0)
        self.x.XSync(self.d, False)
        time.sleep(0.2)

    def key(self, symbol):
        code = self.x.XKeysymToKeycode(self.d, symbol)
        assert code, f"No X keycode for {symbol}"
        shifted = self.x.XkbKeycodeToKeysym(self.d, code, 0, 0) != symbol
        shift = self.x.XKeysymToKeycode(self.d, 0xFFE1)
        if shifted:
            self.t.XTestFakeKeyEvent(self.d, shift, True, 0)
        self.t.XTestFakeKeyEvent(self.d, code, True, 0)
        self.t.XTestFakeKeyEvent(self.d, code, False, 0)
        if shifted:
            self.t.XTestFakeKeyEvent(self.d, shift, False, 0)
        self.x.XSync(self.d, False)
        time.sleep(0.025)

    def text(self, value):
        for char in value:
            self.key(ord(char))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    config = out / "config" / "ember"
    config.mkdir(parents=True)
    (config / "config.toml").write_text('shell = "/bin/bash"\nterminal_mode = "normal"\napp_renderer = "wgpu"\nfont_size = 16.0\nui_font_size = 15.0\ninitial_width = 1500.0\ninitial_height = 950.0\nrestore_session = true\njsh_update_check = "never"\n')
    original_layout = '{"version": 4, "sessions": []}\n'
    (config / "session_history.json").write_text(original_layout)
    project = out / "project"
    project.mkdir()
    (project / "Cargo.toml").write_text('[package]\nname = "native-smoke"\nversion = "0.1.0"\n')
    source = project / "main.rs"
    source.write_text("original-native-text\n")
    runtime = out / "runtime"
    runtime.mkdir(mode=0o700)
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.update(XDG_CONFIG_HOME=str(config.parent), XDG_STATE_HOME=str(out / "state"), XDG_DATA_HOME=str(out / "data"), XDG_CACHE_HOME=str(out / "cache"), XDG_RUNTIME_DIR=str(runtime), LIBGL_ALWAYS_SOFTWARE="1", WGPU_BACKEND=env.get("WGPU_BACKEND", "gl"), VIMINIT=r"set nocompatible viminfofile=NONE noswapfile nomore | set makeprg=printf\ native-build-output")
    logs, processes, xinput = [], [], None
    result = {"status": "fail"}

    def spawn(argv, label, **options):
        log = (out / (label + ".log")).open("w")
        logs.append(log)
        process = subprocess.Popen(argv, env=env, stdout=log, stderr=log, **options)
        processes.append(process)
        return process

    def capture(label):
        path = out / (label + ".png")
        subprocess.run(["import", "-window", str(xinput.window() or "root"), str(path)], env=env, check=True, timeout=10)
        return path

    def words(label):
        path = capture(label)
        text = subprocess.run(["tesseract", str(path), "stdout", "--psm", "11", "tsv"], env=env, check=True, capture_output=True, text=True, timeout=15).stdout
        (out / (label + ".tsv")).write_text(text)
        return [row for row in csv.DictReader(io.StringIO(text), delimiter="\t") if row["text"].strip()]

    def click_word(label, word, rightmost=False):
        rows = [row for row in words(label) if row["text"] == word]
        assert rows, (word, label)
        row = sorted(rows, key=lambda r: int(r["left"]), reverse=rightmost)[0]
        xinput.click(int(row["left"]) + int(row["width"]) / 2, int(row["top"]) + int(row["height"]) / 2)

    try:
        read_fd, write_fd = os.pipe()
        server = spawn(["Xvfb", "-screen", "0", "1700x1100x24", "-nolisten", "tcp", "-displayfd", str(write_fd)], "xvfb", pass_fds=(write_fd,))
        os.close(write_fd)
        assert select.select([read_fd], [], [], 10)[0], "Xvfb startup timed out"
        display = os.read(read_fd, 40).decode().strip()
        os.close(read_fd)
        assert display and server.poll() is None
        env["DISPLAY"] = ":" + display
        xinput = Input(env["DISPLAY"])
        app = spawn([str(args.binary.resolve(strict=True)), "--ide", str(source)], "ember")
        window = wait_for(xinput.window, "Ember native window")
        xinput.x.XSetInputFocus(xinput.d, window, 2, 0)
        xinput.x.XSync(xinput.d, False)
        time.sleep(3)
        initial = " ".join(row["text"] for row in words("startup"))
        assert "VIM9 IDE N " in initial + " ", initial
        assert app.poll() is None
        xinput.click(900, 500)
        xinput.text("gg0inative ")
        xinput.key(0xFF1B)
        time.sleep(0.4)
        assert source.read_text() == "original-native-text\n", "Typing unexpectedly wrote disk"
        click_word("before-save", "Save")
        wait_for(lambda: source.read_text() == "native original-native-text\n", "Native Save writes real Vim buffer")
        result["edit_and_native_save"] = True
        click_word("before-build", "Build", rightmost=True)
        time.sleep(1)
        output = " ".join(row["text"] for row in words("build-output"))
        assert "native-build-output" in output, output
        result["native_build_output"] = True
        xinput.click(900, 400)
        xinput.text("gg0ix")
        xinput.key(0xFF1B)
        time.sleep(0.3)
        click_word("before-dirty-close", "Close")
        time.sleep(0.6)
        assert app.poll() is None, "Dirty editor close destroyed the window"
        assert source.read_text() == "native original-native-text\n"
        dirty = " ".join(row["text"] for row in words("dirty-close-refused"))
        assert "modified" in dirty.lower() or "unsaved" in dirty.lower() or "changes" in dirty.lower(), dirty
        result["modified_close_refused"] = True
        click_word("before-final-save", "Save")
        wait_for(lambda: source.read_text() == "xnative original-native-text\n", "Save after refused close")
        click_word("before-final-close", "Close")
        wait_for(lambda: app.poll() is not None, "Graceful editor/window exit")
        assert app.returncode == 0, app.returncode
        assert (config / "session_history.json").read_text() == original_layout
        result.update(status="pass", graceful_exit=True, shell_layout_preserved=True)
    except Exception as error:
        result["error"] = repr(error)
        if env.get("DISPLAY"):
            try:
                capture("failure")
            except Exception:
                pass
        raise
    finally:
        (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        if xinput:
            xinput.x.XCloseDisplay(xinput.d)
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
        for log in logs:
            log.close()
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
