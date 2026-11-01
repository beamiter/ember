#!/usr/bin/env python3
"""Capture the actual native Ember window with an explicitly named PTY fixture.

Owns an Xvfb server and isolated HOME/XDG directories. Never installs or starts
the user's ordinary application. Screenshots are window pixels, not a renderer
facsimile. This initial gate covers structure, real shell input and resizing;
IME/preedit and final reference pixel acceptance remain separate gates.
"""
import argparse
import importlib.util
import hashlib
import re
import shutil
import json
import os
from pathlib import Path
import shlex
import subprocess
import time


def wait_for(check, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise RuntimeError("Timed out: " + label)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--scale", type=float, choices=(1.0, 1.25, 1.5), default=1.0)
    parser.add_argument("--fixture-ui", action="store_true")
    parser.add_argument("--details", action="store_true")
    parser.add_argument("--stream", action="store_true")
    parser.add_argument("--dock-transitions", action="store_true")
    args = parser.parse_args()
    if args.stream and args.fixture_ui:
        parser.error("Streaming proof requires the production native canvas, without visual fixture overlay")
    args.out.mkdir(parents=True, exist_ok=False)
    repo = Path(__file__).resolve().parents[1]
    code_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()
    dirty_diff = subprocess.check_output(["git", "diff", "HEAD", "--binary"], cwd=repo)
    binary = args.out / "ember-captured-binary"
    shutil.copy2(args.binary, binary)
    binary_sha256 = hashlib.sha256(binary.read_bytes()).hexdigest()
    core_head = subprocess.check_output(["git","-C",str(repo.parent / "jterm_core"),"rev-parse","HEAD"],text=True).strip()
    provenance = {"git_head": code_head, "core_git_head":core_head, "dirty_diff_sha256": hashlib.sha256(dirty_diff).hexdigest(),
                  "binary_sha256": binary_sha256}
    build_manifest = os.environ.get("JTERM_CAPTURE_BUILD_PROVENANCE")
    if build_manifest:
        built = json.loads(Path(build_manifest).read_text())
        assert built["binary_sha256"] == binary_sha256, "Build provenance must match copied native binary"
        provenance.update(capture_git_head=code_head,capture_core_git_head=core_head)
        provenance.update(built)
    env = os.environ.copy()
    for name in ("DISPLAY", "WAYLAND_DISPLAY", "SWAYSOCK"):
        env.pop(name, None)
    processes, handles, results = [], [], []

    def spawn(command, label, **options):
        handle = (args.out / (label + ".log")).open("w")
        handles.append(handle)
        process = subprocess.Popen(command, env=env, stdout=handle, stderr=handle, **options)
        processes.append(process)
        return process

    def run(command, **options):
        return subprocess.run(command, env=env, check=True, timeout=30, **options)

    try:
        spec = importlib.util.spec_from_file_location("native_x11", Path(__file__).with_name("native-x11-display.py"))
        native = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(native)
        native.start_xvfb(spawn, env, args.out, "2600x1800x24")
        for mode in ("block", "chat"):
            home = args.out / mode / "home"
            home.mkdir(parents=True, mode=0o700)
            home.chmod(0o700)
            runtime = args.out / mode / "runtime"
            runtime.mkdir(mode=0o700)
            for name, suffix in (("HOME", ""), ("XDG_CONFIG_HOME", ".config"),
                                 ("XDG_DATA_HOME", ".local/share"), ("XDG_STATE_HOME", ".local/state"),
                                 ("XDG_CACHE_HOME", ".cache")):
                env[name] = str(home / suffix)
            env.pop("JTERM_REFERENCE_FIXTURE", None)
            if args.fixture_ui: env["JTERM_REFERENCE_FIXTURE"] = "1"
            env.update(XDG_RUNTIME_DIR=str(runtime), EMBER_SHELL="/bin/bash", LIBGL_ALWAYS_SOFTWARE="1", RUST_LOG="ember::reference_workspace=info")
            sentinel = args.out / mode / "pty-sentinel.txt"
            env["REFERENCE_PTY_SENTINEL"] = str(sentinel)
            (home / ".bash_profile").write_text("""export HISTFILE=/dev/null
__reference_prompt() {
    local status=$?
    if [[ ${__reference_seen:-0} == 1 ]]; then printf '\\e]133;D;%s\\a' "$status"; fi
    __reference_seen=1
}
PROMPT_COMMAND=__reference_prompt
PS0=$'\\e]133;C\\a'
PS1='\\[\\e]133;A\\a\\]$ \\[\\e]133;B\\a\\]'
""")
            config = home / ".config/ember"
            config.mkdir(parents=True, mode=0o700)
            config.parent.chmod(0o700)
            config.chmod(0o700)
            (config / "config.toml").write_text(f"""initial_width = 1356.0
initial_height = 704.0
restore_session = false
app_renderer = "glow"
gpu_rendering = false
font_size = 12.0
ui_font_size = 14.0
ui_scale = {args.scale}
terminal_mode = "{mode}"
theme = "dark"
shell = "/bin/bash"
eva_companion_enabled = false
ascii_organism_enabled = true
""")
            (config / "config.toml").chmod(0o600)
            (home / ".bash_profile").chmod(0o600)
            app = spawn([str(binary.resolve())], mode + "-app", cwd=str(home))

            def find_window():
                if app.poll() is not None:
                    raise RuntimeError(f"{mode} exited with {app.returncode}")
                query = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(app.pid)],
                                       env=env, capture_output=True, text=True)
                return query.stdout.splitlines()[-1] if query.returncode == 0 and query.stdout.splitlines() else None

            window = wait_for(find_window, mode + " native window")
            run(["xdotool", "windowsize", "--sync", window, str(round(1356 * args.scale)), str(round(704 * args.scale))])
            run(["xdotool", "windowfocus", "--sync", window])
            time.sleep(2)
            empty = args.out / f"ember-{mode}-empty.png"
            run(["import", "-window", window, str(empty)])
            command = "bash " + shlex.quote(str(args.fixture.resolve())) + "; printf '%s\\n' REFERENCE_PTY_ONCE >> \"$REFERENCE_PTY_SENTINEL\""
            run(["xdotool", "type", "--clearmodifiers", "--delay", "2", command])
            run(["xdotool", "key", "--clearmodifiers", "Return"])
            wait_for(lambda: sentinel.exists(), mode + " real PTY input")
            assert sentinel.read_text().splitlines() == ["REFERENCE_PTY_ONCE"], "Input executed more than once"
            time.sleep(1)
            run(["xdotool", "mousemove", "1700", "1000"])
            image = args.out / f"ember-{mode}-1356x704.png"
            run(["import", "-window", window, str(image)])
            geometry = run(["xdotool", "getwindowgeometry", "--shell", window], capture_output=True, text=True).stdout
            observation = re.findall(r"REFERENCE_SHELL_STATE presentation=(\w+) logical=([0-9.]+)x([0-9.]+) dpr=([0-9.]+)",
                                     (args.out / (mode + "-app.log")).read_text())[-1]
            assert observation[0].lower() == mode, "Capture must match actual application presentation"
            assert tuple(map(float, observation[1:3])) == (1356.0, 704.0), "Logical dimensions differ"
            results.append({**provenance, "requested_mode": mode, "actual_mode": observation[0],
                            "logical_size": list(map(float, observation[1:3])), "dpr": float(observation[3]),
                            "capture": str(image), "geometry": geometry,
                            "real_pty_once": True, "rich_fixture_ui": args.fixture_ui, "fixture": str(args.fixture.resolve())})
            def extra_at_reference(image, detail):
                results.append({**provenance, "actual_mode":observation[0], "logical_size":[1356,704],
                                "dpr":float(observation[3]), "capture":str(image), "detail":detail,
                                "app_pid":app.pid, "rich_fixture_ui":args.fixture_ui})
            extra_at_reference(empty, "Native startup before PTY fixture input")
            if args.details:
                time.sleep(1.2)
                image = args.out / f"ember-{mode}-quiet-canvas.png"
                run(["import", "-window", window, str(image)])
                extra_at_reference(image, "Quiet production canvas; organism obeys actual native safe-area admission")
                # Shared logical footer geometry, not a screenshot-derived locator.
                run(["xdotool", "mousemove", "--window", window, str(round(414 * args.scale)), str(round(677 * args.scale))])
                run(["xdotool", "click", "1"])
                time.sleep(0.5)
                image = args.out / f"ember-{mode}-tools.png"
                run(["import", "-window", window, str(image)])
                extra_at_reference(image, "Actual Tools popover after pointer click")
                run(["xdotool", "key", "Escape"])
                run(["xdotool", "mousemove", "--window", window, str(round(700 * args.scale)), str(round(400 * args.scale))])
                run(["xdotool", "click", "5", "click", "5", "click", "5", "click", "5"])
                time.sleep(0.5)
                image = args.out / f"ember-{mode}-scrolled.png"
                run(["import", "-window", window, str(image)])
                extra_at_reference(image, "Native wheel scrolling")
            def record_extra(image, width, height, detail):
                state = re.findall(r"REFERENCE_SHELL_STATE presentation=(\w+) logical=([0-9.]+)x([0-9.]+) dpr=([0-9.]+)",
                                   (args.out / (mode + "-app.log")).read_text())[-1]
                actual = tuple(map(float,state[1:3]))
                expected = (round(width * args.scale) / args.scale, round(height * args.scale) / args.scale)
                assert state[0].lower() == mode and all(abs(a-e) <= 1/64 for a,e in zip(actual,expected)), "Logical size must match its actual rounded physical extent"
                results.append({**provenance, "actual_mode":state[0], "logical_size":list(actual), "requested_logical_size":[width,height],
                                "physical_size":[round(width*args.scale),round(height*args.scale)],
                                "dpr":float(state[3]), "capture":str(image), "detail":detail,
                                "app_pid":app.pid, "rich_fixture_ui":args.fixture_ui})
            if args.dock_transitions:
                if args.scale != 1.0 or args.fixture_ui:
                    raise RuntimeError("Dock transitions require unscaled production glass UI")
                spec = importlib.util.spec_from_file_location("glass_dock", repo / "scripts/native_glass_profile.py")
                glass = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(glass)
                # A long, unsubmitted real readline command must survive mode
                # switches and width reflow byte-for-byte, then execute once.
                payload = "DOCK_" + "abcdefghij" * 60
                dock_sentinel = args.out / mode / "dock-pty.txt"
                command = "printf '%s' " + shlex.quote(payload) + " > " + shlex.quote(str(dock_sentinel))
                run(["xdotool", "type", "--clearmodifiers", "--delay", "1", command])
                for index, (width, height, target) in enumerate([
                    (700,640,"chat"), (1500,850,"chat"),
                    (800,640,"block"), (1356,704,"chat"), (1356,704,mode),
                ]):
                    run(["xdotool","windowsize","--sync",window,str(width),str(height)])
                    time.sleep(0.3)
                    slot = glass.mode_slot(dict(x=0,y=0,width=width,height=height), target)
                    run(["xdotool","mousemove","--window",window,
                         str(slot['x']+slot['width']//2),str(slot['y']+slot['height']//2),"click","1"])
                    time.sleep(0.4)
                    image = args.out / f"ember-{mode}-dock-{index}.png"
                    run(["import","-window",window,str(image)])
                    from PIL import Image
                    with Image.open(image) as pixels:
                        glass.header_paint(pixels, dict(x=0,y=0,width=width,height=height), target)
                    assert run(["xdotool","getwindowfocus"],capture_output=True,text=True).stdout.strip() == window
                    assert not dock_sentinel.exists(), "Layout must not submit input"
                # Native footer click restores terminal focus after chrome controls.
                run(["xdotool","mousemove","--window",window,"650","565","click","1"])
                run(["xdotool","key","End","Return"])
                wait_for(dock_sentinel.exists, "long command after dock transitions")
                assert dock_sentinel.read_text() == payload, "PTY input changed during layout transitions"
                results.append({**provenance, "requested_mode":mode,
                                "dock_transition_pty_exact":True,
                                "scope":"X11/Glow input and mode/resize smoke, not IME or WGPU acceptance"})
            if args.stream:
                # Exercise the real retained terminal, not the visual fixture's ScrollArea.
                stream_file = args.out / mode / "stream-state.txt"
                env_path = shlex.quote(str(stream_file))
                run(["xdotool","mousemove","--window",window,str(round(500*args.scale)),str(round(630*args.scale)),"click","1"])
                command = "printf 'START\\n' > " + env_path + "; for ((i=1;i<=360;i++)); do printf 'REFERENCE_STREAM_ROW_%04d\\n' \"$i\"; sleep 0.025; done; printf 'END\\n' >> " + env_path
                run(["xdotool", "type", "--clearmodifiers", "--delay", "1", command])
                run(["import","-window",window,str(args.out / f"ember-{mode}-stream-command.png")])
                run(["xdotool", "key", "Return"])
                wait_for(stream_file.exists, "real streaming process start")
                time.sleep(3.0)
                run(["xdotool", "mousemove", "--window", window, str(round(700*args.scale)), str(round(350*args.scale))])
                run(["xdotool", "click", "--repeat", "8", "--delay", "50", "4"])
                time.sleep(0.6)
                snapshots=[]
                region=f"{round(480*args.scale)}x{round(180*args.scale)}+{round(420*args.scale)}+{round(230*args.scale)}"
                for label in ("before", "after"):
                    assert stream_file.read_text().splitlines() == ["START"], "Stream must still be running during reading proof"
                    image=args.out/f"ember-{mode}-stream-reading-{label}.png"
                    run(["import","-window",window,str(image)])
                    snapshots.append(run(["convert",str(image),"-crop",region,"+repage","-alpha","off","-depth","8","RGB:-"],capture_output=True).stdout)
                    extra_at_reference(image,"Live stream while reading history")
                    if label == "before":time.sleep(0.8)
                assert snapshots[0] == snapshots[1], "Streaming moved the native reader's history pixels"
                assert run(["xdotool","getwindowfocus"],capture_output=True,text=True).stdout.strip() == window, "Native app lost keyboard focus"
                results[-1]["stream_reading_pixels_unchanged"] = True
                results[-1]["stream_region"] = region
                results[-1]["stream_rgb_sha256"] = hashlib.sha256(snapshots[0]).hexdigest()
                wait_for(lambda: stream_file.read_text().splitlines() == ["START","END"],"real streaming completion")
            for width,height,detail in [(700,640,"navigation and inspector collapsed"),(800,640,"inspector collapsed"),(1500,850,"wide native window")]:
                run(["xdotool", "windowsize", "--sync", window, str(round(width * args.scale)), str(round(height * args.scale))])
                time.sleep(1)
                image=args.out/f"ember-{mode}-{width}x{height}.png"
                run(["import","-window",window,str(image)])
                record_extra(image,width,height,detail)
            app.terminate()
            app.wait(timeout=15)
        (args.out / "results.json").write_text(json.dumps({"captures": results,
            "limitations": ["No original-reference pixel comparison in this environment", "Native IME preedit is a separate gate", "Live-stream reading retention uses separate retained-canvas tests"]}, indent=2))
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
        for handle in handles:
            handle.close()


if __name__ == "__main__":
    main()
