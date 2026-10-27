#!/usr/bin/env python3
"""One live Ember shell across real Settings switches and compositor IME cycles."""
import argparse
import copy
import hashlib
import importlib.util
import json
import subprocess
from pathlib import Path
import time

MODES = ('normal', 'block', 'chat', 'normal')


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def artifact_view(smoke, label):
    # Same app, environment, compositor, process/log ownership and shell. Only
    # screenshot/helper evidence goes into a fresh per-segment directory.
    view = copy.copy(smoke)
    view.out = smoke.out / label
    view.out.mkdir(exist_ok=False)
    return view


def composition_cycle(smoke, module, keyboard_module, visual, helper, identity, label, seat,
                      *, preedit_psm=6, health_psm=6, health_footer=False):
    view = artifact_view(smoke, label)
    control = view.out / 'ime-control'
    control.mkdir(exist_ok=False)
    keyboard = None
    ime = None
    result = {'status': 'running', 'identity': identity, 'label': label}
    try:
        assert seat.poll() is None, 'Persistent seat keyboard exited'
        view.park_pointer()
        ime = view.spawn([str(helper), str(control)], 'ime-client')
        def marker(name):
            def ready():
                if ime.poll() is not None:
                    raise RuntimeError(f'IME helper exited {ime.returncode} awaiting {name}')
                return (control / name).exists()
            module.wait_for(ready, label + ' IME ' + name)
        marker('connected')
        capture = smoke.raw_start(label)
        assert (capture / 'live.bin').read_bytes() == b''
        keyboard = keyboard_module.CompositionKeyboard(view)
        module.wait_for(keyboard.ready, label + ' owned keyboard release roundtrip')
        assert seat.poll() is None
        assert (capture / 'live.bin').read_bytes() == b'', 'Modifier readiness leaked PTY bytes'
        health_options = {'health_psm': health_psm} if health_psm != 6 else {}
        if health_footer:
            health_options['health_footer'] = True
        before = visual.snapshot(view, 'ime-visual-before', None, False, **health_options)
        (control / 'start').touch()
        marker('preedit')
        time.sleep(0.5)
        # Select Chat's sparse preedit layout before capture, never in response
        # to OCR content. Keep custom lifecycle adapters on their own layout.
        options = {'token_psm': preedit_psm} if preedit_psm != 6 else {}
        during = visual.snapshot(view, 'ime-preedit-zero-bytes', before, True,
                                 **health_options, **options)
        preedit = (capture / 'live.bin').read_bytes()
        assert preedit == b'', ('preedit leaked bytes', preedit.hex())
        (capture / 'preedit.bin').write_bytes(preedit)
        keyboard.release_return(control / 'preedit', capture)
        marker('committed')
        expected = '你'.encode()
        module.wait_for(lambda: (capture / 'live.bin').read_bytes() == expected,
                        label + ' exact commit without CR')
        def finalize_input():
            (control / 'finish').touch()
            ime.wait(timeout=5)
            assert ime.returncode == 0
            smoke.raw_finish(capture, identity, expected)
        after = visual.snapshot(view, 'ime-commit-without-cr', before, False,
                                after_capture=finalize_input, **health_options)
        result['preedit_pixels'] = visual.verify_restoration(view, before, during, after)
        assert keyboard.process.poll() is None, 'Composition keyboard exited before grab release'
        assert seat.poll() is None, 'Persistent seat keyboard exited'
        keyboard.stop()
        # One explicit command submission after consumed composition Return.
        assert smoke.command(label + '-submitted') == identity, 'Live switch replaced shell'
        result.update(status='pass', preedit_bytes=0, committed_hex=expected.hex(),
                      composition_return_cr_bytes=0, explicit_submission_count=1,
                      keyboard_arguments=keyboard_module.KEYBOARD_ARGS)
    except Exception as error:
        result.update(status='fail', error=str(error))
        raise
    finally:
        result['helper_exit_code_before_cleanup'] = None if ime is None else ime.poll()
        (view.out / 'result.json').write_text(json.dumps(result, indent=2))
        # Stop/reap a blocked keyboard before stdin disposal. Never release the
        # Return barrier on a failed path. No other segment may start until both
        # owned helper processes are gone.
        try:
            try:
                if keyboard is not None:
                    keyboard.stop()
            finally:
                if ime is not None and ime.poll() is None:
                    ime.terminate()
                    try:
                        ime.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        ime.kill()
                        ime.wait(timeout=5)
        except Exception as error:
            result.update(status='fail', cleanup_error=str(error))
            (view.out / 'result.json').write_text(json.dumps(result, indent=2))
            raise
    return result


def exercise(smoke, module, keyboard_module, visual, helper, seat):
    identity = smoke.command('live-ime-ready')
    segments = []
    old = 'normal'
    for index, mode in enumerate(MODES):
        label = f'live-{index}-{mode}'
        if index:
            smoke.switch_mode(old, mode)
        assert smoke.app.poll() is None and seat.poll() is None
        # Existing same-mode positive control + both Find paste chords retain
        # their exact clipboard readback and zero raw PTY byte assertions.
        smoke.find_clipboard_ownership(identity, label)
        segment = composition_cycle(smoke, module, keyboard_module, visual,
                                    helper, identity, label, seat,
                                    preedit_psm=11 if mode == 'chat' else 6,
                                    health_psm=11, health_footer=True)
        segment['mode'] = mode
        segments.append(segment)
        old = mode
    expected = ['live-ime-ready'] + [f'live-{i}-{mode}-submitted' for i, mode in enumerate(MODES)]
    expected_bytes = ''.join(label + ':' + identity + '\n' for label in expected).encode('utf-8')
    actual_bytes = smoke.transcript.read_bytes()
    assert actual_bytes == expected_bytes, ('Exact sentinel transcript mismatch', actual_bytes.hex(), expected_bytes.hex())
    return {'identity': identity, 'segments': segments, 'sentinels': len(expected),
            'find_zero_byte_cases': 8, 'clipboard_positive_controls': 4}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--ime-client', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    scripts = Path(__file__).parent
    module = load('smoke', scripts / 'native-desktop-smoke.py')
    keyboard = load('keyboard', scripts / 'native-ime-keyboard.py')
    visual = load('visual', scripts / 'native-ime-visual.py')
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    helper = args.ime_client.resolve(strict=True)
    smoke = module.Smoke(args.binary.resolve(strict=True), 'wayland', out)
    smoke.env['EMBER_SMOKE_CAPTURE'] = str((scripts / 'native-ime-pty-capture.py').resolve())
    result = {'status': 'running', 'live_modes': list(MODES), 'scale': 1,
              'binary_sha256': hashlib.sha256(smoke.binary.read_bytes()).hexdigest(),
              'helper_sha256': hashlib.sha256(helper.read_bytes()).hexdigest(),
              'scope': 'live mode transitions between completed compositions; no switch during active preedit'}
    try:
        smoke.start_desktop()
        seat = smoke.spawn(['wtype', '-s', '600000', '-k', 'F24'], 'ime-seat-keyboard')
        time.sleep(0.5)
        # Keep the same readable visual-fixture fonts through every Settings
        # switch; the tiny default Chat preedit makes OCR invent punctuation.
        smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\nrestore_session = false\nfont_size = 16.0\nui_font_size = 16.0\n')
        smoke.app = smoke.spawn([str(smoke.binary)], 'ember')
        module.wait_for(smoke.app_window, 'live IME Ember window')
        time.sleep(2)
        result.update(exercise(smoke, module, keyboard, visual, helper, seat), status='pass')
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
