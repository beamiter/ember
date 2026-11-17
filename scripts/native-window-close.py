#!/usr/bin/env python3
"""Exercise native whole-window confirmation with a real job in a hidden tab."""
import argparse
import csv
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import select
import time
import sys
if sys.version_info < (3, 11):
    import tomli
    sys.modules["tomllib"] = tomli
from PIL import Image, ImageOps


def wait(check, label, seconds=25):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(.1)
    raise AssertionError('Timed out: ' + label)


def live(pid):
    try:
        return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[0] not in ('Z', 'X')
    except FileNotFoundError:
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--eva', choices=('on', 'off'), required=True)
    args = parser.parse_args()
    os.umask(0o077)
    repo, out = args.repo.resolve(), args.output.resolve()
    import tomllib
    app_name = tomllib.loads((repo / 'Cargo.toml').read_text())['package']['name']
    out.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('native', repo / 'scripts/native-desktop-smoke.py')
    native = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(native)
    smoke = native.Smoke(args.binary.resolve(), 'x11', out)
    smoke.env['EVA_COMPANION_SOCKET'] = smoke.env['XDG_RUNTIME_DIR'] + '/missing-eva.sock'
    smoke.env.pop('EVA_COMPANION_MODE', None)
    smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\n'
                            'initial_width = 1100\ninitial_height = 800\nrestore_session = false\n'
                            f'eva_companion_enabled = {str(args.eva == "on").lower()}\n'
                            + ('app_renderer = "glow"\n' if app_name == 'ember' else ''))
    result = dict(app=app_name, eva=args.eva)
    observation = 0

    def rows(label):
        nonlocal observation
        observation += 1
        name = f'{observation:03}-{label}'
        smoke.screenshot(name)
        image = Image.open(out / (name + '.png')).convert('RGB')
        # A fixed threshold preserves white text on Frost's red close button;
        # retain the original screenshot as the native observation.
        prepared = ImageOps.invert(ImageOps.grayscale(image)).point(lambda value: 255 if value > 130 else 0).resize((image.width*2, image.height*2))
        prepared.save(out / (name + '-ocr.png'))
        env = dict(smoke.env, OMP_THREAD_LIMIT='1')
        text = subprocess.check_output(['tesseract', str(out / (name + '-ocr.png')), 'stdout', '--psm', '11', 'tsv'],
                                       env=env, text=True, stderr=subprocess.DEVNULL, timeout=30)
        (out / (name + '.tsv')).write_text(text)
        return [r for r in csv.DictReader(io.StringIO(text), delimiter='\t') if r['text'].strip()]

    def button(words, phrase):
        target = phrase.lower().split()
        for index in range(len(words) - len(target) + 1):
            chunk = words[index:index+len(target)]
            if [r['text'].lower() for r in chunk] == target and len({(r['block_num'], r['par_num'], r['line_num']) for r in chunk}) == 1:
                left = min(int(r['left']) for r in chunk)
                right = max(int(r['left']) + int(r['width']) for r in chunk)
                top = min(int(r['top']) for r in chunk)
                bottom = max(int(r['top']) + int(r['height']) for r in chunk)
                return ((left+right)//4, (top+bottom)//4)

    def observe_button(label, phrase):
        return wait(lambda: button(rows(label), phrase), label)

    def click(point):
        smoke.run(['xdotool', 'mousemove', '--sync', str(point[0]), str(point[1]), 'click', '1'])

    def request_close():
        smoke.key('F4', ('alt',))
        return observe_button('terminal-confirmation', 'Close window')

    try:
        xvfb_log = (out / 'xvfb.log').open('w')
        smoke.logs.append(xvfb_log)
        xvfb = subprocess.Popen(['Xvfb', '-displayfd', '1', '-screen', '0', '1280x900x24', '-nolisten', 'tcp'],
                                env=smoke.env, stdout=subprocess.PIPE, stderr=xvfb_log, text=True)
        smoke.processes.append(xvfb)
        assert select.select([xvfb.stdout], [], [], 15)[0], 'Xvfb did not allocate display'
        number = xvfb.stdout.readline().strip()
        assert number.isdigit() and xvfb.poll() is None, 'Xvfb exited before readiness'
        smoke.env['DISPLAY'] = ':' + number
        smoke.spawn(['openbox', '--sm-disable'], 'openbox')
        time.sleep(.5)
        app = smoke.app = smoke.spawn([str(args.binary.resolve())], app_name)
        wait(smoke.app_window, 'native app window')
        time.sleep(1)
        marker = out / 'shell.pid'
        smoke.text(f'printf "%s\\n" "$$" > {marker}; sleep 120')
        smoke.key('Return')
        shell = int(wait(lambda: marker.read_text().strip() if marker.exists() else None, 'shell identity'))
        children_path = Path(f'/proc/{shell}/task/{shell}/children')
        job = int(wait(lambda: next((p for p in children_path.read_text().split()
                                    if Path(f'/proc/{p}/comm').read_text().strip() == 'sleep'), None), 'real sleep child'))
        result.update(shell_pid=shell, job_pid=job)
        assert os.getpgid(job) != os.getpgid(shell), 'job must own a separate foreground group'
        smoke.key('t', ('ctrl', 'shift'))
        time.sleep(.5)
        point = request_close()
        assert app.poll() is None and live(job)
        smoke.key('c', ('ctrl',))
        time.sleep(.3)
        assert live(job), 'modal leaked Ctrl+C into the foreground job'
        click(observe_button('cancel-button', 'Cancel'))
        time.sleep(.3)
        assert app.poll() is None and live(job), 'Cancel terminated work'
        result['cancel_preserves_job'] = True
        request_close()
        smoke.key('Escape')
        time.sleep(.3)
        assert app.poll() is None and live(job), 'Escape terminated work'
        result['escape_preserves_job'] = True
        point = request_close()
        click(point)
        if args.eva == 'on':
            click(observe_button('eva-keep-open', 'Keep window open'))
            time.sleep(.3)
            assert app.poll() is None and live(job), 'Keep window open terminated work'
            # Cancelling Eva withdrawal revokes the earlier terminal approval.
            click(request_close())
            result['eva_cancel_revokes_terminal_approval'] = True
            click(observe_button('eva-force-close', 'Close anyway'))
        wait(lambda: app.poll() is not None, 'confirmed app exit', 15)
        assert app.returncode == 0, f'app exited with {app.returncode}'
        wait(lambda: not live(job), 'confirmed close terminates real foreground job', 10)
        result.update(confirmed_exit=True, job_terminated=True, hidden_tab_checked=True, modal_blocks_sigint=True, status='pass')
    except BaseException as error:
        result.update(status='fail', error=str(error))
        try:
            smoke.screenshot('failure')
        except Exception:
            pass
        raise
    finally:
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        smoke.close()
    print(json.dumps(result))


if __name__ == '__main__':
    main()
