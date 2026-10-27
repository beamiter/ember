#!/usr/bin/env python3
"""Real compositor/PTY smoke. No synthetic application messages or guard bypasses.

Requires an installed Ember binary. Isolated HOME/XDG dirs and compositor ensure
clipboard contents are synthetic. Failure retains logs/screenshots/transcripts.
The Settings driver uses rendered labels and saved configuration as independent
oracles. This initial gate is not full E11/E05 completion.
"""
import argparse
import importlib.util
import csv
import io
import tomllib
import xml.etree.ElementTree as ET
import hashlib
import json
import os
from pathlib import Path
import shlex
import select
import subprocess
import time


def wait_for(test, label, timeout=30):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = test()
        if value:
            return value
        time.sleep(0.1)
    raise RuntimeError('Timed out: ' + label)


def sentinel_command(sentinel):
    # The displayed command must not depend on the artifact directory length:
    # an extra wrapped row moves the native pixel/OCR fixture's live baseline.
    # The destination is inherited by the real shell, not injected into the app.
    return "printf '%s:%s:%s\\n' " + shlex.quote(sentinel) + ' "$$" "$(tty)" >> "$EMBER_SMOKE_SENTINELS"'


def settings_mode_selector(rows, current):
    """Exact mode value beside the Settings label, never the window title."""
    def bounds(row):
        x, y, w, h = (int(row[k]) for k in ('left', 'top', 'width', 'height'))
        if min(w, h) <= 0:
            raise RuntimeError('Invalid Settings OCR word bounds')
        return x, y, w, h
    def same_row(a, b):
        _, ay, _, ah = bounds(a)
        _, by, _, bh = bounds(b)
        return max(ay, by) < min(ay + ah, by + bh) and abs((2*ay + ah) - (2*by + bh)) <= 10
    tabs = []
    for name in ('Appearance', 'Remote'):
        matches = [r for r in rows if r['text'] == name]
        if len(matches) != 1:
            raise RuntimeError('Missing or ambiguous Settings tab anchor')
        tabs.append(matches[0])
    if not same_row(*tabs):
        raise RuntimeError('Settings tab anchors are on different rows')
    tab_bottom = max(bounds(r)[1] + bounds(r)[3] for r in tabs)
    labels = [(a, b) for a in rows for b in rows
              if a['text'] == 'Terminal' and b['text'] == 'mode'
              and bounds(a)[1] > tab_bottom and same_row(a, b)
              and 0 <= bounds(b)[0] - (bounds(a)[0] + bounds(a)[2]) <= min(bounds(a)[3], bounds(b)[3])]
    if len(labels) != 1:
        raise RuntimeError('Missing or ambiguous Settings Terminal mode label')
    label = labels[0][1]
    x, _, w, h = bounds(label)
    values = [r for r in rows if r['text'] == current and same_row(r, label)
              and 0 <= bounds(r)[0] - (x + w) <= 4 * h]
    if len(values) != 1:
        raise RuntimeError('Missing or ambiguous Settings mode value')
    return values[0]


class Smoke:
    def __init__(self, binary, backend, out):
        self.binary, self.backend, self.out = binary, backend, out
        self.processes = []
        self.logs = []
        self.env = os.environ.copy()
        for name in ('DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK'):
            self.env.pop(name, None)
        home = out / 'home'
        runtime = out / 'runtime'
        runtime.mkdir(mode=0o700)
        home.mkdir()
        for key, suffix in [('HOME', ''), ('XDG_CONFIG_HOME', '.config'),
                            ('XDG_DATA_HOME', '.local/share'), ('XDG_STATE_HOME', '.local/state'),
                            ('XDG_CACHE_HOME', '.cache')]:
            self.env[key] = str(home / suffix)
        self.env.update(EMBER_SMOKE_SHELL_TRACE=str(out / 'synthetic-shell-trace.log'), XDG_RUNTIME_DIR=str(runtime), LIBGL_ALWAYS_SOFTWARE='1',
                        WGPU_BACKEND='vulkan', RUST_LOG='info,wgpu_core=warn,wgpu_hal=info,ember::clipboard_route=debug,egui_winit=trace',
                        EMBER_SMOKE_CAPTURE=str(Path(__file__).with_name('native-pty-capture.py').resolve()),
                        EMBER_SMOKE_RAW_ROOT=str(out / 'raw-pty'))
        config = home / '.config/ember'
        config.mkdir(parents=True)
        self.config = config / 'config.toml'
        (home / '.bash_profile').write_text(r"""export HISTFILE=/dev/null
exec 9>>"$EMBER_SMOKE_SHELL_TRACE"
BASH_XTRACEFD=9
set -x
# Public OSC 133 bash integration documented in Ember's README. No private
# authentication token or foreground guard is overridden by this fixture.
__ember_smoke_prompt() {
    local status=$?
    if [[ ${__ember_smoke_seen:-0} == 1 ]]; then printf '\e]133;D;%s\a' "$status"; fi
    __ember_smoke_seen=1
}
rawcheck() { EMBER_SMOKE_RAW_OUT="$EMBER_SMOKE_RAW_ROOT/$1" python3 "$EMBER_SMOKE_CAPTURE"; }
PROMPT_COMMAND=__ember_smoke_prompt
PS0=$'\e]133;C\a'
PS1='\[\e]133;A\a\]EMBER_SMOKE> \[\e]133;B\a\]'
""")
        self.transcript = out / 'shell-sentinels.txt'
        self.env['EMBER_SMOKE_SENTINELS'] = str(self.transcript)

    def run(self, args, **kwargs):
        return subprocess.run(args, env=self.env, check=True, timeout=30, **kwargs)

    def spawn(self, args, label, **options):
        log = open(self.out / (label + '.log'), 'w')
        self.logs.append(log)
        proc = subprocess.Popen(args, env=self.env, stdout=log, stderr=log, **options)
        self.processes.append(proc)
        return proc

    def start_desktop(self):
        if self.backend == 'x11':
            spec = importlib.util.spec_from_file_location('native_x11', Path(__file__).with_name('native-x11-display.py'))
            display = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(display)
            display.start_xvfb(self.spawn, self.env, self.out, '1280x900x24')
            # Stock Openbox grabs Ctrl+Alt+Left/Right for desktop switching.
            # Keep the real Ember pane shortcuts; remove only those conflicting
            # test-WM bindings, with the original and effective config recorded.
            original = Path('/etc/xdg/openbox/rc.xml').read_bytes()
            (self.out / 'openbox-system-rc.xml').write_bytes(original)
            config = ET.fromstring(original)
            ns = {'ob': 'http://openbox.org/3.4/rc'}
            keyboard = config.find('ob:keyboard', ns)
            assert keyboard is not None, 'Openbox keyboard configuration absent'
            for chord in ('C-A-Left', 'C-A-Right'):
                matches = [node for node in keyboard.findall('ob:keybind', ns)
                           if node.get('key') == chord]
                assert len(matches) == 1, ('Openbox conflict absent or ambiguous', chord)
                actions = matches[0].findall('ob:action', ns)
                assert len(actions) == 1 and actions[0].get('name') == 'GoToDesktop'
                keyboard.remove(matches[0])
            wm_config = self.out / 'openbox-test-rc.xml'
            ET.ElementTree(config).write(wm_config, encoding='utf-8', xml_declaration=True)
            manager = self.spawn(['openbox', '--sm-disable', '--config-file', str(wm_config)], 'openbox')
            def wm_ready():
                assert manager.poll() is None, 'Openbox exited during startup; see openbox.log'
                reply = subprocess.run(['xprop', '-root', '_NET_SUPPORTING_WM_CHECK'],
                                       env=self.env, capture_output=True, text=True, timeout=3)
                (self.out / 'openbox-readiness.txt').write_text(reply.stdout + reply.stderr)
                return reply.returncode == 0 and 'window id # 0x' in reply.stdout
            wait_for(wm_ready, 'Openbox EWMH readiness')
        else:
            cfg = self.out / 'sway.conf'
            cfg.write_text('xwayland disable\noutput * resolution 1280x900\nseat seat0 fallback true\nfocus_follows_mouse no\n')
            self.env.update(WLR_BACKENDS='headless', WLR_RENDERER='pixman', WLR_LIBINPUT_NO_DEVICES='1', WLR_HEADLESS_OUTPUTS='1')
            server = self.spawn(['sway', '-d', '-c', str(cfg)], 'sway')
            runtime = Path(self.env['XDG_RUNTIME_DIR'])
            sock = wait_for(lambda: next((p for p in runtime.glob('wayland-*') if p.is_socket()), None), 'Wayland socket')
            self.env['WAYLAND_DISPLAY'] = sock.name.removesuffix('.lock')
            ipc = wait_for(lambda: next(runtime.glob('sway-ipc.*.sock'), None), 'Sway IPC')
            self.env['SWAYSOCK'] = str(ipc)
            assert server.poll() is None
            helper = os.environ.get('EMBER_POINTER_HELPER')
            assert helper, 'Wayland requires the persistent protocol pointer helper'
            log = open(self.out / 'pointer.log', 'w')
            self.logs.append(log)
            self.pointer = subprocess.Popen([helper], env=self.env, stdin=subprocess.PIPE,
                                            stdout=subprocess.PIPE, stderr=log, text=True)
            self.processes.append(self.pointer)
            self.pointer_ack('ready')
        time.sleep(1)

    def key(self, key, modifiers=()):
        if self.backend == 'x11':
            self.run(['xdotool', 'key', '--clearmodifiers', '+'.join((*modifiers, key))])
        else:
            # Each wtype invocation hotplugs a new virtual keyboard. Allow
            # compositor capability/focus events to reach the app before input.
            args = ['wtype', '-s', '250']
            for mod in modifiers:
                args += ['-M', mod]
            args += ['-k', key]
            for mod in reversed(modifiers):
                args += ['-m', mod]
            args += ['-s', '50']
            self.run(args)
        time.sleep(0.15)

    def text(self, text):
        if self.backend == 'x11':
            self.run(['xdotool', 'type', '--clearmodifiers', '--delay', '8', '--', text])
        else:
            self.run(['wtype', '-s', '250', '-d', '8', '--', text])

    def screenshot(self, name):
        target = str(self.out / (name + '.png'))
        if self.backend == 'x11':
            self.run(['import', '-window', 'root', target])
        else:
            self.run(['grim', target])

    def command(self, sentinel, paste=False):
        command = sentinel_command(sentinel)
        if paste:
            if self.backend == 'x11':
                # Keep the owner as a tracked child instead of daemonizing.
                log = open(self.out / ('clipboard-' + sentinel + '.log'), 'w')
                self.logs.append(log)
                owner = subprocess.Popen(['xclip', '-selection', 'clipboard', '-quiet'], env=self.env,
                                         stdin=subprocess.PIPE, stdout=log, stderr=log)
            else:
                log = open(self.out / ('clipboard-' + sentinel + '.log'), 'w')
                self.logs.append(log)
                owner = subprocess.Popen(['wl-copy', '--foreground'], env=self.env,
                                         stdin=subprocess.PIPE, stdout=log, stderr=log)
            self.processes.append(owner)
            owner.stdin.write(command.encode())
            owner.stdin.close()
            time.sleep(0.5)
            reader = ['xclip', '-selection', 'clipboard', '-o'] if self.backend == 'x11' else ['wl-paste', '--no-newline']
            observed = self.run(reader, capture_output=True).stdout
            assert observed == command.encode(), 'Synthetic platform clipboard readback mismatch'
            self.key('v', ('ctrl', 'shift'))
        else:
            self.text(command)
        if paste:
            time.sleep(2.5)
        self.screenshot(sentinel + '-before-enter')
        self.key('Return')
        wait_for(lambda: self.transcript.exists() and any(line.startswith(sentinel + ':') for line in self.transcript.read_text().splitlines()),
                 'PTY sentinel ' + sentinel)
        # The command must execute exactly once; this does not simulate IME composition.
        time.sleep(0.3)
        matches = [line for line in self.transcript.read_text().splitlines() if line.startswith(sentinel + ':')]
        assert len(matches) == 1, matches
        return matches[0].split(':', 1)[1]

    def app_window(self):
        if self.backend == 'x11':
            result = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(self.app.pid)],
                                    env=self.env, text=True, capture_output=True, timeout=3)
            if result.returncode == 0 and result.stdout.strip():
                windows = result.stdout.splitlines()
                assert len(windows) == 1, ('ambiguous app windows', windows)
                window = windows[0]
                metadata = self.run(['xprop', '-id', window, 'WM_CLASS', '_NET_WM_PID'], capture_output=True, text=True)
                (self.out / 'x11-window.txt').write_text(metadata.stdout)
                self.run(['xdotool', 'windowactivate', '--sync', window])
                return window
            return None
        result = subprocess.run(['swaymsg', '-t', 'get_tree', '-r'], env=self.env, text=True, capture_output=True, timeout=3)
        if result.returncode:
            return None
        tree = json.loads(result.stdout)
        (self.out / 'sway-tree.json').write_text(result.stdout)
        def find(node):
            if node.get('app_id') == 'io.github.beamiter.ember':
                return node['id']
            for child in node.get('nodes', []) + node.get('floating_nodes', []):
                hit = find(child)
                if hit:
                    return hit
        found = find(tree)
        if found:
            self.run(['swaymsg', '[con_id=' + str(found) + ']', 'focus'])
        return found

    def pointer_ack(self, expected):
        ready, _, _ = select.select([self.pointer.stdout], [], [], 5)
        assert ready, 'Pointer protocol helper response timed out'
        response = self.pointer.stdout.readline().strip()
        assert response == expected, ('Pointer helper failed', response)

    def click(self, x, y):
        if self.backend == 'x11':
            self.run(['xdotool', 'mousemove', '--sync', str(x), str(y), 'click', '1'])
        else:
            # Stage one has a single 1280x900 output at scale 1. Protocol
            # absolute coordinates have explicit extents, unlike IPC cursor
            # movement on a seat with no pointer capability.
            self.pointer.stdin.write(f'click {x} {y} 1280 900\n')
            self.pointer.stdin.flush()
            self.pointer_ack('done')
        time.sleep(0.3)

    def words(self, label, psm=11, capture=True):
        if capture:
            self.screenshot(label)
        result = self.run(['tesseract', str(self.out / (label + '.png')), 'stdout',
                           '--psm', str(psm), 'tsv'], capture_output=True, text=True)
        suffix = '' if psm == 11 else '-psm' + str(psm)
        (self.out / (label + suffix + '.tsv')).write_text(result.stdout)
        return [row for row in csv.DictReader(io.StringIO(result.stdout), delimiter='\t', quoting=csv.QUOTE_NONE)
                if row['text'].strip()]

    def click_word(self, word, label):
        matches = [row for row in self.words(label) if row['text'].strip() == word]
        if not matches:
            # A second layout analysis of the SAME captured pixels can recover
            # small buttons lost among dense terminal text. Never relax exact
            # spelling/uniqueness or retry an input action.
            matches = [row for row in self.words(label, psm=6, capture=False)
                       if row['text'].strip() == word]
        assert len(matches) == 1, (word, matches)
        row = matches[0]
        self.click(int(row['left']) + int(row['width']) // 2,
                   int(row['top']) + int(row['height']) // 2)

    def switch_mode(self, old, new):
        self.key('o', ('ctrl', 'shift'))
        time.sleep(0.4)
        # Disambiguate the Advanced tab from the Advanced Settings heading
        # using the actual tab-row anchors, never arbitrary first/top match.
        words = self.words(new + '-settings')
        anchors = []
        for label in ('Appearance', 'Remote'):
            matches = [r for r in words if r['text'] == label]
            assert len(matches) == 1, ('ambiguous tab anchor', label, matches)
            anchors.append(matches[0])
        def shares_row(row, anchor):
            return max(int(row['top']), int(anchor['top'])) < min(
                int(row['top']) + int(row['height']),
                int(anchor['top']) + int(anchor['height']))
        # Selected tab backgrounds can introduce peripheral OCR punctuation.
        # Exact normalized word only: no fuzzy/substring or coordinate fallback.
        rows = [r for r in words if r['text'].strip("‘’'\".,:;()[]").casefold() == 'advanced'
                and all(shares_row(r, anchor) for anchor in anchors)]
        assert len(rows) == 1, ('ambiguous or absent Advanced tab', rows)
        row = rows[0]
        self.click(int(row['left']) + int(row['width']) // 2,
                   int(row['top']) + int(row['height']) // 2)
        row = settings_mode_selector(self.words(new + '-mode-selector'), old.title())
        self.click(int(row['left']) + int(row['width']) // 2,
                   int(row['top']) + int(row['height']) // 2)
        self.click_word(new.title(), new + '-mode-menu')
        if self.backend == 'wayland':
            spec = importlib.util.spec_from_file_location('settings_save', Path(__file__).with_name('native-settings-save-ocr.py'))
            settings_save = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(settings_save)
            settings_save.click_save(self, new + '-mode-save')
        else:
            self.click_word('Save', new + '-mode-save')
        wait_for(lambda: tomllib.loads(self.config.read_text()).get('terminal_mode') == new,
                 'Settings persisted mode ' + new)
        self.key('o', ('ctrl', 'shift'))
        time.sleep(0.5)
        self.screenshot(new + '-live-switch')

    def own_clipboard(self, payload, case):
        log = open(self.out / ('clipboard-' + case + '.log'), 'w')
        self.logs.append(log)
        args = ['xclip', '-selection', 'clipboard', '-quiet'] if self.backend == 'x11' else ['wl-copy', '--foreground']
        owner = subprocess.Popen(args, env=self.env, stdin=subprocess.PIPE, stdout=log, stderr=log)
        self.processes.append(owner)
        owner.stdin.write(payload.encode())
        owner.stdin.close()
        time.sleep(0.5)
        assert self.read_clipboard() == payload.encode(), 'Clipboard owner readback mismatch'

    def read_clipboard(self):
        reader = ['xclip', '-selection', 'clipboard', '-o'] if self.backend == 'x11' else ['wl-paste', '--no-newline']
        return self.run(reader, capture_output=True).stdout

    def raw_start(self, case):
        self.text('rawcheck ' + case)
        self.key('Return')
        out = self.out / 'raw-pty' / case
        def ready():
            status = self.app.poll()
            if status is not None:
                raise RuntimeError(f'Ember exited {status} before raw PTY readiness: {case}; see ember.log')
            return (out / 'ready').exists()
        wait_for(ready, 'real raw PTY collector ' + case)
        return out

    def raw_finish(self, out, identity, expected):
        (out / 'expected.bin').write_bytes(expected)
        (out / 'finish').touch()
        wait_for((out / 'result.json').exists, 'raw capture completed ' + out.name)
        result = json.loads((out / 'result.json').read_text())
        assert result['status'] == 'pass', result
        assert str(result['shell_pid']) + ':' + result['tty'] == identity, result
        actual = (out / 'bytes.bin').read_bytes()
        assert actual == expected, ('wrong raw PTY bytes', out.name, actual.hex(), expected.hex())
        time.sleep(0.3)

    def find_clipboard_ownership(self, identity, case):
        payload = 'EMBERUI' + case.replace('-', '').upper()
        # A zero-byte result is meaningful only with a same-mode positive control.
        out = self.raw_start(case + '-clipboard-positive')
        self.own_clipboard(payload, case + '-positive')
        self.key('v', ('ctrl', 'shift'))
        time.sleep(0.5)
        self.raw_finish(out, identity, payload.encode())
        for suffix, modifiers in [('ctrlv', ('ctrl',)), ('ctrlshiftv', ('ctrl', 'shift'))]:
            label = case + '-find-' + suffix
            payload = 'EMBERUI' + label.replace('-', '').upper()
            out = self.raw_start(label)
            self.key('f', ('ctrl', 'shift'))
            time.sleep(0.3)
            self.key('a', ('ctrl',))
            self.key('BackSpace')
            self.own_clipboard(payload, label)
            self.key('v', modifiers)
            self.screenshot(label + '-pasted')
            self.key('a', ('ctrl',))
            # Replace the source clipboard before Copy, so a stale source value
            # cannot masquerade as successful UI paste/selection/copy.
            self.own_clipboard('DECOY' + label, label + '-decoy')
            self.key('c', ('ctrl',))
            wait_for(lambda: self.read_clipboard() == payload.encode(), 'Find clipboard roundtrip ' + label)
            (out / 'ui-readback.txt').write_bytes(self.read_clipboard())
            self.key('Escape')
            time.sleep(0.3)
            self.raw_finish(out, identity, b'')

    def raw_punctuation_oracle(self, identity, case):
        self.text('rawcheck ' + case)
        self.key('Return')
        out = self.out / 'raw-pty' / case
        wait_for((out / 'ready').exists, 'real raw PTY collector')
        # Keep the original failing virtual-keyboard redirection sequence.
        # It must arrive as text only, with no CR/Enter inserted before `>`.
        payload = "printf '%s:%s:%s\\n' block-switched \"$$\" \"$(tty)\" >> /tmp/synthetic"
        (out / 'expected.bin').write_bytes(payload.encode())
        self.text(payload)
        self.screenshot(case + '-raw-punctuation-before-finish')
        (out / 'finish').touch()
        wait_for((out / 'result.json').exists, 'raw byte capture completed')
        result = json.loads((out / 'result.json').read_text())
        assert result['status'] == 'pass', result
        assert str(result['shell_pid']) + ':' + result['tty'] == identity, result
        actual = (out / 'bytes.bin').read_bytes()
        assert actual == payload.encode(), ('wrong raw PTY bytes', actual.hex(), payload.encode().hex())
        time.sleep(0.3)

    def exercise(self):
        self.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\n'
                               'initial_width = 1100\ninitial_height = 800\nrestore_session = false\n')
        app = self.app = self.spawn([str(self.binary)], 'ember')
        wait_for(self.app_window, 'Ember window')
        time.sleep(2)
        assert app.poll() is None
        self.screenshot('normal-startup')
        identity = self.command('normal-typed')
        self.raw_punctuation_oracle(identity, 'normal')
        self.find_clipboard_ownership(identity, 'initial-normal')
        assert self.command('normal-paste', paste=True) == identity
        old = 'normal'
        for mode in ('block', 'chat', 'normal'):
            self.switch_mode(old, mode)
            self.raw_punctuation_oracle(identity, mode + 'switch')
            self.find_clipboard_ownership(identity, 'switched-' + mode)
            assert self.command(mode + '-switched') == identity, 'Mode switch replaced shell'
            assert self.command(mode + '-switched-paste', paste=True) == identity
            old = mode
        self.key('e', ('ctrl', 'shift'))
        time.sleep(1)
        second = self.command('split-right')
        assert second != identity, 'Split must own a different shell'
        self.key('Left', ('ctrl', 'alt'))
        assert self.command('split-left-return') == identity
        self.key('Right', ('ctrl', 'alt'))
        assert self.command('split-right-return') == second
        self.screenshot('panes-completed')
        lines = self.transcript.read_text().splitlines()
        expected = ['normal-typed', 'normal-paste', 'block-switched', 'block-switched-paste',
                    'chat-switched', 'chat-switched-paste', 'normal-switched', 'normal-switched-paste',
                    'split-right', 'split-left-return', 'split-right-return']
        assert len(lines) == len(expected), lines
        assert [line.split(':', 1)[0] for line in lines] == expected
        assert app.poll() is None, 'Application exited unexpectedly'
        return {'sentinels': len(lines), 'primary_identity': identity, 'split_identity': second,
                'live_modes': ['normal', 'block', 'chat', 'normal'],
                'find_clipboard_zero_byte_cases': 8, 'clipboard_positive_controls': 4}

    def close(self):
        for proc in reversed(self.processes):
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
        for log in self.logs:
            log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--backend', required=True, choices=['x11', 'wayland'])
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    args.binary = args.binary.resolve(strict=True)
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    result = {'backend': args.backend, 'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              'excluded': ['IME composition', 'physical GPU', 'fractional scale and narrow window',
              'selection/copy geometry', 'task/settings UI clipboard ownership and adversarial clipboard scenarios', 'clipboard latency']}
    smoke = Smoke(args.binary, args.backend, args.output)
    try:
        smoke.start_desktop()
        result.update(smoke.exercise())
        result['status'] = 'pass'
    except Exception as error:
        result.update(status='fail', error=str(error))
        try:
            smoke.screenshot('failure')
        except Exception:
            pass
        raise
    finally:
        result_path = args.output / 'result.json'
        result_path.write_text(json.dumps(result, indent=2))
        try:
            smoke.close()
        except Exception as cleanup_error:
            result.update(status='fail', cleanup_error=str(cleanup_error))
            result_path.write_text(json.dumps(result, indent=2))
            raise


if __name__ == '__main__':
    main()
