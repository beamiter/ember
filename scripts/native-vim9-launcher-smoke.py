#!/usr/bin/env python3
"""Check Space/Enter ownership after clicking Vim9, on an isolated X11 display.

Run with xvfb-run. Requires xdotool and Vim 9; all settings and files are isolated.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import time


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--tab-position", choices=("top", "sidebar"), default="top")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    for name in ("config", "state", "data", "cache", "runtime", "project"):
        (out / name).mkdir(mode=0o700)
    config = out / "config" / "ember"
    config.mkdir()
    (config / "config.toml").write_text(
        'restore_session = false\nterminal_mode = "normal"\n'
        'initial_width = 1000.0\ninitial_height = 700.0\n'
        'app_renderer = "wgpu"\njsh_update_check = "never"\n'
        f'tab_bar_position = "{args.tab_position}"\n'
    )
    shell = out / "fixture-shell"
    shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc -i\n")
    shell.chmod(0o700)
    quote = lambda value: "'" + str(value).replace("'", "''") + "'"
    vimrc = out / "fixture.vim"
    vimrc.write_text(
        "set nocompatible viminfofile=NONE noswapfile nomore nowrap\n"
        "function! LauncherState(timer) abort\n"
        "let scripts = getscriptinfo({'name': 'vim9-ide-bridge.vim$'})\n"
        f"let target = {quote(out / 'vim-state-')} . getpid() . '.json'\n"
        "call writefile([json_encode({'pid': getpid(), 'native': !empty(scripts), 'text': getline('.'), 'mode': mode()})], target . '.tmp')\n"
        "call rename(target . '.tmp', target)\n"
        "endfunction\ncall timer_start(40, function('LauncherState'), {'repeat': -1})\n"
    )
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("GVIMINIT", None)
    env.update({
        "XDG_CONFIG_HOME": str(out / "config"), "XDG_STATE_HOME": str(out / "state"),
        "XDG_DATA_HOME": str(out / "data"), "XDG_CACHE_HOME": str(out / "cache"),
        "XDG_RUNTIME_DIR": str(out / "runtime"), "EMBER_SHELL": str(shell),
        "VIMINIT": "execute 'source ' . fnameescape(" + quote(vimrc) + ")",
        "WINIT_UNIX_BACKEND": "x11", "WGPU_BACKEND": "gl", "LIBGL_ALWAYS_SOFTWARE": "1",
    })
    def xdo(*words):
        return subprocess.run(["xdotool", *map(str, words)], env=env, text=True,
                              capture_output=True, timeout=5, check=True).stdout.strip()
    def states():
        snapshots = {}
        for path in out.glob("vim-state-*.json"):
            try:
                snapshot = json.loads(path.read_text())
                snapshots[snapshot["pid"]] = snapshot
            except (FileNotFoundError, json.JSONDecodeError):
                continue
        return snapshots
    with (out / "application.log").open("w") as log:
        app = subprocess.Popen([str(args.binary.resolve())], cwd=out / "project", env=env,
                               stdout=log, stderr=log, start_new_session=True)
        def wait(probe, label):
            deadline = time.monotonic() + 25
            while time.monotonic() < deadline:
                value = probe()
                if value:
                    return value
                if app.poll() is not None:
                    raise RuntimeError(f"{label}: Ember exited {app.returncode}")
                time.sleep(.05)
            raise RuntimeError("Timed out: " + label + "; states=" + str(states()))
        def windows():
            result = subprocess.run(["xdotool", "search", "--onlyvisible", "--class", "ember"],
                                    env=env, capture_output=True, text=True, timeout=5)
            return set(result.stdout.splitlines())
        try:
            initial = wait(windows, "shell window")
            assert len(initial) == 1, initial
            window = next(iter(initial))
            xdo("windowfocus", "--sync", window)
            wait(lambda: "Shell session started successfully" in (out / "application.log").read_text(),
                 "shell startup")
            shell_result = out / "shell-space.txt"
            command = "printf '%s' 'shell space still works' > " + shlex.quote(str(shell_result))
            xdo("type", "--clearmodifiers", "--delay", 8, command)
            xdo("key", "Return")
            wait(lambda: shell_result.exists() and shell_result.read_text() == "shell space still works",
                 "shell receives spaces and Enter")
            assert not states(), "typing spaces launched Vim before an explicit click"
            geometry = dict(line.split("=", 1) for line in xdo("getwindowgeometry", "--shell", window).splitlines())
            launch_x, launch_y = int(geometry["WIDTH"]) - 94, 20
            for attempt in range(2):
                previous_pids = set(states())
                xdo("mousemove", "--window", window, launch_x, launch_y)
                xdo("click", 1)
                opened = wait(lambda: next((snapshot for pid, snapshot in states().items()
                                           if pid not in previous_pids and snapshot.get("native")), None),
                              "explicit Vim9 launch")
                editor_pid = opened["pid"]
                expected_pids = previous_pids | {editor_pid}
                def editor_text(expected, label):
                    def probe():
                        snapshots = states()
                        assert set(snapshots) == expected_pids, (
                            f"{label} opened another editor: expected {expected_pids}, got {snapshots}"
                        )
                        return snapshots[editor_pid]["text"] == expected
                    wait(probe, label)
                xdo("type", "--clearmodifiers", "--delay", 30, "i alpha  beta ")
                editor_text(" alpha  beta ", "Insert-mode spaces")
                xdo("key", "Return")
                xdo("type", "--clearmodifiers", "--delay", 30, " next line ")
                editor_text(" next line ", "Insert-mode Enter")
                xdo("key", "Escape")
                xdo("type", "--clearmodifiers", "--delay", 15, ":w! " + str(out / f"saved-{attempt}.txt"))
                xdo("key", "Return")
                saved = out / f"saved-{attempt}.txt"
                wait(lambda: saved.exists() and saved.read_text() == " alpha  beta \n next line \n",
                     "command-line spaces and Enter")
                assert set(states()) == expected_pids, "command-line input opened another editor"
                assert windows() == initial, "keyboard input opened another GUI window"
            report = {"shell_space_and_enter": True, "launcher_clicks": 2,
                      "vim_insert_and_command_space_and_enter": True,
                      "tab_position": args.tab_position, "same_gui_window": True}
            (out / "report.json").write_text(json.dumps(report, indent=2))
            print(json.dumps(report, indent=2))
        except BaseException as error:
            failure = {"error": str(error), "vim_states": states(),
                       "tab_position": args.tab_position}
            (out / "failure-state.json").write_text(json.dumps(failure, indent=2))
            if "window" in locals():
                try:
                    subprocess.run(["import", "-window", window, str(out / "failure.png")],
                                   env=env, capture_output=True, timeout=10, check=True)
                except (OSError, subprocess.SubprocessError):
                    pass
            raise
        finally:
            # start_new_session confines group signals to this fixture's app.
            if app.poll() is None:
                try:
                    os.killpg(app.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            try:
                app.wait(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(app.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                app.wait(timeout=5)


if __name__ == "__main__":
    main()
