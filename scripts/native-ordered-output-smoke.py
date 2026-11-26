#!/usr/bin/env python3
"""Verify the final clipboard effect and peer tab after real PTY output and exit."""
import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import select
import shlex
import subprocess
import sys
import termios
import time
import tty


def publish(root, name, payload):
    target = root / (name + '.json')
    pending = target.with_suffix('.tmp')
    pending.write_text(json.dumps(payload))
    pending.replace(target)


def producer(root, lane):
    root.mkdir()
    terminal = os.open('/dev/tty', os.O_RDWR)
    saved = termios.tcgetattr(terminal)

    def write(data):
        rest = memoryview(data)
        while rest:
            rest = rest[os.write(terminal, rest):]

    def wait_for(name):
        deadline = time.monotonic() + 30
        while not (root / name).exists():
            assert time.monotonic() < deadline, name
            time.sleep(0.02)

    def query(row, col):
        write(f'\x1b[{row};{col}H\x1b[6n'.encode())
        answer = b''
        cutoff = time.monotonic() + 20
        while not answer.endswith(b'R'):
            budget = cutoff - time.monotonic()
            assert budget > 0 and select.select([terminal], [], [], budget)[0], answer
            chunk = os.read(terminal, 16)
            assert chunk, 'PTY closed waiting for a cursor reply'
            answer += chunk
            assert len(answer) < 32, answer
        assert answer == f'\x1b[{row};{col}R'.encode(), answer
        return answer.hex()

    try:
        tty.setraw(terminal)
        early = query(3, 4)
        publish(root, 'ready', {'pid': os.getpid(), 'tty': os.ttyname(0), 'reply': early})
        wait_for('emit')
        payload = b'ember output before final cursor query ' + b'.' * 50 + b'\r\n'
        for _ in range(10):
            write(payload * 1500)
        late = query(7, 19)
        publish(root, 'parsed', {'reply': late, 'bytes': len(payload) * 15000})
        wait_for('exit')
        token = f'ember-{lane}-tail-after-query'.encode()
        write(b'FINAL_TAIL\x1b]52;c;' + base64.b64encode(token) + b'\x07')
    except Exception as error:
        publish(root, 'error', {'error': str(error)})
        raise
    finally:
        termios.tcsetattr(terminal, termios.TCSANOW, saved)
        os.close(terminal)
    return 37


def run(binary, output, backend):
    module_path = Path(__file__).with_name('native-desktop-smoke.py').resolve()
    sys.path.insert(0, str(module_path.parent))
    spec = importlib.util.spec_from_file_location('ember_desktop', module_path)
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    smoke = runner.Smoke(binary, backend, output)
    workspace = Path(smoke.env['XDG_RUNTIME_DIR']) / 'workspace'
    smoke.env.update(JTERM_WORKSPACE_STATE_DIR=str(workspace))
    smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "block"\napp_renderer = "glow"\n'
                            'restore_session = false\nosc52_clipboard_write = true\n'
                            'initial_width = 1100\ninitial_height = 800\neva_companion_enabled = false\n')
    smoke.config.chmod(0o600)
    result = {'backend': backend, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'cases': {}}

    def open_receipt(root, name):
        def read():
            failure = root / 'error.json'
            if failure.exists():
                raise AssertionError(failure.read_text())
            record = root / (name + '.json')
            return json.loads(record.read_text()) if record.exists() else None
        return runner.wait_for(read, f'{root.name} {name}', timeout=24)

    def clipboard_matches(expected):
        if backend == 'x11':
            args = ['xclip', '-selection', 'clipboard', '-o']
        else:
            args = ['wl-paste', '--no-newline']
        reply = subprocess.run(args, env=smoke.env,
                               capture_output=True, timeout=3)
        return reply.returncode == 0 and reply.stdout == expected

    try:
        smoke.start_desktop()
        if backend == 'wayland':
            smoke.spawn(['wtype', '-s', '600000', '-k', 'F24'], 'seat-keyboard')
        # Track the private broker so the desktop fixture owns its teardown.
        host = smoke.spawn([str(binary), '--jterm-session-host'], 'workspace-host')
        runner.wait_for((workspace / 'run/host.sock').is_socket, 'workspace host')
        assert host.poll() is None
        smoke.app = smoke.spawn([str(binary)], 'ember')
        runner.wait_for(smoke.app_window, 'Ember window')
        time.sleep(1)
        peer = smoke.command('peer-original')
        for lane in ('hidden', 'active'):
            smoke.key('t', ('ctrl', 'shift'))
            identity = smoke.command(lane + '-original')
            assert identity != peer
            root = output / lane
            script = Path(__file__).resolve()
            argv = [sys.executable, str(script), '--produce', str(root), '--lane', lane]
            smoke.text('exec ' + shlex.join(argv))
            smoke.key('Return')
            ready = open_receipt(root, 'ready')
            assert identity == f'{ready["pid"]}:{ready["tty"]}'
            if lane == 'hidden':
                smoke.key('Page_Up', ('ctrl',))
                assert smoke.command(lane + '-peer') == peer
            (root / 'emit').touch()
            parsed = open_receipt(root, 'parsed')
            smoke.screenshot(lane + '-after-cursor-query')
            (root / 'exit').touch()
            expected = f'ember-{lane}-tail-after-query'.encode()
            runner.wait_for(lambda: clipboard_matches(expected), 'native clipboard from the final PTY batch')
            runner.wait_for(lambda: not Path(f'/proc/{ready["pid"]}').exists(), 'real PTY leader reaped')
            # Give the exit reducer a frame before testing keyboard ownership.
            time.sleep(0.7)
            returned = smoke.command(lane + '-returned-peer')
            assert returned == peer
            if lane == 'hidden':
                smoke.key('Page_Down', ('ctrl',))
                assert smoke.command(lane + '-only-peer-left') == peer
            result['cases'][lane] = {'shell': identity, 'peer_shell': returned,
                                     'first_cursor_reply': ready['reply'], 'last_cursor_reply': parsed['reply'],
                                     'processed_bytes': parsed['bytes'], 'clipboard': expected.decode()}
        result['status'] = 'pass'
    except Exception as error:
        result.update(status='fail', error=str(error))
        raise
    finally:
        try:
            smoke.close()
        except Exception as error:
            result.update(status='fail', cleanup_error=str(error))
            raise
        finally:
            publish(output, 'result', result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--produce', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--lane', choices=['hidden', 'active'], help=argparse.SUPPRESS)
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--backend', choices=['x11', 'wayland'])
    args = parser.parse_args()
    if args.produce is not None and args.lane is not None:
        return producer(args.produce, args.lane)
    if args.binary is None or args.output is None or args.backend is None:
        parser.error('--binary, --output and --backend are required')
    run(args.binary.resolve(strict=True), args.output.resolve(), args.backend)
    return 0


if __name__ == '__main__':
    sys.exit(main())
