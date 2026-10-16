#!/usr/bin/env python3
"""Preregistered E09 direct-work enclosing-region experiment; not full-startup acceptance."""
import argparse
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import statistics
import subprocess
import sys
import tempfile
import time

CANDIDATE = '60e70c4a24d52e41d173c4baf26bf06bf3bd1569'
HELPER_SHA = '19967416389d166f427aaa5b0337e31e8762dd4ca1d03b6d14cc1ef7661d620c'
DRIVER_SHA = '15459be5d29042ab08ba7dd1fa67fdd4afd3799d33acbeb8c97446b384aad16f'
OVERLAY_SHA = 'e564f56be1a4b2f2cf8cfb94b767ed6d176eecee5bee0af476444dcc79c3ba79'
PHASE = re.compile(r'^EMBER_DIRECT_PHASE_(\d+)_NS=(\d+) COUNT=(\d+)$', re.M)
QUIET = re.compile(r'^EMBER_DIRECT_QUIET=(\d+)$', re.M)


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def load_pinned(name, path, expected):
    if digest(path) != expected:
        raise ValueError(f'Unreviewed {name} identity')
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def phases(text, elapsed_ns):
    matches = list(PHASE.finditer(text))
    quiet = list(QUIET.finditer(text))
    # Reject malformed additional marker lines as well as duplicate valid ones.
    lines = [line for line in text.splitlines() if line.startswith('EMBER_DIRECT_')]
    if len(matches) != 3 or len(quiet) != 1 or len(lines) != 4:
        raise ValueError('Missing, malformed or duplicate direct-work markers')
    if [int(m[1]) for m in matches] != [0, 1, 2] or quiet[0][1] != '1':
        raise ValueError('Nonquiet state or inconsistent phase ordering')
    main = re.findall(r'^EMBER_BENCHMARK_MAIN_TO_FIRST_PAINT_NS=(\d+)$', text, re.M)
    if len(main) != 1 or int(main[0]) != elapsed_ns:
        raise ValueError('Inconsistent full-frame marker')
    main_position = text.index('EMBER_BENCHMARK_MAIN_TO_FIRST_PAINT_NS=')
    if matches[0].start() < main_position or quiet[0].start() < matches[-1].start():
        raise ValueError('Direct-work markers are out of order')
    counts = [int(m[3]) for m in matches]
    totals = [int(m[2]) for m in matches]
    if counts[:2] != [1, 1] or counts[2] < 1 or any(n <= 0 for n in totals):
        raise ValueError('Phase execution counts or durations invalid')
    total = sum(totals)
    if total > elapsed_ns:
        raise ValueError('Enclosing regions exceed full-frame interval')
    return dict(phase_ns=totals, phase_counts=counts, direct_work_bound_ns=total)


def run(args):
    import psutil
    helper = load_pinned('paired_helper', args.helper.resolve(), HELPER_SHA)
    driver = load_pinned('desktop_driver', args.driver.resolve(), DRIVER_SHA)
    if args.candidate_sha != CANDIDATE or args.overlay_sha != OVERLAY_SHA:
        raise ValueError('Unreviewed source or overlay identity')
    binary = args.candidate.resolve(strict=True)
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    result = dict(status='running', metric='sum of three direct-work enclosing regions',
                  limitation='Does not establish full-startup causal overhead or change the original failed gate',
                  source=CANDIDATE, overlay_sha256=OVERLAY_SHA, helper_sha256=HELPER_SHA,
                  driver_sha256=DRIVER_SHA, binary_sha256=digest(binary), backend=args.backend,
                  warmup_launches=5, measured_launches=60, budget_ns=50_000_000,
                  clock=vars(time.get_clock_info('monotonic')), samples=[])
    (out / 'result.json').write_text(json.dumps(result, indent=2))
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), 'Could not establish backend ownership')

    def interrupted(signum, _frame):
        raise RuntimeError(f'Runner interrupted by signal {signum}')

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    desktop = runtime = None
    try:
        desktop_out = out / 'desktop'; desktop_out.mkdir()
        desktop = driver.Smoke(binary, args.backend, desktop_out)
        runtime = tempfile.TemporaryDirectory(prefix='ember-d-', dir='/tmp')
        desktop.env['XDG_RUNTIME_DIR'] = runtime.name
        cwd = out / 'cwd'; cwd.mkdir()
        fixture = json.dumps({'version': 4, 'sessions': [
            {'name': f'fixed-{i}', 'tags': [], 'cwd': str(cwd), 'session_id': f'bench-{i}'}
            for i in range(64)]}, separators=(',', ':')).encode()
        (out / 'fixture.json').write_bytes(fixture)
        result['fixture_sha256'] = hashlib.sha256(fixture).hexdigest()
        desktop.start_desktop()
        for index in range(65):
            sample = out / f'{index:02}'; sample.mkdir()
            attempt = dict(index=index, warmup=index < 5, status='pending',
                           supervisor_returncode=None, sample=str(sample))
            result['samples'].append(attempt)
            (out / 'result.json').write_text(json.dumps(result, indent=2))
            proc = None
            try:
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
                request = dict(binary=str(binary), log=str(sample / 'app.log'),
                               result=str(sample / 'result.json'), screenshot=screenshot)
                request_path = sample / 'request.json'; request_path.write_text(json.dumps(request))
                with (sample / 'helper.log').open('wb') as log:
                    proc = subprocess.Popen([sys.executable, str(args.helper.resolve()), '--sample-supervisor', str(request_path)],
                                            env=env, stdout=log, stderr=subprocess.STDOUT)
                    attempt['supervisor_returncode'] = helper.wait_supervisor(proc, sample)
                observed = json.loads((sample / 'result.json').read_text())
                supervisor = json.loads((sample / 'supervisor.json').read_text())
                attempt.update(observed=observed, supervisor=supervisor)
                if attempt['supervisor_returncode'] or observed['status'] != 'pass' or not observed['cleanup_verified'] or supervisor['status'] != 'pass' or not supervisor['cleanup']['empty']:
                    raise RuntimeError('Functional or cleanup failure; sample is retained without retry')
                attempt.update(phases((sample / 'app.log').read_text(), observed['elapsed_ns']))
                attempt['status'] = 'pass'
            except BaseException as error:
                attempt.update(status='fail', error=str(error))
                raise
            finally:
                if proc is not None: attempt['supervisor_returncode'] = proc.poll()
                (sample / 'attempt.json').write_text(json.dumps(attempt, indent=2))
                (out / 'result.json').write_text(json.dumps(result, indent=2))
        measured = [s['direct_work_bound_ns'] for s in result['samples'] if not s['warmup']]
        result.update(median_ns=statistics.median(measured), p95_ns=helper.percentile(measured, .95),
                      max_ns=max(measured), all_measured_below_budget=all(n < 50_000_000 for n in measured))
        result['status'] = 'pass' if result['all_measured_below_budget'] else 'over-budget'
    except BaseException as error:
        result.update(status='fail', error=str(error))
    finally:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        try:
            if desktop is not None: desktop.close()
        except BaseException as error:
            result.update(status='fail', desktop_cleanup_error=str(error))
        finally:
            result['backend_cleanup'] = helper.drain_children(psutil.Process(), 15)
            if not result['backend_cleanup']['empty']:
                result.update(status='fail', cleanup_error='Backend owned tree not empty')
            try:
                if runtime is not None: runtime.cleanup()
            except BaseException as error:
                result.update(status='fail', runtime_cleanup_error=str(error))
            finally:
                (out / 'result.json').write_text(json.dumps(result, indent=2))
    return 0 if result['status'] == 'pass' else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ['candidate', 'helper', 'driver', 'output']:
        parser.add_argument('--' + option, type=Path, required=True)
    for option in ['candidate-sha', 'overlay-sha']:
        parser.add_argument('--' + option, required=True)
    parser.add_argument('--backend', choices=['x11', 'wayland'], required=True)
    raise SystemExit(run(parser.parse_args()))
