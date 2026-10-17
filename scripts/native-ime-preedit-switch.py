#!/usr/bin/env python3
"""Hosted-only Settings switches with a real compositor preedit already active."""
import argparse
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import time
import tomllib
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


class FreshVisual:
    """Bounded fresh-cycle health handoff from the completed transition."""
    def __init__(self, visual, smoke, segment, identity, mode):
        if (segment.get('status') != 'pass' or segment.get('identity') != identity
                or segment.get('to') != mode or segment.get('pty_bytes') != 0):
            raise RuntimeError('Unverified transition health handoff')
        self.visual, self.smoke = visual, smoke
        self.identity, self.mode = identity, mode
        self.prior = copy.deepcopy(segment['after_challenge_visual'])
        self.image = smoke.out / f"preedit-{segment['from']}-to-{mode}" / 'after-challenge.png'
        self.used = False
        self.initial = None
        self.phase = 0
        self.view = None
        # Destination mode selects layout before this cycle captures anything.
        self.preedit_psm = 11 if mode == 'normal' else 6

    def snapshot(self, view, label, baseline, present):
        diagnostic = {'label': label, 'expected_token_present': present,
                      'fresh_cycle_phase': self.phase, 'status': 'running'}
        try:
            result = self._snapshot(view, label, baseline, present)
            diagnostic.update(status='pass', image_sha256=result['image_sha256'],
                              health_ocr_psm=result['health_ocr_psm'])
            return result
        except Exception as error:
            diagnostic.update(status='fail', error=str(error))
            raise
        finally:
            (view.out / 'ime-fresh-visual-phase.json').write_text(json.dumps(diagnostic, indent=2))

    def _snapshot(self, view, label, baseline, present):
        if baseline is not None:
            expected = [('ime-preedit-zero-bytes', True), ('ime-commit-without-cr', False)]
            if (self.initial is None or self.phase >= len(expected)
                    or (label, present) != expected[self.phase] or baseline != self.initial
                    or view is not self.view or view.app is not self.smoke.app
                    or self.smoke.app.poll() is not None
                    or baseline['rectangle']['pid'] != self.smoke.app.pid
                    or tomllib.loads(self.smoke.config.read_text()).get('terminal_mode') != self.mode):
                raise RuntimeError('Unverified fresh visual cycle')
            image = Path(self.initial['image'])
            if (image != view.out / 'ime-visual-before.png'
                    or hashlib.sha256(image.read_bytes()).hexdigest() != self.initial['image_sha256']
                    or json.loads(image.with_name('ime-visual-before-visual.json').read_text()) != self.initial):
                raise RuntimeError('Fresh baseline provenance changed')
            # Same-PNG alternate layout only for the ordered post-commit absence
            # check, after this cycle's own successful rendered preedit control.
            result = self.visual.snapshot(view, label, baseline, present,
                                          health_fallback=self.phase == 1,
                                          token_psm=self.preedit_psm if self.phase == 0 else 6)
            self.phase += 1
            return result
        if self.used or label != 'ime-visual-before' or present:
            raise RuntimeError('Health handoff only permits one initial absence snapshot')
        self.used = True
        prior = self.prior
        if (self.smoke.app.poll() is not None or view.app is not self.smoke.app
                or prior['rectangle']['pid'] != self.smoke.app.pid
                or tomllib.loads(self.smoke.config.read_text()).get('terminal_mode') != self.mode):
            raise RuntimeError('Health handoff app or mode changed')
        if (Path(prior['image']) != self.image
                or prior.get('expected_token_count') != 0
                or prior.get('observed_token_count') != 0 or prior.get('matches') != []):
            raise RuntimeError('Health handoff is not the verified absent-token image')
        if (hashlib.sha256(self.image.read_bytes()).hexdigest() != prior['image_sha256']
                or json.loads(self.image.with_name('after-challenge-visual.json').read_text()) != prior):
            raise RuntimeError('Health handoff image or metadata provenance changed')
        result = self.visual.snapshot(view, label, prior, False, health_fallback=True)
        self.initial = copy.deepcopy(result)
        self.view = view
        (view.out / 'ime-health-handoff.json').write_text(json.dumps({
            'source_image': str(self.image), 'source_image_sha256': prior['image_sha256'],
            'source_rectangle': prior['rectangle'], 'source_anchor_box': prior['anchor_box'],
            'mode': self.mode, 'identity': self.identity, 'preedit_ocr_psm': self.preedit_psm, 'initial_image_sha256': result['image_sha256'],
            'health_ocr_psm': result['health_ocr_psm']}, indent=2))
        return result

    def verify_restoration(self, *args):
        return self.visual.verify_restoration(*args)


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
        verified_after = visual.snapshot(view, 'after-challenge', after, False, health_fallback=True)
        (control / 'finish').touch()
        ime.wait(timeout=5)
        assert ime.returncode == 0
        assert smoke.app.poll() is None and seat.poll() is None
        # Observer has exited before finishing collector; its final drain sees
        # delayed PTY bytes too. This capture deliberately expects zero bytes.
        smoke.raw_finish(capture, identity, b'')
        result.update(status='pass', native_outcome=outcomes[0], pty_bytes=0,
                      after_challenge_visual=verified_after)
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
            smoke, module, keyboard, FreshVisual(visual, smoke, segment, identity, new),
            helper, identity, label, seat)
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
