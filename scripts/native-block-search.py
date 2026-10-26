#!/usr/bin/env python3
"""First E03 real-app correctness gate. OCR is NOT a latency measurement.

Both entry points use the exact target/neighbor pixel oracle; the former
numeric OCR fixture remains available to pure regression tests only.

No native geometry/IME handshake, app method injection, or instrumented binary.
Run raw and snapshot scenarios in separate fresh app processes/HOMEs.
"""
import argparse
import hashlib
import importlib.util
import json
import os
import re
from pathlib import Path
import shlex
import threading
import subprocess
import time


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


native = load('e03_desktop', 'native-desktop-smoke.py')
fixture = load('e03_output', 'native-block-search-output.py')
public = load('e03_public', 'native-block-search-atspi-driver.py')
BLOCK_SEARCH_CHORD = ('g', ('ctrl', 'shift'))


def exact_phrase(rows, phrase):
    """Exact contiguous words on ONE OCR line, never concatenate unrelated UI."""
    groups = {}
    for row in rows:
        key = tuple(row[k] for k in ('page_num', 'block_num', 'par_num', 'line_num'))
        groups.setdefault(key, []).append(row)
    matches = []
    needle = phrase.split()
    for group in groups.values():
        group.sort(key=lambda r: int(r['left']))
        for start in range(len(group) - len(needle) + 1):
            part = group[start:start + len(needle)]
            if [r['text'] for r in part] == needle:
                matches.append(part)
    return matches


def phrase_center(rows, phrase):
    matches = exact_phrase(rows, phrase)
    if len(matches) != 1:
        raise RuntimeError(f'Absent/ambiguous rendered phrase {phrase!r}: {len(matches)}')
    part = matches[0]
    left = min(int(r['left']) for r in part)
    right = max(int(r['left']) + int(r['width']) for r in part)
    top = min(int(r['top']) for r in part)
    bottom = max(int(r['top']) + int(r['height']) for r in part)
    return (left + right) // 2, (top + bottom) // 2


def raw_hit_claim(rows, token):
    matches = exact_phrase(rows, f'literal match: {token}')
    if len(matches) != 1:
        raise RuntimeError('Absent/ambiguous raw hit positive control')
    matched = matches[0][0]
    keys = ('page_num', 'block_num', 'par_num', 'line_num')
    group = sorted((row for row in rows if all(row[k] == matched[k] for k in keys)),
                   key=lambda row: int(row['left']))
    text = ' '.join(row['text'] for row in group)
    anchor = re.search(r'\bRetained row #([0-9]+)\b', text)
    if not anchor:
        raise RuntimeError('Raw hit lacks readable stable retained-row identity')
    return dict(token=token, retained_row=int(anchor.group(1)))


def active_hit_claim(rows, token):
    if len(exact_phrase(rows, 'Stop search')) != 1:
        raise RuntimeError('No unique active Stop control with the visible hit')
    return raw_hit_claim(rows, token)


def validate_manifest(result, mode, record, identity, observer_pilot=False):
    count = {'raw': 100000, 'snapshot': 2000, 'flush': 256}[mode]
    expected = hashlib.sha256()
    for index in range(count):
        expected.update(fixture.line(mode, index, record, observer_pilot))
    if (result.get('observer_pilot', False) != observer_pilot or
            result.get('mode') != mode or result.get('record') != record or
            result.get('lines') != count or result.get('bytes') != count * 97 or
            result.get('sha256') != expected.hexdigest()):
        raise RuntimeError('Emitter manifest differs from exact expected stream')
    if f"{result.get('shell_pid')}:{result.get('tty')}" != identity:
        raise RuntimeError('Emitter shell/PTY differs from initial live shell')
    if type(result.get('columns')) is not int or result['columns'] < 100:
        raise RuntimeError('Emitter geometry is missing or would wrap')


class SearchSmoke(native.Smoke):
    def __init__(self, binary, backend, out):
        super().__init__(binary, backend, out)
        self.env = public.probe.child_environment(self.env)
        self.bus_daemon = None
        self.public = None
        self.sequence = 0
        self.events = []
        self.fixture_dir = out / 'fixtures'
        self.fixture_dir.mkdir()
        self.env['EMBER_E03_FIXTURES'] = str(self.fixture_dir)
        self.env['EMBER_E03_GENERATOR'] = str(Path(__file__).with_name('native-block-search-output.py').resolve())
        profile = Path(self.env['HOME']) / '.bash_profile'
        with profile.open('a') as stream:
            stream.write('\ne03emit() { python3 "$EMBER_E03_GENERATOR" "$@"; }\n')
        self.memory_stop = threading.Event()
        self.memory_thread = None

    def observe(self, name):
        self.sequence += 1
        label = f'{self.sequence:04d}-{name}'
        rows = self.words(label)
        self.last_observation_label = label
        assert self.app.poll() is None, 'Real app exited'
        self.events.append(dict(event='observation', label=label, monotonic_ns=time.monotonic_ns()))
        return rows

    def wait_phrase(self, phrase, name, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            rows = self.observe(name)
            if exact_phrase(rows, phrase):
                return rows
            time.sleep(0.1)
        if name == 'final-output-navigation':
            # Diagnostic only, AFTER the unchanged OCR deadline has failed.
            # This cannot convert a failed native gate to a pass.
            try:
                window = self.app_window()
                if window != self.public.window or self.app.poll() is not None:
                    raise RuntimeError('Owned application/window changed before body diagnostic')
                self.screenshot('failed-body-before')
                before = self.out / 'failed-body-before.png'
                tree = public.probe.discovery(self.app.pid, self.out / 'failed-body-tree.json', None)
                self.screenshot('failed-body-after')
                after = self.out / 'failed-body-after.png'
                if tree['owner'] != self.public.owner or self.app_window() != window or self.app.poll() is not None:
                    raise RuntimeError('Owned application/window changed during body diagnostic')
                self.events.append(dict(event='failed-body-diagnostic',
                    evidence='public render semantics; not compositor proof',
                    owner=tree['owner'], pid=tree['pid'],
                    before=before.name, after=after.name,
                    before_sha256=hashlib.sha256(before.read_bytes()).hexdigest(),
                    after_sha256=hashlib.sha256(after.read_bytes()).hexdigest()))
            except Exception as error:
                self.events.append(dict(event='failed-body-diagnostic-error', error=str(error)))
        raise RuntimeError(f'Missing rendered phrase {phrase!r}; see every captured observation')

    def wait_active_hit(self, token, name, timeout=15):
        if token != self.public.expected:
            raise RuntimeError('Active raw token differs from verified query')
        result = self.public.raw_result('active', name)
        self.events.append(dict(event='active-hit-claim', claim=result['claim']))
        return result, result['claim']

    def click_phrase(self, phrase, name):
        self.public.button(phrase, name)  # Fresh exact label + immutable screenshot bracket.

    def click_output_scope(self):
        self.public.output_scope()

    def emit(self, mode, record=0):
        path = self.fixture_dir / f'{mode}-{record:02d}.json'
        assert not path.exists()
        self.text(f'e03emit {mode} {record}')
        self.key('Return')
        native.wait_for(path.exists, f'real PTY {mode}/{record}', timeout=120)
        result = json.loads(path.read_text())
        validate_manifest(result, mode, record, self.identity)
        # A separate shell command proves the emitter returned to the live shell.
        identity = self.command(f'{mode}-{record:02d}-done')
        assert identity == self.identity, 'Fixture switched shell/pane unexpectedly'

    def open_find(self, query):
        self.key(*BLOCK_SEARCH_CHORD)  # Exactly once; never retry to focus.
        self.public.query(query)
        self.click_output_scope()

    def navigate(self, token, row, pointer=False, raw=False, expected_claim=None):
        if not raw and token == 'E03SNAPSHOT00':
            line = fixture.line('snapshot', row, 0).decode().rstrip('\n')
            node = self.public.snapshot_result(line, 'snapshot-hit-visible')
            if not pointer:
                raise RuntimeError('Snapshot fixture requires its verified pointer target')
            x,y,width,height = node['screen_extents']
            self.click(x + width // 2, y + height // 2)
            rows = self.wait_phrase(f'ROW{row + 1:06d}', 'final-output-navigation')
            assert exact_phrase(rows, token), 'Final viewport lacks exact target record token'
            assert not exact_phrase(rows, 'Latest'), 'Picker still open; final navigation unproven'
            return
        if raw:
            if token != self.public.expected:raise RuntimeError('Raw query changed')
            result = self.public.raw_result('stopped' if expected_claim is not None else 'complete', 'raw-hit-visible')
            if expected_claim is not None:
                assert result['claim'] == expected_claim, 'Activation claim changed after repaint'
            if pointer:
                x,y,width,height = result['hit']['screen_extents']
                self.click(x + width // 2, y + height // 2)
            else:
                self.public.focus_query()
                self.key('Return')
        else:
            raise RuntimeError('Unsupported exact search navigation fixture')
        # Query/result label alone is insufficient: require adjacent real output row
        # after picker closes. Unique neighboring row was never in its hit snippet.
        rows = self.wait_phrase(f'ROW{row + 1:06d}', 'final-output-navigation')
        assert exact_phrase(rows, token), 'Final viewport lacks exact target record token'
        assert not exact_phrase(rows, 'Latest'), 'Picker still open; final navigation unproven'

    def sample_memory(self):
        def sample():
            with (self.out / 'app-rss.jsonl').open('w') as stream:
                while not self.memory_stop.wait(0.01):
                    try:
                        data = Path(f'/proc/{self.app.pid}/status').read_text()
                        values = {line.split(':')[0]: line.split(':', 1)[1].strip()
                                  for line in data.splitlines()
                                  if line.startswith(('VmRSS:', 'VmHWM:', 'Threads:'))}
                        stream.write(json.dumps(dict(monotonic_ns=time.monotonic_ns(), **values)) + '\n')
                    except FileNotFoundError:
                        return
        self.memory_thread = threading.Thread(target=sample, daemon=True)
        self.memory_thread.start()

    def exercise_search(self, scenario):
        self.config.write_text('shell = "/bin/bash"\nterminal_mode = "block"\n'
                               'initial_width = 1200\ninitial_height = 800\nfont_size = 12.0\n'
                               'scrollback_lines = 160000\ncommand_history_max_entries = 1000\n'
                               'restore_session = false\n')
        self.app = self.spawn([str(self.binary)], 'ember')
        window = native.wait_for(self.app_window, 'owned Ember window')
        self.public = public.Driver(self, window)
        self.sample_memory()
        time.sleep(2)
        self.identity = self.command('initial-shell')
        if scenario == 'raw':
            self.emit('raw')
            self.emit('flush')
            self.open_find('E03EARLYHIT')
            self.click_phrase('Search this retained output', 'raw-start')
            # Native loading activation must stay open. This is an observable
            # adversary, not deterministic control of a particular worker boundary.
            self.public.focus_query()
            _, claim = self.wait_active_hit('E03EARLYHIT', 'raw-loading-hit')
            self.key('Return')
            _, after_return = self.wait_active_hit('E03EARLYHIT', 'loading-return-rejected')
            assert after_return == claim, 'Loading Return replaced the visible hit identity'
            self.click_phrase('Stop search', 'stop-once')
            stopped = self.public.raw_result('stopped', 'stopped-repaint')
            assert stopped['claim'] == claim, 'Stop changed verified hit identity'
            # Positive control: the exact rejected hit becomes activatable after
            # Stop and a fresh screenshot, without replacing query or result.
            self.navigate('E03EARLYHIT', 16, pointer=True, raw=True, expected_claim=claim)
            assert self.command('stopped-hit-navigation') == self.identity
            # Independent explicit flow retains Restart coverage after closure.
            self.open_find('E03EARLYHIT')
            self.click_phrase('Search this retained output', 'restart-flow-start')
            self.click_phrase('Stop search', 'restart-flow-stop')
            self.wait_phrase('Restart this output', 'restart-flow-stopped')
            self.click_phrase('Restart this output', 'restart-once')
            # Status text wraps with retained-range/progress chrome. Verify its
            # exact owned public label instead of joining unrelated OCR lines;
            # final navigation still requires independent target/neighbor pixels.
            self.public.raw_result('complete', 'raw-complete')
            self.navigate('E03EARLYHIT', 16, pointer=True, raw=True)
        else:
            for record in range(64):
                self.emit('snapshot', record)
            self.emit('flush')
            self.open_find('E03SNAPSHOT00')
            self.click_phrase('Earlier history', 'snapshot-earlier')
            self.navigate('E03SNAPSHOT00', 16, pointer=True)
        return dict(scenario=scenario, shell_identity=self.identity,
                    correctness='first-stage matrix only; see exclusions')

    def close(self):
        self.memory_stop.set()
        if self.memory_thread:
            self.memory_thread.join(timeout=2)
        (self.out / 'events.json').write_text(json.dumps(self.events, indent=2))
        super().close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--backend', choices=['x11'], default='x11')
    parser.add_argument('--scenario', choices=['raw', 'snapshot'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--observer-pilot', action='store_true',
                        help='Compatibility entry point for the former independent pixel observer pilot')
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    result = dict(binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  backend=args.backend, scenario=args.scenario,
                  exclusions=['input p95 and presentation latency', 'complete memory accounting',
                              'natural-limit Continue', 'continuous PTY contention',
                              'pane/query/reset/eviction/alternate-screen race matrix',
                              'keyboard final navigation', 'Wayland'])
    # The ambiguous numeric OCR observer is superseded, not character-repaired.
    # Keep both CI entry points on the same strict positive/negative pixel proof.
    smoke_class = load('e03_observer_pilot', 'native-block-search-observer-pilot.py').PilotSmoke
    result['fixture_version'] = 'bounded-pixel-v2'
    smoke = smoke_class(binary, args.backend, out)
    if args.observer_pilot:
        result['observer_pilot'] = 'compatibility entry point; bounded-pixel-v2 oracle'
    try:
        log = open(out/'private-dbus.log', 'w'); smoke.logs.append(log)
        smoke.bus_daemon = subprocess.Popen(['dbus-daemon', '--session', '--nofork', '--print-address=1'],
                                            env=smoke.env, stdout=log, stderr=log, start_new_session=True)
        smoke.processes.append(smoke.bus_daemon)
        def bus_ready():
            if smoke.bus_daemon.poll() is not None: raise RuntimeError('Private D-Bus exited')
            lines = (out/'private-dbus.log').read_text().splitlines()
            return lines[0] if lines and lines[0].startswith('unix:') else None
        smoke.env['DBUS_SESSION_BUS_ADDRESS'] = native.wait_for(bus_ready, 'private D-Bus')
        smoke.start_desktop()
        smoke.run(['/usr/bin/python3', str(Path(public.probe.__file__).resolve()), '--enable', '--output', str(out/'enable.json')])
        os.environ.clear(); os.environ.update(smoke.env)
        result['activated_service_cleanup'] = 'unverified'
        result.update(smoke.exercise_search(args.scenario), status='pass')
    except Exception as error:
        result.update(status='fail', error=str(error))
        try:
            smoke.screenshot('failure')
        except Exception:
            pass
        raise
    finally:
        try:
            public.probe.close_resources(smoke.bus_daemon, smoke)
        except Exception as error:
            result.update(status='fail', cleanup_error=str(error))
            raise
        finally:
            (out / 'result.json').write_text(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
