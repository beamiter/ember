#!/usr/bin/env python3
"""Preregistered independent pixel observer experiment. Never read body node text as success."""
import hashlib
import importlib.util
import json
from pathlib import Path
import time

spec = importlib.util.spec_from_file_location('e03_legacy_pilot_base', Path(__file__).with_name('native-block-search.py'))
base = importlib.util.module_from_spec(spec); spec.loader.exec_module(base)


def bbox(words):
    left = min(int(w['left']) for w in words); top = min(int(w['top']) for w in words)
    right = max(int(w['left']) + int(w['width']) for w in words)
    bottom = max(int(w['top']) + int(w['height']) for w in words)
    return left, top, right, bottom


def unique_phrase(words, phrase):
    matches = base.exact_phrase(words, phrase)
    if len(matches) != 1:
        raise RuntimeError('Missing/ambiguous exact pilot phrase')
    part = matches[0]
    boxes = [bbox([word]) for word in part]
    if any(right <= left or bottom <= top for left, top, right, bottom in boxes):
        raise RuntimeError('Invalid pilot word rectangle')
    for a, b in zip(boxes, boxes[1:]):
        height = max(a[3] - a[1], b[3] - b[1])
        overlap = min(a[3], b[3]) - max(a[1], b[1])
        if overlap * 2 < height or not 0 <= b[0] - a[2] <= height:
            raise RuntimeError('Pilot phrase words are not spatially contiguous')
    return part


def live_baseline(words):
    unique_phrase(words, base.fixture.PILOT_LIVE_MARKER)
    if any(w['text'] in ('TARGET', 'NEIGHBOR') for w in words):
        raise RuntimeError('Target viewport already present before search')


def pixel_claim(words, mode, capture, activation):
    """Exact strings + one-frame adjacent geometry; no fuzzy/case/character normalization."""
    if (capture['activation'] != activation or capture['started_ns'] <= activation
            or capture['finished_ns'] < capture['started_ns'] or not capture['exclusive_fresh_path']):
        raise RuntimeError('Old or unproven screenshot provenance')
    if len(capture['png_sha256']) != 64:
        raise RuntimeError('Missing screenshot digest')
    if any(w['text'] in ('LATEST', 'CEDAR') for w in words):
        raise RuntimeError('Live endpoint still present in final viewport')
    observations = [(w['text'], w['left'], w['top'], w['width'], w['height']) for w in words]
    if len(observations) != len(set(observations)):
        raise RuntimeError('Duplicate OCR observations')
    target, neighbor = base.fixture.PILOT_MARKERS[mode]
    first = unique_phrase(words, target); second = unique_phrase(words, neighbor)
    a, b = bbox(first), bbox(second)
    height = min(a[3] - a[1], b[3] - b[1])
    if (height <= 0 or max(a[3] - a[1], b[3] - b[1]) > 2 * height or b[1] <= a[3] or b[1] - a[1] > 2 * height
            or abs(a[0] - b[0]) > height):
        raise RuntimeError('Pilot markers are not aligned adjacent output rows')
    # Reject any intervening OCR baseline, even if both markers were somewhere on screen.
    if any(a[3] < int(w['top']) < b[1] for w in words):
        raise RuntimeError('Intervening OCR row between pilot markers')
    return {'target': target, 'neighbor': neighbor, 'target_box': a, 'neighbor_box': b,
            'capture': capture, 'source': 'exact OCR words from one newly captured PNG'}


def closed_picker(tree, owner, pid):
    base.public.probe.validate_tree(tree['nodes'], tree['root'], owner, pid, None)
    if tree['owner'] != owner or tree['pid'] != pid:
        raise RuntimeError('Foreign application during pixel observation')
    # Only closed-surface/ownership evidence is used; submitted-body text is NEVER inspected.
    forbidden = {'Search scope: All', 'Search scope: Cmd', 'Search scope: Out', 'Latest'}
    if any(n['name'] in forbidden or n['role'] == 79 for n in tree['nodes']):
        raise RuntimeError('Picker/editor still present; pixels cannot prove final navigation')


class PilotSmoke(base.SearchSmoke):
    def emit(self, mode, record=0):
        path = self.fixture_dir / f'{mode}-{record:02d}.json'
        assert not path.exists()
        self.text(f'e03emit {mode} {record} --observer-pilot')
        self.key('Return')
        base.native.wait_for(path.exists, f'real pilot PTY {mode}/{record}', timeout=120)
        result = json.loads(path.read_text())
        base.validate_manifest(result, mode, record, self.identity, observer_pilot=True)
        assert self.command(f'{mode}-{record:02d}-done') == self.identity

    def fresh_pixels(self, name):
        label = f'{self.sequence + 1:04d}-{name}'
        png, tsv = self.out / (label + '.png'), self.out / (label + '.tsv')
        if png.exists() or tsv.exists(): raise RuntimeError('Pilot refuses a reused observation path')
        if self.app.poll() is not None or self.app_window() != self.public.window:
            raise RuntimeError('Owned application/window changed before fresh capture')
        started = time.monotonic_ns()
        words = self.observe(name)
        if self.last_observation_label != label or not png.is_file() or not tsv.is_file():
            raise RuntimeError('Pilot capture identity changed')
        if self.app.poll() is not None or self.app_window() != self.public.window:
            raise RuntimeError('Owned application/window changed during fresh capture')
        return words, dict(pid=self.app.pid, window=self.public.window, started_ns=started, finished_ns=time.monotonic_ns(),
            exclusive_fresh_path=True, label=label, png_sha256=hashlib.sha256(png.read_bytes()).hexdigest(),
            tsv_sha256=hashlib.sha256(tsv.read_bytes()).hexdigest())

    def open_find(self, query):
        words, capture = self.fresh_pixels('pilot-before-search')
        live_baseline(words)
        mode = {'E03EARLYHIT': 'raw', 'E03SNAPSHOT00': 'snapshot'}.get(query)
        if mode is None: raise RuntimeError('Unregistered pilot query')
        self.pilot_baseline = dict(query=query, mode=mode, pid=self.app.pid,
                                   window=self.public.window, capture=capture)
        self.events.append(dict(event='pilot-live-baseline', **self.pilot_baseline))
        super().open_find(query)

    def navigate(self, token, row, pointer=False, raw=False, expected_claim=None):
        if row != 16 or not pointer:
            raise RuntimeError('Pilot only supports preregistered pointer activation of row 16')
        baseline = getattr(self, 'pilot_baseline', None)
        mode = 'raw' if raw else 'snapshot'
        if (not baseline or baseline['query'] != token or baseline['mode'] != mode
                or baseline['pid'] != self.app.pid or baseline['window'] != self.public.window):
            raise RuntimeError('Missing current live-viewport negative control')
        self.pilot_baseline = None  # A baseline cannot authorize a second activation.
        if token != self.public.expected:
            raise RuntimeError('Pilot query changed')
        if raw:
            result = self.public.raw_result('stopped' if expected_claim is not None else 'complete', 'raw-hit-visible')
            if expected_claim is not None and result['claim'] != expected_claim:
                raise RuntimeError('Pilot raw activation claim changed')
            node = result['hit']
        else:
            if token != 'E03SNAPSHOT00': raise RuntimeError('Unexpected pilot snapshot token')
            line = base.fixture.line(mode, row, 0, observer_pilot=True).decode().rstrip('\n')
            node = self.public.snapshot_result(line, 'snapshot-hit-visible')
        x, y, width, height = node['screen_extents']
        before_words, before_capture = self.fresh_pixels('pilot-before-activation')
        if any(w['text'] == 'NEIGHBOR' for w in before_words):
            raise RuntimeError('Neighbor already visible before activation')
        activation = time.monotonic_ns()
        if before_capture['started_ns'] <= baseline['capture']['finished_ns']:
            raise RuntimeError('Old pre-activation observation')
        self.click(x + width // 2, y + height // 2)  # Exactly once; no navigation retries.
        deadline = time.monotonic() + 30  # Same observation budget as the legacy final oracle.
        last_error = None
        while time.monotonic() < deadline:
            label = f'{self.sequence + 1:04d}-pilot-final-output'
            before_tree = base.public.probe.discovery(self.app.pid, self.out / (label + '-before-picker.json'), None)
            closed_picker(before_tree, self.public.owner, self.app.pid)
            if time.monotonic() >= deadline: raise RuntimeError('Pilot observation deadline exhausted')
            words, capture = self.fresh_pixels('pilot-final-output')  # Unchanged PSM11, no scaling.
            capture['activation'] = activation
            try:
                claim = pixel_claim(words, mode, capture, activation)
            except RuntimeError as error:
                last_error = str(error); time.sleep(0.1); continue
            tree = base.public.probe.discovery(self.app.pid, self.out / (label + '-closed-picker.json'), None)
            closed_picker(tree, self.public.owner, self.app.pid)
            if time.monotonic() >= deadline: raise RuntimeError('Pilot observation deadline exhausted')
            if self.app.poll() is not None or self.app_window() != self.public.window:
                raise RuntimeError('Application/window changed during pilot observation')
            self.events.append(dict(event='pilot-pixel-claim', claim=claim,
                live_baseline=baseline, before_activation=before_capture,
                legacy_row_exact=bool(base.exact_phrase(words, 'ROW000017')),
                legacy_token_exact=bool(base.exact_phrase(words, token))))
            return
        raise RuntimeError('Pilot pixel oracle failed without parameter retries: ' + str(last_error))

    def exercise_search(self, scenario):
        result = super().exercise_search(scenario)
        result['correctness'] = 'independent observer experiment only; legacy gate and full E03 remain separate'
        return result
