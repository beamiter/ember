#!/usr/bin/env python3
"""PROPOSAL ONLY: paired instrumented main-entry to first-painted-frame benchmark."""
import argparse
import ctypes
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import random
import re
import signal
import statistics
import subprocess
import sys
import time
import tempfile

MARKER = re.compile(r'^EMBER_BENCHMARK_MAIN_TO_FIRST_PAINT_NS=(\d+)$', re.M)
RESTORED = '[SessionPersistence] Restored 64 sessions'
APPROVED_BASELINE = '1c43338c0292f40fd294cb89d243bae356d64965'
APPROVED_CANDIDATE = '60e70c4a24d52e41d173c4baf26bf06bf3bd1569'
APPROVED_OVERLAY = '74c3daf63089139cf927db14a83f1979e071efda3791841d6a6ae942b701ebe8'


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def sample_helper(request_path):
    # This dedicated helper owns only one app tree, never the shared desktop.
    # Subreaping keeps orphaned/double-forked descendants within its ownership.
    import psutil
    request = json.loads(request_path.read_text())
    result = {'status': 'fail', 'cleanup_verified': False}
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), 'Could not establish sample child ownership')
    owner = psutil.Process()

    def interrupted(signum, _frame):
        raise RuntimeError(f'Sample interrupted by signal {signum}')

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    log_path = Path(request['log'])
    app = None
    try:
        with log_path.open('wb') as log:
            app = subprocess.Popen([request['binary']], stdout=log, stderr=log)
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                if app.poll() is not None:
                    raise RuntimeError(f'Application exited before readiness: {app.returncode}')
                text = log_path.read_text(errors='replace')
                markers = list(MARKER.finditer(text))
                if len(markers) > 1:
                    raise RuntimeError('Duplicate first-paint marker')
                if markers:
                    marker = markers[0]
                    if text.count(RESTORED) != 1 or text.find(RESTORED) > marker.start():
                        raise RuntimeError('Exactly 64 successful restores must be logged before first paint')
                    descendants = owner.children(recursive=True)
                    bash_path = os.path.realpath('/bin/bash')
                    shells = []
                    for child in descendants:
                        try:
                            if os.path.realpath(child.exe()) != bash_path:
                                continue
                            tty = os.readlink(f'/proc/{child.pid}/fd/0')
                            if not re.fullmatch(r'/dev/pts/\d+', tty):
                                raise RuntimeError('Restored Bash has no PTY on stdin')
                            shells.append({'pid': child.pid, 'created': child.create_time(), 'pty': tty})
                        except psutil.NoSuchProcess:
                            raise RuntimeError('A restored shell exited during readiness evidence capture')
                    if len(shells) != 64 or len({x['pty'] for x in shells}) != 64:
                        raise RuntimeError(f'Expected 64 live Bash identities on distinct PTYs, got {len(shells)}')
                    result.update(status='pass', elapsed_ns=int(marker.group(1)),
                                  restore_diagnostic_before_marker=True, restored_shells=shells,
                                  descendants=[{'pid': p.pid, 'created': p.create_time(), 'name': p.name()}
                                               for p in owner.children(recursive=True)])
                    # Readiness timing has already been sampled in the app.
                    # Optional screenshot is correctness evidence, never a clock.
                    if request['screenshot']:
                        subprocess.run(request['screenshot'], check=True, timeout=10)
                    # The capture command may outlive an app crash or a second
                    # marker. Validate the final observed log and process before
                    # intentional cleanup, and retain the pre-cleanup status.
                    final_text = log_path.read_text(errors='replace')
                    final_markers = list(MARKER.finditer(final_text))
                    result['app_exit_before_cleanup'] = app.poll()
                    if result['app_exit_before_cleanup'] is not None:
                        raise RuntimeError('Application exited during readiness capture')
                    if len(final_markers) != 1 or final_text.count(RESTORED) != 1 or final_text.find(RESTORED) > final_markers[0].start():
                        raise RuntimeError('Final readiness markers were duplicated or out of order')
                    if int(final_markers[0].group(1)) != result['elapsed_ns']:
                        raise RuntimeError('Readiness marker changed during capture')
                    break
                time.sleep(0.01)
            else:
                raise TimeoutError('No first-painted-frame marker within 60 seconds')
    except BaseException as error:
        result.update(status='fail', error=str(error))
    finally:
        # Repeated signals must not interrupt the bounded owned-tree drain.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        if app is not None:
            result['app_exit_before_cleanup'] = app.poll()
            if result['app_exit_before_cleanup'] is not None:
                result.update(status='fail', error='Application exited before intentional cleanup')
        cleanup = drain_children(owner, 10, app)
        result['cleanup_verified'] = cleanup['empty']
        result['cleanup'] = cleanup
        if app is not None:
            result['app_exit_after_cleanup'] = app.poll()
            if result['app_exit_after_cleanup'] is None or result['app_exit_after_cleanup'] >= 0:
                result.update(status='fail', error='Application was not alive until intentional signal cleanup')
            final_text = log_path.read_text(errors='replace')
            final_markers = list(MARKER.finditer(final_text))
            if len(final_markers) != 1 or final_text.count(RESTORED) != 1 or final_text.find(RESTORED) > final_markers[0].start():
                result.update(status='fail', error='Final log markers invalid after cleanup')
        if not cleanup['empty']:
            result.update(status='fail', cleanup_error='Owned descendants remain after bounded cleanup')
        Path(request['result']).write_text(json.dumps(result, indent=2))
    return 0 if result['status'] == 'pass' else 1


def drain_children(owner, seconds, reap=None):
    """Iteratively kill/reap this exclusive subreaper's complete owned tree."""
    import psutil
    deadline = time.monotonic() + seconds
    identities = {}
    while time.monotonic() < deadline:
        if reap is not None: reap.poll()
        children = owner.children(recursive=True)
        if not children:
            return {'empty': True, 'owned': list(identities.values()), 'remaining': []}
        for child in children:
            try:
                identity = {'pid': child.pid, 'created': child.create_time()}
                identities[(child.pid, identity['created'])] = identity
                child.kill()
            except psutil.NoSuchProcess:
                pass
        # Let Popen reap its own child so its real exit code is retained.
        waiting = [child for child in children if reap is None or child.pid != reap.pid]
        psutil.wait_procs(waiting, timeout=min(.25, max(0, deadline - time.monotonic())))
        if reap is not None: reap.poll()
    remaining = []
    for child in owner.children(recursive=True):
        try: remaining.append({'pid': child.pid, 'created': child.create_time()})
        except psutil.NoSuchProcess: pass
    return {'empty': not remaining, 'owned': list(identities.values()), 'remaining': remaining}


def sample_supervisor(request_path):
    # This exclusive outer subreaper has no compositor or unrelated children.
    # Unknown descendants orphaned by abrupt helper exit are adopted here.
    import psutil
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), 'Could not establish outer sample ownership')
    result = {'status': 'fail', 'helper_returncode': None}
    proc = None

    def interrupted(signum, _frame):
        raise RuntimeError(f'Supervisor interrupted by signal {signum}')

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        proc = subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
                                 '--sample-helper', str(request_path)])
        deadline = time.monotonic() + 85
        while proc.poll() is None and time.monotonic() < deadline:
            time.sleep(.025)
        result['helper_returncode'] = proc.poll()
        if result['helper_returncode'] is None:
            raise TimeoutError('Sample helper exceeded 85-second watchdog')
        if result['helper_returncode'] != 0:
            raise RuntimeError('Sample helper failed')
        # Any child left after the helper exits rejects the sample, even if
        # the subsequent cleanup succeeds.
        if psutil.Process().children(recursive=True):
            raise RuntimeError('Sample helper exited with owned descendants')
        result['status'] = 'pass'
    except BaseException as error:
        result.update(status='fail', error=str(error))
    finally:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        result['cleanup'] = drain_children(psutil.Process(), 15, proc)
        if proc is not None:
            result['helper_returncode'] = proc.poll()
        if not result['cleanup']['empty']:
            result.update(status='fail', cleanup_error='Supervisor owned tree not empty')
        (request_path.parent / 'supervisor.json').write_text(json.dumps(result, indent=2))
    return 0 if result['status'] == 'pass' else 1


def wait_supervisor(proc, sample):
    """Bounded parent wait; interruption gives the supervisor cleanup time."""
    import psutil
    try:
        return proc.wait(timeout=110)
    except BaseException:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        # Do not kill the subreaper before its owned children are drained.
        try: proc.terminate()
        except ProcessLookupError: pass
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            supervisor = psutil.Process(proc.pid)
            # Last-resort cleanup freezes the supervisor and its full subtree
            # repeatedly to stop forks, then kills descendants before owner.
            owned = {}
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                try: supervisor.suspend()
                except psutil.NoSuchProcess: break
                children = supervisor.children(recursive=True)
                for child in children:
                    try:
                        owned[(child.pid, child.create_time())] = child
                        child.suspend()
                        child.kill()
                    except psutil.NoSuchProcess: pass
                live = []
                for child in supervisor.children(recursive=True):
                    try:
                        if child.status() != psutil.STATUS_ZOMBIE: live.append(child)
                    except psutil.NoSuchProcess: pass
                if not live: break
                time.sleep(.025)
            remaining = []
            for child in supervisor.children(recursive=True):
                try:
                    if child.status() != psutil.STATUS_ZOMBIE:
                        remaining.append({'pid': child.pid, 'created': child.create_time()})
                except psutil.NoSuchProcess: pass
            evidence = {'owned': [{'pid': key[0], 'created': key[1]} for key in owned],
                        'remaining': remaining}
            (sample / 'parent-emergency-cleanup.json').write_text(json.dumps(evidence, indent=2))
            if remaining:
                # Keep the subreaper alive and resume it so it can finish its
                # own cleanup; never knowingly orphan live descendants.
                supervisor.resume()
                raise RuntimeError('Parent cleanup failed with live owned descendants; evidence retained')
            supervisor.kill()
            proc.wait(timeout=5)
        raise


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]


def summarize(pairs):
    deltas = [(pair['candidate'] - pair['baseline']) / 1e6 for pair in pairs]
    rng = random.Random(0)
    means = [statistics.mean(rng.choices(deltas, k=len(deltas))) for _ in range(10000)]
    return {'paired_delta_ms': deltas, 'mean_delta_ms': statistics.mean(deltas),
            'median_delta_ms': statistics.median(deltas), 'p95_delta_ms': percentile(deltas, .95),
            'max_delta_ms': max(deltas), 'stdev_delta_ms': statistics.stdev(deltas),
            'mean_delta_bootstrap_95ci_ms': [percentile(means, .025), percentile(means, .975)],
            'baseline_p95_ms': percentile([p['baseline'] / 1e6 for p in pairs], .95),
            'candidate_p95_ms': percentile([p['candidate'] / 1e6 for p in pairs], .95)}


def run(args):
    # The outer owner has only this backend's dedicated desktop and samples.
    # Abrupt supervisor exit reparents descendants here. Any failed sample
    # stops this backend, closes its desktop, then drains the entire owned tree.
    # SIGKILL of this outer runner itself cannot execute Python cleanup.
    import psutil
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), 'Could not establish backend ownership')
    if (args.baseline_sha, args.candidate_sha, args.overlay_sha) != (APPROVED_BASELINE, APPROVED_CANDIDATE, APPROVED_OVERLAY):
        raise ValueError('Benchmark identities differ from reviewed immutable inputs')
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
    result = {'interval': 'first main statement to existing first-painted-frame branch',
              'not_measured': ['OS process launch', 'compositor scanout'],
              'baseline_source': args.baseline_sha, 'candidate_source': args.candidate_sha,
              'baseline_binary_sha256': digest(baseline), 'candidate_binary_sha256': digest(candidate),
              'overlay_sha256': args.overlay_sha, 'fixture_sha256': hashlib.sha256(fixture).hexdigest(),
              'backend': args.backend, 'warmup_pairs': 5, 'measured_pairs': 30,
              'reporting_unit': 'nanoseconds; actual clock resolution not inferred from unit',
              'watchdog_clock': vars(time.get_clock_info('monotonic')), 'samples': [], 'status': 'running'}
    pairs = []
    try:
        desktop.start_desktop()
        for pair_index in range(35):
            pair = {}
            order = [('baseline', baseline), ('candidate', candidate)]
            if pair_index % 2: order.reverse()
            for label, binary in order:
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
                attempt = dict(pair=pair_index, label=label, warmup=pair_index < 5,
                               status='pending', supervisor_returncode=None, sample=str(sample))
                result['samples'].append(attempt)
                (out / 'result.json').write_text(json.dumps(result, indent=2))
                proc = None
                try:
                    with (sample / 'helper.log').open('wb') as helper_log:
                        proc = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), '--sample-supervisor', str(request_path)],
                                                env=env, stdout=helper_log, stderr=subprocess.STDOUT)
                        attempt['supervisor_returncode'] = wait_supervisor(proc, sample)
                    observed = json.loads((sample / 'result.json').read_text())
                    supervisor = json.loads((sample / 'supervisor.json').read_text())
                    attempt.update(observed=observed, supervisor=supervisor)
                    if attempt['supervisor_returncode'] or observed['status'] != 'pass' or not observed['cleanup_verified'] or supervisor['status'] != 'pass' or not supervisor['cleanup']['empty']:
                        raise RuntimeError(f'Failed sample {pair_index}/{label}; not retried or discarded')
                    pair[label] = observed['elapsed_ns']
                    attempt['status'] = 'pass'
                except BaseException as error:
                    attempt.update(status='fail', error=str(error))
                    raise
                finally:
                    if proc is not None: attempt['supervisor_returncode'] = proc.poll()
                    (sample / 'attempt.json').write_text(json.dumps(attempt, indent=2))
                    (out / 'result.json').write_text(json.dumps(result, indent=2))
            if pair_index >= 5: pairs.append(pair)
        result.update(summarize(pairs))
        upper = result['mean_delta_bootstrap_95ci_ms'][1]
        result['status'] = 'pass' if upper < 50 and result['p95_delta_ms'] < 50 else 'inconclusive-or-over-budget'
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
    if len(sys.argv) == 3 and sys.argv[1] == '--sample-supervisor':
        return sample_supervisor(Path(sys.argv[2]))
    if len(sys.argv) == 3 and sys.argv[1] == '--sample-helper':
        return sample_helper(Path(sys.argv[2]))
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ['baseline', 'candidate', 'driver', 'output']:
        parser.add_argument('--' + option, type=Path, required=True)
    for option in ['baseline-sha', 'candidate-sha', 'overlay-sha']:
        parser.add_argument('--' + option, required=True)
    parser.add_argument('--backend', choices=['x11', 'wayland'], required=True)
    return run(parser.parse_args())


if __name__ == '__main__':
    raise SystemExit(main())
