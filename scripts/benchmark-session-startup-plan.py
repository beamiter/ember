#!/usr/bin/env python3
"""Fixed, reviewable cumulative startup experiment. Never a shipped app change."""
import argparse
import ctypes
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time

HELPER_SHA = '19967416389d166f427aaa5b0337e31e8762dd4ca1d03b6d14cc1ef7661d620c'
LEGACY_PATH = Path(__file__).resolve().with_name('benchmark-session-startup.py')
# Retain the original helper byte-for-byte: direct-work also pins its hash.
if hashlib.sha256(LEGACY_PATH.read_bytes()).hexdigest() != HELPER_SHA:
    raise ValueError('Legacy sampling/cleanup/statistics helper hash mismatch')
spec = importlib.util.spec_from_file_location('pinned_startup_helper', LEGACY_PATH)
legacy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(legacy)
digest = legacy.digest
summarize = legacy.summarize
wait_supervisor = legacy.wait_supervisor
drain_children = legacy.drain_children


def read_plan(path):
    plan = json.loads(path.read_text())
    keys = ('baseline_sha', 'candidate_sha', 'driver_control_sha', 'warmup_pairs',
            'measured_pairs', 'budget_ms', 'bootstrap_resamples', 'bootstrap_seed',
            'overlay_sha256', 'legacy_helper_sha256')
    expected = ('1c43338c0292f40fd294cb89d243bae356d64965',
                '9607c1cd4c4f623ed383a7f1a7d7ebb07a22b5e7',
                'e4c292f10463e52fe7e74e808ca10fc4ab27729c',
                5, 240, 50, 10000, 0,
                '74c3daf63089139cf927db14a83f1979e071efda3791841d6a6ae942b701ebe8',
                HELPER_SHA)
    numeric = ('schema', 'warmup_pairs', 'measured_pairs', 'budget_ms',
               'bootstrap_resamples', 'bootstrap_seed')
    if (any(type(plan.get(k)) is not int for k in numeric)
            or plan.get('schema') != 1
            or tuple(plan.get(k) for k in keys) != expected):
        raise ValueError('Plan differs from the reviewed immutable experiment')
    if not plan.get('scope') or not plan.get('endpoint'):
        raise ValueError('Plan must disclose scope and endpoint')
    return plan


def verify_inputs(plan, driver_root, overlay):
    expected_paths = {'scripts/native-desktop-smoke.py', 'scripts/native-pty-capture.py',
                      'scripts/native-pointer.c',
                      'scripts/protocols/wlr-virtual-pointer-unstable-v1.xml'}
    if set(plan.get('driver_files', {})) != expected_paths:
        raise ValueError('Driver dependency closure differs from reviewed files')
    for relative, expected in plan['driver_files'].items():
        if digest(driver_root / relative) != expected:
            raise ValueError('Driver dependency hash mismatch: ' + relative)
    if digest(overlay) != plan['overlay_sha256']:
        raise ValueError('Overlay hash mismatch')


def schedule(plan):
    for index in range(plan['warmup_pairs'] + plan['measured_pairs']):
        order = ('baseline', 'candidate') if index % 2 == 0 else ('candidate', 'baseline')
        yield index, index < plan['warmup_pairs'], order


def validate_attempt(returncode, observed, supervisor):
    if (returncode != 0 or observed.get('status') != 'pass'
            or observed.get('cleanup_verified') is not True
            or supervisor.get('status') != 'pass'
            or supervisor.get('cleanup', {}).get('empty') is not True):
        raise RuntimeError('Failed sample; not retried or discarded')


def evaluate(pairs, expected_count):
    if len(pairs) != expected_count:
        raise ValueError('Incomplete fixed-size experiment; never retry or drop samples')
    if any(set(pair) != {'baseline', 'candidate'} or
           any(type(v) is not int or v <= 0 for v in pair.values()) for pair in pairs):
        raise ValueError('Each pair needs two positive integer timing observations')
    result = summarize(pairs)
    result['status'] = ('pass' if result['mean_delta_bootstrap_95ci_ms'][1] < 50
                        and result['p95_delta_ms'] < 50 else 'inconclusive-or-over-budget')
    return result


def run(args):
    # The outer owner has only this backend's dedicated desktop and samples.
    # Abrupt supervisor exit reparents descendants here. Any failed sample
    # stops this backend, closes its desktop, then drains the entire owned tree.
    # SIGKILL of this outer runner itself cannot execute Python cleanup.
    plan = read_plan(args.plan)
    verify_inputs(plan, args.driver_root, args.overlay)
    args.driver = args.driver_root / 'scripts/native-desktop-smoke.py'
    import psutil
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), 'Could not establish backend ownership')
    def interrupted(signum, _frame):
        raise RuntimeError(f'Runner interrupted by signal {signum}')
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('desktop_smoke', args.driver.resolve())
    driver = importlib.util.module_from_spec(spec); spec.loader.exec_module(driver)
    desktop_out = out / 'desktop'; desktop_out.mkdir()
    desktop = driver.Smoke(args.baseline.resolve(), args.backend, desktop_out)
    runtime = tempfile.TemporaryDirectory(prefix='ember-b-', dir='/tmp')
    desktop.env['XDG_RUNTIME_DIR'] = runtime.name
    baseline, candidate = args.baseline.resolve(strict=True), args.candidate.resolve(strict=True)
    fixture_cwd = out / 'cwd'; fixture_cwd.mkdir()
    fixture = json.dumps({'version': 4, 'sessions': [
        {'name': f'fixed-{i}', 'tags': [], 'cwd': str(fixture_cwd), 'session_id': f'bench-{i}'}
        for i in range(64)]}, separators=(',', ':')).encode()
    (out / 'fixture.json').write_bytes(fixture)
    result = {'interval': plan['endpoint'], 'scope': plan['scope'], 'plan': plan,
              'plan_sha256': digest(args.plan), 'control_source': args.control_sha,
              'legacy_helper_sha256': HELPER_SHA,
              'not_measured': ['OS process launch', 'compositor scanout'],
              'baseline_source': plan['baseline_sha'], 'candidate_source': plan['candidate_sha'],
              'baseline_binary_sha256': digest(baseline), 'candidate_binary_sha256': digest(candidate),
              'overlay_sha256': plan['overlay_sha256'], 'fixture_sha256': hashlib.sha256(fixture).hexdigest(),
              'backend': args.backend, 'warmup_pairs': plan['warmup_pairs'], 'measured_pairs': plan['measured_pairs'],
              'reporting_unit': 'nanoseconds; actual clock resolution not inferred from unit',
              'watchdog_clock': vars(time.get_clock_info('monotonic')), 'samples': [], 'status': 'running'}
    pairs = []
    try:
        desktop.start_desktop()
        binaries = {'baseline': baseline, 'candidate': candidate}
        for pair_index, warmup, order in schedule(plan):
            pair = {}
            for label in order:
                binary = binaries[label]
                sample = out / f'{pair_index:02}-{label}'; sample.mkdir()
                env = desktop.env.copy(); home = sample / 'home'; home.mkdir()
                for key, suffix in [('HOME',''), ('XDG_CONFIG_HOME','.config'), ('XDG_DATA_HOME','.local/share'),
                                    ('XDG_STATE_HOME','.local/state'), ('XDG_CACHE_HOME','.cache')]:
                    env[key] = str(home / suffix)
                config = home / '.config/ember'; config.mkdir(parents=True)
                (config / 'session_history.json').write_bytes(fixture)
                (config / 'session_history.json').chmod(0o600)
                (config / 'config.toml').write_text('shell = "/bin/bash"\nterminal_mode = "normal"\nrestore_session = true\ninitial_width = 1100\ninitial_height = 800\n')
                (home / '.bash_profile').write_text("export HISTFILE=/dev/null\nPS1='BENCHMARK> '\n")
                screenshot = ['import', '-window', 'root', str(sample / 'first-painted.png')] if args.backend == 'x11' else ['grim', str(sample / 'first-painted.png')]
                request = {'binary': str(binary), 'log': str(sample / 'app.log'),
                           'result': str(sample / 'result.json'), 'screenshot': screenshot}
                request_path = sample / 'request.json'; request_path.write_text(json.dumps(request))
                attempt = dict(pair=pair_index, label=label, warmup=warmup,
                               status='pending', supervisor_returncode=None, sample=str(sample))
                result['samples'].append(attempt)
                (out / 'result.json').write_text(json.dumps(result, indent=2))
                proc = None
                try:
                    with (sample / 'helper.log').open('wb') as helper_log:
                        proc = subprocess.Popen([sys.executable, str(LEGACY_PATH), '--sample-supervisor', str(request_path)],
                                                env=env, stdout=helper_log, stderr=subprocess.STDOUT)
                        attempt['supervisor_returncode'] = wait_supervisor(proc, sample)
                    observed = json.loads((sample / 'result.json').read_text())
                    supervisor = json.loads((sample / 'supervisor.json').read_text())
                    attempt.update(observed=observed, supervisor=supervisor)
                    validate_attempt(attempt['supervisor_returncode'], observed, supervisor)
                    pair[label] = observed['elapsed_ns']
                    attempt['status'] = 'pass'
                except BaseException as error:
                    attempt.update(status='fail', error=str(error))
                    raise
                finally:
                    if proc is not None: attempt['supervisor_returncode'] = proc.poll()
                    (sample / 'attempt.json').write_text(json.dumps(attempt, indent=2))
                    (out / 'result.json').write_text(json.dumps(result, indent=2))
            if not warmup: pairs.append(pair)
        result.update(evaluate(pairs, plan['measured_pairs']))
    except BaseException as error:
        result.update(status='fail', error=str(error))
        raise
    finally:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        try:
            desktop.close()
        except BaseException as error:
            result.update(status='fail', desktop_cleanup_error=str(error))
        finally:
            result['backend_cleanup'] = drain_children(psutil.Process(), 15)
            if not result['backend_cleanup']['empty']:
                result.update(status='fail', cleanup_error='Backend owned tree not empty')
            try:
                runtime.cleanup()
            finally:
                (out / 'result.json').write_text(json.dumps(result, indent=2))
    return 0 if result['status'] == 'pass' else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ['baseline', 'candidate', 'driver-root', 'overlay', 'plan', 'output']:
        parser.add_argument('--' + option, type=Path, required=True)
    parser.add_argument('--control-sha', required=True)
    parser.add_argument('--backend', choices=['x11', 'wayland'], required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9a-f]{40}', args.control_sha):
        parser.error('control-sha must be an immutable full commit SHA')
    return run(args)


if __name__ == '__main__':
    raise SystemExit(main())
