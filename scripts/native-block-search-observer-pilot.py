#!/usr/bin/env python3
"""Bounded pixel navigation oracle. Never read body node text as success.

Promotes the independent target/neighbor controls after the legacy numeric OCR
fixture produced false negatives. Historical failed verdicts remain unchanged.
"""
import hashlib
import importlib.util
import csv
import io
import json
from pathlib import Path
import re
import time

spec = importlib.util.spec_from_file_location('e03_legacy_pilot_base', Path(__file__).with_name('native-block-search.py'))
base = importlib.util.module_from_spec(spec); spec.loader.exec_module(base)


def terminal_rectangle(image, owned):
    """Locate the painted terminal by its non-text surface, never marker words.

    Within the actual owned client, require the major wide, predominantly
    dark terminal surface. Registered palettes come from the actual
    pre-redesign builtin dark theme/card hover and current terminal surface.
    No target/query/neighbor text participates in choosing this ROI.
    """
    from PIL import Image, ImageChops, ImageDraw
    x, y, width, height = owned
    if (min(x, y) < 0 or width <= 598 or height <= 182 or
            x + width > image.width or y + height > image.height):
        raise RuntimeError('Invalid owned terminal client bounds')
    surface = image.convert('RGB').crop((x, y, x + width, y + height))
    candidates = []
    # Legacy completed-card hover composites foreground220@13/255 over29,
    # yielding39. Keep that real native state visible to the same observer.
    for palette in ((9, 12, 23), (29, 29, 29), (39, 39, 39)):
        channels = ImageChops.difference(surface, Image.new('RGB', surface.size, palette)).split()
        distance = ImageChops.lighter(ImageChops.lighter(channels[0], channels[1]), channels[2])
        mask = distance.point([255] + [0] * 255)
        pixels = mask.tobytes()
        for row in range(height):
            for run in re.finditer(b'\xff{300,}', pixels[row*width:(row+1)*width]):
                seed = (run.start(), row)
                if mask.getpixel(seed) != 255:
                    continue
                ImageDraw.floodfill(mask, seed, 128, thresh=0)
                component = mask.point([255 if value == 128 else 0 for value in range(256)])
                left, top, right, bottom = component.getbbox()
                area = (right-left) * (bottom-top)
                if (right-left >= width * 0.5 and bottom-top >= 80 and
                        component.histogram()[255] >= area * 0.5):
                    candidates.append((area, (left+x, top+y, right+x, bottom+y)))
                mask.paste(0, mask=component)
    if not candidates:
        raise RuntimeError('Native terminal surface is absent or too small')
    candidates.sort(reverse=True)
    if len(candidates) > 1 and candidates[0][0] < candidates[1][0] * 2:
        raise RuntimeError('Native terminal surface is ambiguous')
    return candidates[0][1]


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


class PickerOpen(RuntimeError):
    """Owned picker has not closed yet; observation may wait, never reactivate."""


def closed_picker(tree, owner, pid):
    base.public.probe.validate_tree(tree['nodes'], tree['root'], owner, pid, None)
    if tree['owner'] != owner or tree['pid'] != pid:
        raise RuntimeError('Foreign application during pixel observation')
    # Only closed-surface/ownership evidence is used; submitted-body text is NEVER inspected.
    forbidden = {'Search scope: All', 'Search scope: Cmd', 'Search scope: Out', 'Latest'}
    if any(n['name'] in forbidden or n['role'] == 79 for n in tree['nodes']):
        raise PickerOpen('Picker/editor still present; pixels cannot prove final navigation')


class PilotSmoke(base.SearchSmoke):
    def terminal_words(self, label):
        from PIL import Image, ImageOps
        source = self.out / (label + '.png')
        original = hashlib.sha256(source.read_bytes()).hexdigest()
        geometry = self.run(['xdotool', 'getwindowgeometry', '--shell', self.public.window],
                            capture_output=True, text=True).stdout
        values = dict(line.split('=', 1) for line in geometry.splitlines() if '=' in line)
        owned = [int(values[key]) for key in ('X', 'Y', 'WIDTH', 'HEIGHT')]
        with Image.open(source) as image:
            region = terminal_rectangle(image, owned)
            crop = image.convert('RGB').crop(region)
        raw = self.out / (label + '-terminal.png')
        analysis = self.out / (label + '-terminal-gray-negate.png')
        crop.save(raw)
        # Fixed, pixel-preserving analysis of the same immutable native image.
        # PSM11, exact words, unique occurrences and adjacency remain unchanged.
        ImageOps.invert(ImageOps.grayscale(crop)).save(analysis)
        reply = self.run(['tesseract', str(analysis), 'stdout', '--psm', '11', 'tsv'],
                         capture_output=True, text=True)
        tsv = self.out / (label + '-terminal.tsv')
        tsv.write_text(reply.stdout)
        rows = []
        for row in csv.DictReader(io.StringIO(reply.stdout), delimiter='\t', quoting=csv.QUOTE_NONE):
            if not row['text'].strip():
                continue
            box = [int(row[key]) for key in ('left', 'top', 'width', 'height')]
            if (min(box[:2]) < 0 or min(box[2:]) <= 0 or
                    box[0] + box[2] > crop.width or box[1] + box[3] > crop.height):
                raise RuntimeError('Terminal OCR word crosses its native surface')
            rows.append(dict(row, left=str(box[0] + region[0]), top=str(box[1] + region[1])))
        if hashlib.sha256(source.read_bytes()).hexdigest() != original:
            raise RuntimeError('Native source image changed during terminal analysis')
        (self.out / (label + '-terminal-region.json')).write_text(json.dumps(dict(
            owned_client=owned, region=region, native_source=source.name, source_sha256=original,
            region_sha256=hashlib.sha256(raw.read_bytes()).hexdigest(),
            analysis=analysis.name, analysis_sha256=hashlib.sha256(analysis.read_bytes()).hexdigest(),
            tsv=tsv.name, tsv_sha256=hashlib.sha256(tsv.read_bytes()).hexdigest()), indent=2))
        return rows, tsv

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
        self.observe(name)  # Retain the original whole-frame OCR as evidence.
        words, terminal_tsv = self.terminal_words(label)
        if self.last_observation_label != label or not png.is_file() or not tsv.is_file():
            raise RuntimeError('Pilot capture identity changed')
        if self.app.poll() is not None or self.app_window() != self.public.window:
            raise RuntimeError('Owned application/window changed during fresh capture')
        return words, dict(pid=self.app.pid, window=self.public.window, started_ns=started, finished_ns=time.monotonic_ns(),
            exclusive_fresh_path=True, label=label, png_sha256=hashlib.sha256(png.read_bytes()).hexdigest(),
            tsv_sha256=hashlib.sha256(terminal_tsv.read_bytes()).hexdigest(),
            terminal_tsv=terminal_tsv.name)

    def open_find(self, query):
        # A completed generator/sentinel proves PTY execution, not that the UI
        # consumed and painted its queued output. Wait for positive live pixels
        # before sending the one search action (100k-row raw output can lag).
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            words, capture = self.fresh_pixels('pilot-before-search')
            try:
                live_baseline(words)
            except RuntimeError:
                time.sleep(0.1)
                continue
            break
        else:
            raise RuntimeError('Live viewport pixel baseline deadline exhausted')
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
            # Captured terminal rows omit blank right-hand cells; the stream
            # manifest above independently checks all 96 emitted columns.
            line = base.fixture.line(mode, row, 0, observer_pilot=True).decode().rstrip(' \n')
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
            try:
                closed_picker(before_tree, self.public.owner, self.app.pid)
            except PickerOpen:
                time.sleep(0.1)
                continue
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
        result['correctness'] = 'bounded pixel navigation gate; full E03 exclusions remain'
        result['pixel_oracle'] = 'exact target and adjacent unqueried neighbor with positive live baseline'
        return result
