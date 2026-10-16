#!/usr/bin/env python3
"""Stage-one native geometry positive control; navigation only, no remote connection."""
import argparse
from fractions import Fraction
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import time


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sway(smoke, command):
    result = smoke.run(['swaymsg', '-r', command], capture_output=True, text=True)
    replies = json.loads(result.stdout)
    if not isinstance(replies, list) or not replies or any(reply.get('success') is not True for reply in replies):
        raise RuntimeError('Sway geometry command rejected: ' + result.stdout)
    with (smoke.out / 'geometry-commands.jsonl').open('a') as log:
        log.write(json.dumps({'command': command, 'reply': replies}) + '\n')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--scale', required=True, choices=['1', '1.5', '2'])
    parser.add_argument('--window', required=True, choices=['tiled', 'narrow'])
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    desktop = load('desktop', 'native-desktop-smoke.py')
    geometry = load('geometry', 'native-wayland-geometry.py')
    binary = args.binary.resolve(strict=True)
    smoke = desktop.Smoke(binary, 'wayland', out)
    result = {'status': 'running', 'scale': args.scale, 'window': args.window,
              'mode': 'normal', 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'scope': 'measured geometry and Settings Remote navigation only'}
    try:
        smoke.start_desktop()
        outputs = json.loads(smoke.run(['swaymsg', '-t', 'get_outputs', '-r'], capture_output=True, text=True).stdout)
        active = [o for o in outputs if o.get('active') is True]
        if len(active) != 1 or not re.fullmatch(r'[A-Za-z0-9_-]+', active[0]['name']):
            raise RuntimeError('Unexpected isolated output identity')
        name = active[0]['name']
        sway(smoke, f'output {name} mode 2400x1800 position 0 0 scale {args.scale} transform normal')
        scale = Fraction(args.scale)
        output_rect = {'x': 0, 'y': 0, 'width': int(2400 / scale), 'height': int(1800 / scale)}
        actual = json.loads(smoke.run(['swaymsg', '-t', 'get_outputs', '-r'], capture_output=True, text=True).stdout)
        (out / 'configured-outputs.json').write_text(json.dumps(actual, indent=2))
        active = [o for o in actual if o.get('active') is True]
        if len(active) != 1 or active[0]['name'] != name or active[0]['rect'] != output_rect:
            raise RuntimeError('Requested output geometry not applied')
        smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\nrestore_session = false\n')
        config_before = smoke.config.read_bytes()
        smoke.app = smoke.spawn([str(binary)], 'ember')
        node = desktop.wait_for(smoke.app_window, 'owned Ember window')
        sway(smoke, f'[con_id={node}] border none')
        floating = args.window == 'narrow'
        expected_rect = output_rect
        if floating:
            sway(smoke, f'[con_id={node}] floating enable')
            sway(smoke, f'[con_id={node}] border none')
            sway(smoke, f'[con_id={node}] resize set width 720 px height 720 px')
            sway(smoke, f'[con_id={node}] move position 30 40')
            expected_rect = {'x': 30, 'y': 40, 'width': 720, 'height': 720}
        # Bounded settling queries only, never replay an input or resize command.
        def settled():
            try:
                return geometry.observe(smoke, 'settling', args.scale, expected_rect, floating)
            except RuntimeError:
                if smoke.app.poll() is not None:
                    raise RuntimeError('Ember exited during geometry setup')
                return None
        measured = desktop.wait_for(settled, 'exact owned geometry')
        time.sleep(1)
        identity = smoke.command('geometry-ready')
        smoke.key('o', ('ctrl', 'shift'))
        time.sleep(0.4)
        before = geometry.capture(smoke, 'remote-tab-before', args.scale, expected_rect, floating, measured)
        rows = smoke.words('remote-tab-before', psm=6, capture=False)
        target = geometry.remote_tab(rows, before)
        result['pointer_target'] = geometry.click(smoke, 'remote-tab', target, before, args.scale, expected_rect, floating)
        time.sleep(0.4)
        after = geometry.capture(smoke, 'remote-tab-after', args.scale, expected_rect, floating, before)
        heading = geometry.remote_heading(smoke.words('remote-tab-after', psm=6, capture=False), after)
        result['remote_heading'] = heading
        # Toggle closes Settings without Save, editing a host, or invoking SSH.
        smoke.key('o', ('ctrl', 'shift'))
        time.sleep(0.3)
        if smoke.command('geometry-resumed') != identity:
            raise RuntimeError('Shell PID/TTY changed after navigation')
        if smoke.config.read_bytes() != config_before:
            raise RuntimeError('Navigation changed configuration')
        lines = smoke.transcript.read_text().splitlines()
        if lines != ['geometry-ready:' + identity, 'geometry-resumed:' + identity]:
            raise RuntimeError('Unexpected shell submission count or identity')
        geometry.capture(smoke, 'geometry-resumed', args.scale, expected_rect, floating, measured)
        result.update(status='pass', geometry=measured, identity=identity, resumed_submission_count=1,
                      config_unchanged=True)
    except Exception as error:
        result.update(status='fail', error=str(error))
        try:
            smoke.screenshot('failure')
        except Exception:
            pass
        raise
    finally:
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        try:
            smoke.close()
        except Exception as error:
            result.update(status='fail', cleanup_error=str(error))
            (out / 'result.json').write_text(json.dumps(result, indent=2))
            raise


if __name__ == '__main__':
    main()
