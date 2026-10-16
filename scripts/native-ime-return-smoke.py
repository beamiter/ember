#!/usr/bin/env python3
"""Genuine compositor IME Return consumption with real raw PTY bytes."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runner', type=Path, required=True)
    parser.add_argument('--ime-client', type=Path, required=True)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--mode', choices=['normal', 'block', 'chat'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location('native_smoke', args.runner)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    smoke = module.Smoke(args.binary.resolve(strict=True), 'wayland', out)
    smoke.env['EMBER_SMOKE_CAPTURE'] = str(Path(__file__).with_name('native-ime-pty-capture.py').resolve())
    keyboard_spec = importlib.util.spec_from_file_location('ime_keyboard', Path(__file__).with_name('native-ime-keyboard.py'))
    keyboard_module = importlib.util.module_from_spec(keyboard_spec)
    keyboard_spec.loader.exec_module(keyboard_module)
    keyboard = None
    result = {'status': 'running', 'mode': args.mode,
              'binary_sha256': hashlib.sha256(smoke.binary.read_bytes()).hexdigest(),
              'helper_sha256': hashlib.sha256(args.ime_client.resolve(strict=True).read_bytes()).hexdigest(),
              'mechanism': 'input-method-v2 keyboard grab and compositor preedit/commit requests',
              'excluded': ['X11 XIM', 'full IBus/Fcitx engine behavior', 'physical-device IME toggle',
                           'asserted visual preedit rendering', 'live mode switching']}
    try:
        smoke.start_desktop()
        smoke.spawn(['wtype', '-s', '600000', '-k', 'F24'], 'ime-seat-keyboard')
        time.sleep(0.5)
        control = out / 'ime-control'
        control.mkdir()
        ime = smoke.spawn([str(args.ime_client.resolve(strict=True)), str(control)], 'ime-client')
        def wait_ime_marker(name, label):
            def ready():
                status = ime.poll()
                if status is not None:
                    raise RuntimeError(f'IME helper exited {status} while awaiting {name}; see ime-client.log')
                return (control / name).exists()
            module.wait_for(ready, label)
        wait_ime_marker('connected', 'IME protocol connected')
        smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "' + args.mode + '"\nrestore_session = false\n')
        smoke.app = smoke.spawn([str(smoke.binary)], 'ember')
        module.wait_for(smoke.app_window, 'installed Ember IME window')
        time.sleep(2)
        identity = smoke.command('ime-ready')
        capture = smoke.raw_start('ime-return-' + args.mode)
        assert (capture / 'live.bin').read_bytes() == b'', 'Bytes before keyboard readiness'
        keyboard = keyboard_module.CompositionKeyboard(smoke)
        module.wait_for(keyboard.ready, 'owned keyboard Shift release roundtrip')
        assert (capture / 'live.bin').read_bytes() == b'', 'Modifier readiness leaked PTY bytes'
        result['keyboard_arguments'] = keyboard_module.KEYBOARD_ARGS
        result['modifier_roundtrip_verified'] = True
        (control / 'start').touch()
        wait_ime_marker('preedit', 'genuine compositor preedit')
        time.sleep(0.5)
        smoke.screenshot('ime-preedit-zero-bytes')
        preedit = (capture / 'live.bin').read_bytes()
        assert preedit == b'', ('preedit leaked terminal bytes', preedit.hex())
        (capture / 'preedit.bin').write_bytes(preedit)
        keyboard.release_return(control / 'preedit', capture)
        wait_ime_marker('committed', 'native composition Return consumed and grab released')
        expected = '你'.encode()
        module.wait_for(lambda: (capture / 'live.bin').read_bytes() == expected, 'commit text reaches PTY without CR')
        smoke.screenshot('ime-commit-without-cr')
        smoke.raw_finish(capture, identity, expected)
        (control / 'finish').touch()
        ime.wait(timeout=5)
        assert ime.returncode == 0
        assert keyboard.process.poll() is None, 'Keyboard must stay alive through commit/release'
        keyboard.stop()
        assert smoke.command('ime-input-resumed') == identity
        lines = smoke.transcript.read_text().splitlines()
        assert len(lines) == 2 and [line.split(':', 1)[0] for line in lines] == ['ime-ready', 'ime-input-resumed'], lines
        smoke.screenshot('ime-real-enter-resumed')
        result.update(status='pass', identity=identity, preedit_bytes=0,
                      preedit_evidence='compositor request completed; screenshot retained without render assertion',
                      committed_hex=expected.hex(), composition_return_cr_bytes=0,
                      resumed_submission_count=1)
    except Exception as error:
        result.update(status='fail', error=str(error))
        try:
            smoke.screenshot('failure')
        except Exception:
            pass
        raise
    finally:
        if 'ime' in locals():
            result['helper_exit_code_before_cleanup'] = ime.poll()
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        try:
            try:
                if keyboard is not None:
                    keyboard.stop()
            finally:
                smoke.close()
        except Exception as error:
            result.update(status='fail', cleanup_error=str(error))
            (out / 'result.json').write_text(json.dumps(result, indent=2))
            raise


if __name__ == '__main__':
    main()
