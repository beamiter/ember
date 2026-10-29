#!/usr/bin/env python3
"""Installed-app recovery acceptance through real desktop input and screenshots.

Run separately from the general desktop gate. Imports its compositor and OCR
helpers without replacing them. Uses only isolated synthetic session metadata.
"""
import argparse
import csv
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import re
import tempfile
import native_glass_profile as glass_profile

spec = importlib.util.spec_from_file_location('desktop_smoke', Path(__file__).with_name('native-desktop-smoke.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class MissingRecoveryCard(RuntimeError):
    pass


def wait_startup_surface(smoke):
    """Wait for actual native paint, not merely a mapped empty GPU window.

    Every first-frame capture remains an artifact. The positive toolbar
    palette is independent of all recovery words; the exact warning checks
    still run afterwards on their own fresh captured pixels.
    """
    sequence = 0
    observations = []
    def painted():
        nonlocal sequence
        assert smoke.app.poll() is None, 'Ember exited before startup paint'
        sequence += 1
        label = f'startup-paint-{sequence:02d}'
        owned = glass_profile.owned_window(smoke) if glass_profile.selected(smoke) else None
        smoke.screenshot(label)
        image = smoke.out / (label + '.png')
        if owned is not None:
            from PIL import Image
            digest = hashlib.sha256(image.read_bytes()).hexdigest()
            if glass_profile.owned_window(smoke) != owned:
                raise RuntimeError('Recovery identity/geometry changed across capture')
            try:
                with Image.open(image) as pixels:
                    witness = glass_profile.header_paint(pixels, owned['rect'],
                                                         glass_profile.fixture_mode(smoke))
            except RuntimeError as error:
                observations.append(dict(image=image.name, sha256=digest,
                                         owned=owned, profile=glass_profile.NAME, paint_error=str(error)))
                return False
            if hashlib.sha256(image.read_bytes()).hexdigest() != digest:
                raise RuntimeError('Recovery source pixels changed during observation')
            observations.append(dict(image=image.name, sha256=digest,
                                     owned=owned, witness=witness))
            return True
        pixels = smoke.run(['convert', str(image), '-colorspace', 'sRGB',
                            '-alpha', 'off', '-depth', '8', 'RGB:-'], capture_output=True).stdout
        count = sum(pixels[i:i+3] == bytes((15, 23, 38)) for i in range(0, len(pixels), 3))
        observations.append(dict(image=image.name, sha256=hashlib.sha256(image.read_bytes()).hexdigest(),
                                 native_toolbar_pixels=count))
        return count >= 500
    try:
        driver.wait_for(painted, 'painted native recovery workspace', timeout=30)
    finally:
        (smoke.out / 'startup-paint.json').write_text(json.dumps(dict(
            pid=smoke.app.pid, window=smoke.recovery_window,
            renderer=getattr(smoke, 'renderer', None) or 'product-default',
            backend=smoke.backend, observations=observations), indent=2))


def recovery_rectangle(image, owned):
    """Find the native floating card from its body/header, without OCR words."""
    from PIL import Image, ImageChops, ImageDraw
    x, y, width, height = [owned[key] for key in ('x', 'y', 'width', 'height')]
    if (min(x, y) < 0 or min(width, height) <= 0 or
            x + width > image.width or y + height > image.height):
        raise RuntimeError('Recovery client lies outside captured pixels')
    client = image.convert('RGB').crop((x, y, x + width, y + height))
    def palette_mask(surface, color):
        channels = ImageChops.difference(surface, Image.new('RGB', surface.size, color)).split()
        return ImageChops.lighter(ImageChops.lighter(channels[0], channels[1]), channels[2]).point([255] + [0] * 255)
    mask = palette_mask(client, (29, 29, 29))
    raw = mask.tobytes()
    candidates = []
    for row in range(height):
        # A long blank run supplies a flood-fill seed; warning text never
        # supplies one. Repeated rows within a component are marked consumed.
        for run in re.finditer(b'\xff{300,}', raw[row * width:(row + 1) * width]):
            seed = (run.start(), row)
            if mask.getpixel(seed) != 255:
                continue
            ImageDraw.floodfill(mask, seed, 128, thresh=0)
            component = mask.point([255 if value == 128 else 0 for value in range(256)])
            bounds = component.getbbox()
            left, top, right, bottom = bounds
            if (right - left >= 300 and bottom - top >= 80 and
                    component.histogram()[255] >= (right-left) * (bottom-top) * 0.5):
                candidates.append(bounds)
            # Consume this component before examining independent candidates.
            mask.paste(0, mask=component)
    if not candidates:
        raise MissingRecoveryCard('Native recovery card absent')
    if len(candidates) != 1:
        raise RuntimeError('Native recovery card ambiguous')
    left, top, right, bottom = candidates[0]
    header_top = max(0, top - 42)
    header = palette_mask(client.crop((left, header_top, right, top)), (45, 45, 45)).getbbox()
    if (header is None or header[2] - header[0] < (right-left) * 0.8 or
            header[3] - header[1] < 18):
        raise RuntimeError('Recovery card lacks its attached native header')
    return (left + x, header_top + header[1] + y, right + x, bottom + y)


def recovery_words(smoke, label):
    from PIL import Image, ImageOps
    # This existing observer brackets an immutable native screenshot with
    # read-only PID/focus/geometry checks. Preserve its full-frame OCR too.
    smoke.reference_settings_words(label)
    ownership = json.loads((smoke.out / (label + '-settings-analysis.json')).read_text())
    source = smoke.out / (label + '.png')
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    with Image.open(source) as image:
        region = recovery_rectangle(image, ownership['owned']['rect'])
        crop = image.convert('RGB').crop(region)
    raw = smoke.out / (label + '-recovery-card.png')
    analysis = smoke.out / (label + '-recovery-card-gray-negate.png')
    crop.save(raw)
    ImageOps.invert(ImageOps.grayscale(crop)).save(analysis)
    reply = smoke.run(['tesseract', str(analysis), 'stdout', '--psm', '6', 'tsv'],
                      capture_output=True, text=True)
    tsv = smoke.out / (label + '-recovery-card.tsv')
    tsv.write_text(reply.stdout)
    rows = []
    for row in csv.DictReader(io.StringIO(reply.stdout), delimiter='\t', quoting=csv.QUOTE_NONE):
        if not row['text'].strip():
            continue
        a, b, w, h = [int(row[key]) for key in ('left', 'top', 'width', 'height')]
        if min(a, b) < 0 or min(w, h) <= 0 or a+w > crop.width or b+h > crop.height:
            raise RuntimeError('Recovery OCR word crosses its native card')
        rows.append(dict(row, left=str(a + region[0]), top=str(b + region[1])))
    if hashlib.sha256(source.read_bytes()).hexdigest() != digest:
        raise RuntimeError('Recovery source pixels changed during analysis')
    (smoke.out / (label + '-recovery-analysis.json')).write_text(json.dumps(dict(
        owned=ownership['owned'], region=region, source_sha256=digest,
        card_sha256=hashlib.sha256(raw.read_bytes()).hexdigest(),
        analysis_sha256=hashlib.sha256(analysis.read_bytes()).hexdigest(),
        tsv_sha256=hashlib.sha256(tsv.read_bytes()).hexdigest(), ocr_psm=6), indent=2))
    return rows


def require_phrases(smoke, label, phrases, text=None):
    if text is None:
        text = ' '.join(row['text'] for row in recovery_words(smoke, label))
    assert all(text.count(phrase) == 1 for phrase in phrases), (phrases, text)
    return text


def phrase_target(rows, phrase):
    # OCR backgrounds can enlarge a word box symmetrically. Compare its
    # center and shared text line, not just its top edge; keep the same 5px
    # alignment tolerance, exact words and one unique contiguous candidate.
    lines = {}
    for row in rows:
        key = tuple(row[k] for k in ('page_num', 'block_num', 'par_num', 'line_num'))
        lines.setdefault(key, []).append(row)
    wanted = phrase.split()
    matches = []
    for line in lines.values():
        line.sort(key=lambda r: int(r['left']))
        for index in range(len(line) - len(wanted) + 1):
            group = line[index:index + len(wanted)]
            if [r['text'] for r in group] != wanted:
                continue
            boxes = [tuple(int(r[k]) for k in ('left', 'top', 'width', 'height')) for r in group]
            if any(min(w, h) <= 0 for _, _, w, h in boxes):
                continue
            centers = [2*y + h for _, y, _, h in boxes]
            if max(centers) - min(centers) > 10:
                continue
            if max(y for _, y, _, _ in boxes) >= min(y+h for _, y, _, h in boxes):
                continue
            if any(not 0 <= bx-(ax+aw) <= min(ah, bh)
                   for (ax, _, aw, ah), (bx, _, _, bh) in zip(boxes, boxes[1:])):
                continue
            matches.append(group)
    if len(matches) != 1:
        raise RuntimeError(f'Absent or ambiguous exact phrase {phrase!r}: {len(matches)}')
    return matches[0][0]


def click_phrase(smoke, phrase, label, rows=None):
    row = phrase_target(smoke.words(label) if rows is None else rows, phrase)
    smoke.click(int(row['left']) + int(row['width']) // 2,
                int(row['top']) + int(row['height']) // 2)


def click_recovery_word(smoke, word, label, card=True):
    rows = recovery_words(smoke, label) if card else smoke.reference_settings_words(label)
    matches = [row for row in rows if row['text'] == word]
    if len(matches) != 1:
        raise RuntimeError(f'Absent or ambiguous exact recovery control {word!r}')
    row = matches[0]
    smoke.click(int(row['left']) + int(row['width']) // 2,
                int(row['top']) + int(row['height']) // 2)


def exercise(smoke, case):
    count = 67 if case == 'over-limit' else 2
    value = {'version': 4, 'sessions': [dict(name=f'synthetic-{i}', tags=[], session_id=f'recovery-{i}') for i in range(count)]}
    if case == 'corrupt-layout':
        value['tabs'] = [{'root': {'kind': 'unknown'}}]
    if case == 'invalid-cwd':
        value['sessions'][1]['cwd'] = str(smoke.out / 'missing-directory')
    source = smoke.config.parent / 'session_history.json'
    original = json.dumps(value).encode()
    source.write_bytes(original)
    source.chmod(0o600)
    config = 'shell = "/bin/bash"\nterminal_mode = "normal"\ninitial_width = 1100\ninitial_height = 800\nrestore_session = true\n'
    if getattr(smoke, 'renderer', None) is not None:
        config += f'app_renderer = "{smoke.renderer}"\n'
    smoke.config.write_text(config)
    smoke.app = smoke.spawn([str(smoke.binary)], 'ember')
    smoke.recovery_window = driver.wait_for(smoke.app_window, 'Ember recovery startup')
    if smoke.backend == 'x11':
        (smoke.out / 'startup-window-geometry.txt').write_text(smoke.run(
            ['xdotool', 'getwindowgeometry', '--shell', smoke.recovery_window],
            capture_output=True, text=True).stdout)
    wait_startup_surface(smoke)
    # This one real terminal click also moves the pointer away from summary
    # text; Wayland screenshots include the otherwise-obscuring I-beam.
    smoke.click(900, 680)
    rows = smoke.reference_settings_words('startup') if case == 'normal' else recovery_words(smoke, 'startup')
    text = ' '.join(row['text'] for row in rows)
    (smoke.out / 'startup-ocr.txt').write_text(text)
    assert smoke.app.poll() is None
    if case == 'normal':
        assert 'recovery summary' not in text.lower(), text
        from PIL import Image
        ownership = json.loads((smoke.out / 'startup-settings-analysis.json').read_text())
        with Image.open(smoke.out / 'startup.png') as image:
            try:
                recovery_rectangle(image, ownership['owned']['rect'])
            except MissingRecoveryCard:
                pass
            else:
                raise RuntimeError('Normal startup unexpectedly contains a native recovery card')
        smoke.command('quiet-startup-usable')
        return {'normal_startup_quiet': True, 'native_card_absence_checked': True}
    phrases = ['Session recovery summary']
    if case == 'over-limit':
        phrases.append('64 session records retained; 3 skipped above the 64-session limit.')
    elif case == 'corrupt-layout':
        phrases.append('Saved pane layout adjusted: invalid or oversized layout references.')
    else:
        phrases.extend(['Session 2:', 'directory invalid or unavailable'])
    require_phrases(smoke, 'startup', phrases, text)
    # Terminal input while the summary remains open proves nonmodal behavior.
    smoke.command('summary-nonmodal-usable')
    click_recovery_word(smoke, 'Dismiss', 'before-dismiss')
    click_recovery_word(smoke, 'details', 'reopen-summary', card=False)
    click_phrase(smoke, 'Recheck original metadata', 'before-recheck',
                 rows=recovery_words(smoke, 'before-recheck'))
    require_phrases(smoke, 'after-recheck', ['Live sessions unchanged'])
    # Export requires an explicit UI path and click. Use the entry's hint to
    # find it, never an application-private action or command channel.
    click_recovery_word(smoke, 'absolute', 'export-path')
    export = smoke.out / 'original-export.json'
    smoke.text(str(export))
    click_recovery_word(smoke, 'locally', 'export-button')
    driver.wait_for(export.exists, 'user-initiated local export')
    assert export.read_bytes() == original
    assert export.stat().st_mode & 0o777 == 0o600
    return {'summary_causes': True, 'nonmodal_terminal_input': True, 'dismiss_reopen': True,
            'read_only_recheck': True, 'original_export_sha256': hashlib.sha256(original).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--backend', choices=['x11', 'wayland'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--case', choices=['normal', 'corrupt-layout', 'over-limit', 'invalid-cwd'], required=True)
    parser.add_argument('--renderer', choices=['wgpu', 'glow'],
                        help='Select a supported native renderer; default uses the product default')
    args = parser.parse_args()
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    smoke = driver.Smoke(args.binary.resolve(strict=True), args.backend, out)
    smoke.renderer = args.renderer
    # Sway's IPC pathname must fit sockaddr_un even when evidence lives in
    # a deep CI checkout. Keep only its private runtime under a short owned
    # directory; all screenshots/logs/results retain their original paths.
    runtime = tempfile.TemporaryDirectory(prefix='ember-r-', dir='/tmp')
    smoke.env['XDG_RUNTIME_DIR'] = runtime.name
    result = {'case': args.case, 'backend': args.backend, 'renderer': args.renderer or 'product-default',
              'binary_sha256': hashlib.sha256(smoke.binary.read_bytes()).hexdigest()}
    try:
        smoke.start_desktop()
        result.update(exercise(smoke, args.case), status='pass')
    except Exception as error:
        result.update(status='fail', error=str(error))
        try: smoke.screenshot('failure')
        except Exception: pass
        raise
    finally:
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        try:
            smoke.close()
        finally:
            runtime.cleanup()


if __name__ == '__main__':
    main()
