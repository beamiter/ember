#!/usr/bin/env python3
"""Hosted-only Settings switches with a real compositor preedit already active."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time
import importlib.util


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def zero(capture, stage):
    actual = (capture / 'live.bin').read_bytes()
    if actual != b'':
        raise AssertionError(f'{stage}: premature PTY bytes {actual.hex()}')


def transition(smoke, module, live, visual, helper, identity, old, new, seat):
    label = f'preedit-{old}-to-{new}'
    view = live.artifact_view(smoke, label)
    control = view.out / 'ime-control'
    control.mkdir()
    ime = None
    result = {'status': 'running', 'from': old, 'to': new, 'identity': identity}
    try:
        assert smoke.app.poll() is None and seat.poll() is None
        capture = smoke.raw_start(label)
        zero(capture, 'collector ready')
        ime = view.spawn([str(helper), str(control)], 'ime-client')
        def marker(name):
            def ready():
                if ime.poll() is not None:
                    raise RuntimeError(f'IME observer exited {ime.returncode} awaiting {name}')
                return (control / name).exists()
            module.wait_for(ready, label + ' ' + name)
        marker('connected')
        before = visual.snapshot(view, 'before', None, False)
        (control / 'start').touch()
        marker('preedit')
        time.sleep(0.5)
        visual.snapshot(view, 'active-preedit', before, True)
        zero(capture, 'visible active preedit')
        # The observer deliberately has NO keyboard grab. Real Settings input
        # remains routed by Ember; no helper forwards or replays the shortcut.
        smoke.switch_mode(old, new)
        zero(capture, 'Settings switch complete')
        after = visual.snapshot(view, 'cancelled-preedit', None, False)
        assert after['rectangle'] == before['rectangle'], 'Switch changed owned surface'
        (control / 'challenge').touch()
        marker('challenged')
        outcomes = [name for name in ('late-commit-sent', 'context-abandoned')
                    if (control / name).exists()]
        assert len(outcomes) == 1, 'Ambiguous native context outcome'
        time.sleep(0.5)
        zero(capture, 'late commit quarantine or abandoned context')
        visual.snapshot(view, 'after-challenge', after, False)
        (control / 'finish').touch()
        ime.wait(timeout=5)
        assert ime.returncode == 0
        assert smoke.app.poll() is None and seat.poll() is None
        # Observer has exited before finishing collector; its final drain sees
        # delayed PTY bytes too. This capture deliberately expects zero bytes.
        smoke.raw_finish(capture, identity, b'')
        result.update(status='pass', native_outcome=outcomes[0], pty_bytes=0)
    except Exception as error:
        result.update(status='fail', error=str(error))
        raise
    finally:
        try:
            if ime is not None and ime.poll() is None:
                ime.terminate()
                try:
                    ime.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    ime.kill()
                    ime.wait(timeout=5)
        except Exception as error:
            result.update(status='fail', cleanup_error=str(error))
            raise
        finally:
            (view.out / 'result.json').write_text(json.dumps(result, indent=2))
    return result


def exercise(smoke, module, live, keyboard, visual, observer, helper, seat):
    identity = smoke.command('preedit-switch-ready')
    results = []
    for old, new in zip(live.MODES, live.MODES[1:]):
        segment = transition(smoke, module, live, visual, observer, identity, old, new, seat)
        # A new native context and fresh composition prove cancellation did not
        # permanently disable IME. Same shell, exact UTF-8, no composing CR.
        label = f'fresh-after-{old}-to-{new}'
        segment['fresh_composition'] = live.composition_cycle(
            smoke, module, keyboard, visual, helper, identity, label, seat)
        results.append(segment)
    labels = ['preedit-switch-ready'] + [f'fresh-after-{a}-to-{b}-submitted'
                                         for a, b in zip(live.MODES, live.MODES[1:])]
    expected = ''.join(label + ':' + identity + '\n' for label in labels).encode()
    assert smoke.transcript.read_bytes() == expected, 'Exact sentinel transcript mismatch'
    return {'identity': identity, 'transitions': results, 'sentinels': len(labels)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'ime-client', 'preedit-client', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    scripts = Path(__file__).parent
    module = load('smoke', scripts / 'native-desktop-smoke.py')
    live = load('live', scripts / 'native-ime-live-switch.py')
    keyboard = load('keyboard', scripts / 'native-ime-keyboard.py')
    visual = load('visual', scripts / 'native-ime-visual.py')
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    smoke = module.Smoke(args.binary.resolve(strict=True), 'wayland', out)
    smoke.env['EMBER_SMOKE_CAPTURE'] = str((scripts / 'native-ime-preedit-capture.py').resolve())
    helper, observer = args.ime_client.resolve(strict=True), args.preedit_client.resolve(strict=True)
    result = {'status': 'running', 'scope': 'Settings-boundary cancellation of active compositor preedit',
              'sha256': {name: hashlib.sha256(path.read_bytes()).hexdigest()
                         for name, path in [('binary', smoke.binary), ('observer', observer), ('return_helper', helper)]}}
    try:
        smoke.start_desktop()
        seat = smoke.spawn(['wtype', '-s', '600000', '-k', 'F24'], 'ime-seat-keyboard')
        time.sleep(0.5)
        smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\nrestore_session = false\n')
        smoke.app = smoke.spawn([str(smoke.binary)], 'ember')
        module.wait_for(smoke.app_window, 'preedit switch Ember window')
        time.sleep(2)
        result.update(exercise(smoke, module, live, keyboard, visual, observer, helper, seat), status='pass')
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
            (out / 'result.json').write_text(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
