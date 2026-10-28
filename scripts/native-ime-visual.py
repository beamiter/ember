"""Fail-closed rendered-preedit evidence in a stable owned scale-one surface."""
import hashlib
from collections import deque
import json
import math
import os
import re

PREEDIT_TOKEN = 'EMBERPREEDITMARKER'
HEALTH_ANCHOR = 'master'
REFERENCE_SHELL_HEALTH = False
# Opted into only by registered scale-one14/16pt reference fixtures.
# The original132f private geometry probe measured a23px native caret line.
# sync_font_metrics takes the maximum of native-atlas metrics and the control
# galley+4px inset. A separate locked-egui16pt measurement gives18+4=22px;
# the actual native23px line, not that smaller control minimum, is this bound.
REFERENCE_INLINE_BODY = False
INLINE_FIXTURE_FONT_SIZE = 16.0
INLINE_FIXTURE_LINE_HEIGHT = 23
INLINE_FIXTURE_REGISTRATIONS = {
    16.0: (16, 23, 'native132f actual caret height23 at configured16pt/line_spacing1/scale1; locked-egui control18+4=22 is a lower bound'),
    14.0: (14, 20, 'native d6 fourteen-point active-switch frame:20px outer input row,18px exact dark plane after rounded border; source sync_font_metrics reserves control galley plus4'),
}
OUTPUT_RECT = {'x': 0, 'y': 0, 'width': 1280, 'height': 900}
FOOTER_RECT = {'x': 0, 'y': 876, 'width': 1280, 'height': 24}


def box(row):
    return {key: int(row[field]) for key, field in
            [('x', 'left'), ('y', 'top'), ('width', 'width'), ('height', 'height')]}


def contained(inner, outer):
    return (inner['width'] > 0 and inner['height'] > 0 and
            inner['x'] >= outer['x'] and inner['y'] >= outer['y'] and
            inner['x'] + inner['width'] <= outer['x'] + outer['width'] and
            inner['y'] + inner['height'] <= outer['y'] + outer['height'])


def owned_rectangle(tree, pid):
    matches = []
    def visit(node):
        if node.get('app_id') == 'io.github.beamiter.ember':
            matches.append(node)
        for child in node.get('nodes', []) + node.get('floating_nodes', []):
            visit(child)
    visit(tree)
    if len(matches) != 1:
        raise RuntimeError('Absent or ambiguous Ember surface')
    node = matches[0]
    if node.get('pid') != pid or node.get('focused') is not True:
        raise RuntimeError('Preedit surface ownership/focus mismatch')
    rect = node['rect']
    if (any(type(rect[key]) is not int for key in OUTPUT_RECT) or
            not contained(rect, OUTPUT_RECT)):
        raise RuntimeError('Invalid or off-output app rectangle')
    return {'id': node['id'], 'pid': pid, 'rect': rect}


def validate_rows(rows):
    if not rows:
        raise RuntimeError('Empty OCR cannot establish absence')
    for row in rows:
        try:
            confidence = float(row['conf'])
            if (row['level'] != '5' or not isinstance(row['text'], str) or
                    not row['text'].strip() or not math.isfinite(confidence) or
                    not 0 <= confidence <= 100 or not contained(box(row), OUTPUT_RECT)):
                raise ValueError('invalid OCR word')
        except (KeyError, TypeError, ValueError) as error:
            raise RuntimeError('Malformed OCR word evidence') from error


def exact_matches(rows, rectangle, token=PREEDIT_TOKEN):
    matches = []
    for row in rows:
        if row['text'] != token:
            continue
        if not contained(box(row), rectangle['rect']):
            raise RuntimeError('Exact token crosses or lies outside owned rectangle')
        matches.append(row)
    return matches


def footer_health(smoke, label, image, rectangle):
    # Fixed geometry, selected before OCR; this is a pixel-preserving crop of
    # the same immutable capture, never a retry, resize, mask or text repair.
    if not contained(FOOTER_RECT, rectangle['rect']):
        raise RuntimeError('Health footer lies outside owned surface')
    footer_label = label + '-health-footer'
    footer = smoke.out / (footer_label + '.png')
    crop = f"{FOOTER_RECT['width']}x{FOOTER_RECT['height']}+{FOOTER_RECT['x']}+{FOOTER_RECT['y']}"
    smoke.run(['convert', str(image), '-crop', crop, '+repage', str(footer)])
    dimensions = smoke.run(['identify', '-format', '%w %h', str(footer)],
                           capture_output=True, text=True).stdout
    if dimensions != '1280 24':
        raise RuntimeError('Unexpected health footer dimensions or image count')
    footer_sha256 = hashlib.sha256(footer.read_bytes()).hexdigest()
    rows = smoke.words(footer_label, psm=7, capture=False)
    validate_rows(rows)
    local_rectangle = dict(FOOTER_RECT, x=0, y=0)
    mapped = []
    for row in rows:
        # Validate every local word before translating into original image
        # coordinates; even an unrelated out-of-crop word invalidates evidence.
        if not contained(box(row), local_rectangle):
            raise RuntimeError('OCR word crosses health footer crop bounds')
        mapped.append(dict(row, left=str(int(row['left']) + FOOTER_RECT['x']),
                           top=str(int(row['top']) + FOOTER_RECT['y'])))
    if hashlib.sha256(footer.read_bytes()).hexdigest() != footer_sha256:
        raise RuntimeError('Health footer changed during OCR analysis')
    return mapped, {'rectangle': dict(FOOTER_RECT), 'image': str(footer),
                    'image_sha256': footer_sha256,
                    'tsv': str(smoke.out / (footer_label + '-psm7.tsv')), 'ocr_psm': 7}


def reference_header_health(smoke, label, image, rectangle):
    """Native-shell readiness without depending on a removed footer word.

    The PID-owned/focused surface is verified before and after capture. Require
    the immutable 42px toolbar's actual palette and a real PTY sentinel from
    this same live shell. Preedit still uses exact OCR, pixel restoration and
    byte assertions; no OCR rows are manufactured from the native proof.
    """
    owned = rectangle['rect']
    region = dict(x=owned['x'], y=owned['y'], width=owned['width'], height=42)
    if not contained(region, owned) or smoke.app.poll() is not None:
        raise RuntimeError('Reference header lies outside live owned surface')
    transcript = smoke.transcript.read_bytes()
    marker = getattr(smoke, 'reference_readiness_marker', 'preedit-switch-ready')
    if marker not in ('preedit-switch-ready', 'ime-ready', 'live-ime-ready'):
        raise RuntimeError('Unregistered reference readiness marker')
    matches = re.findall(rb'^' + re.escape(marker.encode('ascii')) + rb':([0-9]+):(/dev/pts/[0-9]+)$', transcript, re.M)
    if len(matches) != 1:
        raise RuntimeError('Missing exact real-PTY readiness identity')
    shell_pid, terminal = matches[0]
    os.kill(int(shell_pid), 0)
    name = label + '-health-header'
    target = smoke.out / (name + '.png')
    crop = f"{region['width']}x{region['height']}+{region['x']}+{region['y']}"
    smoke.run(['convert', str(image), '-crop', crop, '+repage', str(target)])
    dimensions = smoke.run(['identify', '-format', '%w %h', str(target)], capture_output=True, text=True).stdout
    if dimensions != f"{region['width']} {region['height']}":
        raise RuntimeError('Unexpected reference header dimensions')
    digest = hashlib.sha256(target.read_bytes()).hexdigest()
    pixels = smoke.run(['convert', str(target), '-colorspace', 'sRGB', '-alpha', 'off', '-depth', '8', 'RGB:-'], capture_output=True).stdout
    expected = region['width'] * region['height']
    if len(pixels) != expected * 3:
        raise RuntimeError('Unexpected native toolbar pixel count')
    background = sum(pixels[i:i+3] == bytes((15,23,38)) for i in range(0,len(pixels),3))
    # This positive visual signature is registered from reference_shell's
    # exact titlebar palette, rather than inferred from OCR token failure.
    if not expected * 0.5 <= background <= expected * 0.95:
        raise RuntimeError('Native toolbar background/content signature absent')
    if hashlib.sha256(target.read_bytes()).hexdigest() != digest:
        raise RuntimeError('Reference header changed during analysis')
    return {'rectangle': region, 'image': str(target), 'image_sha256': digest,
            'scope': 'PID-owned native toolbar pixels and real PTY identity',
            'background_rgb': [15,23,38], 'background_pixel_count': background,
            'total_pixel_count': expected, 'shell_pid': int(shell_pid),
            'tty': terminal.decode(), 'sentinel_transcript_sha256': hashlib.sha256(transcript).hexdigest()}


def reference_token_rows(smoke, label, image, rectangle, psm):
    """Exact token OCR in the entire native center, with unchanged pixels.

    Reference geometry scopes the surface before analysis; token content never
    selects the region. It excludes navigation/inspector text that otherwise
    interferes with OCR segmentation of light/dark native message surfaces.
    """
    owned=rectangle['rect']
    region={'x':owned['x']+292,'y':owned['y']+91,
            'width':owned['width']-292-306,'height':owned['height']-91}
    if not contained(region,owned):raise RuntimeError('Invalid reference native canvas crop')
    name=label+'-native-canvas'
    target=smoke.out/(name+'.png')
    smoke.run(['convert',str(image),'-crop',f"{region['width']}x{region['height']}+{region['x']}+{region['y']}",'+repage',str(target)])
    digest=hashlib.sha256(target.read_bytes()).hexdigest()
    rows=smoke.words(name,psm=psm,capture=False)
    validate_rows(rows)
    local=dict(region,x=0,y=0)
    mapped=[]
    for row in rows:
        if not contained(box(row),local):raise RuntimeError('Token OCR crosses native canvas bounds')
        mapped.append(dict(row,left=str(int(row['left'])+region['x']),top=str(int(row['top'])+region['y'])))
    if hashlib.sha256(target.read_bytes()).hexdigest()!=digest:raise RuntimeError('Token canvas crop changed during OCR')
    return mapped,{'rectangle':region,'image':str(target),'image_sha256':digest,'ocr_psm':psm}


def native_body_registration(pixels, width, height, *, single_line=True,
                             min_height=16, max_height=23):
    """Register every substantial single-line dark plane without reading text.

    The reference terminal surface is RGB9,12,23. Connected components are
    measured before OCR; no target word, cursor position or token count selects
    a component. Partial glyphs/small icons cannot establish a terminal body.
    """
    if len(pixels) != width * height * 3:
        raise RuntimeError('Wrong native canvas RGB byte count')
    if (type(min_height) is not int or type(max_height) is not int or
            min_height <= 0 or max_height < min_height):
        raise RuntimeError('Unregistered native body height contract')
    mask = bytearray(pixels[i:i+3] == bytes((9, 12, 23))
                     for i in range(0, len(pixels), 3))
    groups = {}
    for seed in range(len(mask)):
        if not mask[seed]:
            continue
        mask[seed] = 0
        pending = deque([seed])
        x0 = x1 = seed % width
        y0 = y1 = seed // width
        count = 0
        while pending:
            current = pending.popleft()
            count += 1
            x, y = current % width, current // width
            x0, x1, y0, y1 = min(x0, x), max(x1, x), min(y0, y), max(y1, y)
            neighbors = []
            if x: neighbors.append(current - 1)
            if x + 1 < width: neighbors.append(current + 1)
            if y: neighbors.append(current - width)
            if y + 1 < height: neighbors.append(current + width)
            for neighbor in neighbors:
                if mask[neighbor]:
                    mask[neighbor] = 0
                    pending.append(neighbor)
        component = dict(x=x0, y=y0, width=x1-x0+1, height=y1-y0+1, dark_pixels=count)
        # A real caret or pale composition can split a native dark plane into
        # disconnected fragments. Reunite only exactly collinear fragments,
        # before any width filter. No vertical tolerance or word targeting.
        groups.setdefault((component['y'], component['height']), []).append(component)
    registration = []
    for (y, h), components in groups.items():
        x = min(c['x'] for c in components)
        w = max(c['x']+c['width'] for c in components)-x
        if (w >= 250 and min_height <= h and
                (not single_line or h <= max_height)):
            registration.append(dict(rectangle=dict(x=x, y=y, width=w, height=h),
                                     components=sorted(components, key=lambda c: c['x'])))
    return sorted(registration, key=lambda r: (r['rectangle']['y'], r['rectangle']['x']))


def native_body_rectangles(pixels, width, height, **bounds):
    return [record['rectangle'] for record in native_body_registration(pixels, width, height, **bounds)]


def reference_body_observations(smoke, label, image, rectangle, contract):
    """Additional OCR passes over all registered bodies in the same PNG.

    Whole-canvas passes remain in use. This resolves their segmentation loss
    when a one-line body is surrounded by a light conversation and composer.
    Empty tight-body OCR is allowed only because whole-canvas/health evidence
    has independently been validated by snapshot().
    """
    owned = rectangle['rect']
    canvas = dict(x=owned['x']+292, y=owned['y']+91,
                  width=owned['width']-292-306, height=owned['height']-91)
    if not contained(canvas, owned):
        raise RuntimeError('Native body canvas outside owned surface')
    raw = smoke.run(['convert', str(image), '-crop',
                     f"{canvas['width']}x{canvas['height']}+{canvas['x']}+{canvas['y']}",
                     '+repage', '-colorspace', 'sRGB', '-alpha', 'off', '-depth', '8', 'RGB:-'],
                    capture_output=True).stdout
    regions = native_body_registration(raw, canvas['width'], canvas['height'],
        min_height=contract['min_height'], max_height=contract['line_height'])
    observations, evidence = [], []
    for index, registration in enumerate(regions):
        local = registration['rectangle']
        region = dict(local, x=local['x']+canvas['x'], y=local['y']+canvas['y'])
        if not contained(region, canvas):
            raise RuntimeError('Registered body crosses native canvas')
        name = f'{label}-native-body-{index}'
        target = smoke.out / (name+'.png')
        smoke.run(['convert', str(image), '-crop',
                   f"{region['width']}x{region['height']}+{region['x']}+{region['y']}",
                   '+repage', str(target)])
        digest = hashlib.sha256(target.read_bytes()).hexdigest()
        rows = smoke.words(name, psm=6, capture=False)
        if rows: validate_rows(rows)
        mapped = []
        for row in rows:
            if not contained(box(row), dict(local, x=0, y=0)):
                raise RuntimeError('OCR word crosses registered native body')
            mapped.append(dict(row, left=str(int(row['left'])+region['x']),
                               top=str(int(row['top'])+region['y'])))
        if hashlib.sha256(target.read_bytes()).hexdigest() != digest:
            raise RuntimeError('Native body changed during OCR')
        observations.append((name, mapped))
        evidence.append(dict(rectangle=region, image=str(target), image_sha256=digest,
                             ocr_psm=6, tsv=str(smoke.out/(name+'-psm6.tsv')),
                             components=[dict(c, x=c['x']+canvas['x'], y=c['y']+canvas['y'])
                                         for c in registration['components']]))
    return observations, evidence


def require_legacy_token_geometry(primary, selected, rectangle, body_observations):
    """Preserve legacy cross-layout geometry except a proved missing thin row.

    A missing selected pass is covered only by the exact same physical word
    measured in an independently registered one-line body. No exemption applies
    to a word outside that body, a duplicate or two disagreeing nonempty boxes.
    """
    if len(primary) > 1:
        raise RuntimeError('Token layouts disagree on exact token geometry')
    if primary and [box(r) for r in primary] != [box(r) for r in selected]:
        covered = (not selected and len(primary) == 1 and any(
            box(word) == box(primary[0])
            for _, rows in body_observations for word in exact_matches(rows, rectangle)))
        if not covered:
            raise RuntimeError('Token layouts disagree on exact token geometry')


def require_native_plane_witness(word, rectangle, canvas, planes):
    """A full-image-only word must belong to exactly one native dark plane."""
    bounds = box(word)
    if not contained(bounds, rectangle['rect']) or not contained(bounds, canvas):
        raise RuntimeError('Exact token outside owned native canvas')
    matches = [plane for plane in planes if contained(bounds, plane['rectangle'])]
    if len(matches) != 1:
        raise RuntimeError('Missing or ambiguous native plane for whole-image exact token')
    return matches[0]


def reference_native_plane_witnesses(smoke, image, rectangle, contract):
    """Geometry only for multi-line planes; do not add a tighter OCR pass."""
    owned = rectangle['rect']
    canvas = dict(x=owned['x']+292, y=owned['y']+91,
                  width=owned['width']-292-306, height=owned['height']-91)
    if not contained(canvas, owned):
        raise RuntimeError('Native witness canvas outside owned surface')
    raw = smoke.run(['convert',str(image),'-crop',
                    f"{canvas['width']}x{canvas['height']}+{canvas['x']}+{canvas['y']}",
                    '+repage','-colorspace','sRGB','-alpha','off','-depth','8','RGB:-'],
                    capture_output=True).stdout
    planes=[]
    for registration in native_body_registration(raw, canvas['width'], canvas['height'], single_line=False,
            min_height=contract['min_height'], max_height=contract['line_height']):
        local = registration['rectangle']
        planes.append(dict(rectangle=dict(local,x=local['x']+canvas['x'],y=local['y']+canvas['y']),
                           components=[dict(c,x=c['x']+canvas['x'],y=c['y']+canvas['y'])
                                       for c in registration['components']]))
    return canvas, planes, hashlib.sha256(raw).hexdigest()


def inline_body_fixture_contract(config_path):
    import tomllib
    config_bytes = config_path.read_bytes()
    config = tomllib.loads(config_bytes.decode('utf-8'))
    font = config.get('font_size')
    if (type(font) not in (int, float) or font not in INLINE_FIXTURE_REGISTRATIONS or config.get('ui_font_size') != font or
            type(config.get('line_spacing')) not in (int, float) or config.get('line_spacing') != 1.0):
        raise RuntimeError('Unregistered font/line spacing for inline-body fixture')
    minimum, maximum, source = INLINE_FIXTURE_REGISTRATIONS[font]
    return dict(config=str(config_path), config_sha256=hashlib.sha256(config_bytes).hexdigest(),
                font_size=font, ui_font_size=font, line_height=maximum,
                min_height=minimum, scale=1, line_spacing=1.0, line_source=source)


def merge_exact_token_observations(observations, rectangle):
    """Union exact occurrences; merge only the same overlapping physical word.

    A missing OCR pass cannot erase another pass's occurrence. Two words from
    one pass are never deduplicated, even if their boxes are identical. Across
    passes only identical boxes deduplicate. Overlapping unequal boxes remain
    a geometry error, including one-pixel shifts. Distinct native bodies/words
    remain separate occurrences, not a maximum count.
    """
    groups = []
    for source, rows in observations:
        for row in exact_matches(rows, rectangle):
            b = box(row)
            def overlaps_word(other):
                a = box(other)
                return (max(a['x'], b['x']) < min(a['x']+a['width'], b['x']+b['width']) and
                        max(a['y'], b['y']) < min(a['y']+a['height'], b['y']+b['height']))
            overlaps = [group for group in groups if overlaps_word(group['row'])]
            if len(overlaps) > 1:
                raise RuntimeError('Ambiguous overlapping exact-token evidence')
            if overlaps:
                if box(overlaps[0]['row']) != b:
                    raise RuntimeError('Token layouts disagree on exact token geometry')
                if source in overlaps[0]['sources']:
                    raise RuntimeError('Duplicate exact-token rows in one OCR pass')
                overlaps[0]['sources'].append(source)
            else:
                groups.append(dict(row=row, sources=[source]))
    return [group['row'] for group in groups], groups


def snapshot(smoke, label, baseline, present, *, health_fallback=False, token_psm=6,
             health_psm=6, health_footer=False, after_capture=None):
    if after_capture is not None and (not callable(after_capture) or present or baseline is None):
        raise RuntimeError('Input finalization requires a final absence capture')
    if health_psm not in (6, 11):
        raise RuntimeError('Unsupported health anchor layout')
    if token_psm not in (6,11) or (token_psm==11 and (not present or baseline is None)):
        raise RuntimeError('Alternate token layout requires a current-cycle presence baseline')
    def geometry(suffix):
        result = smoke.run(['swaymsg', '-t', 'get_tree', '-r'], capture_output=True, text=True)
        (smoke.out / (label + suffix + '-tree.json')).write_text(result.stdout)
        return owned_rectangle(json.loads(result.stdout), smoke.app.pid)
    rectangle = geometry('-before')
    if baseline is not None and rectangle != baseline['rectangle']:
        raise RuntimeError('App rectangle or identity changed during visual oracle')
    outputs = smoke.run(['swaymsg', '-t', 'get_outputs', '-r'], capture_output=True, text=True)
    (smoke.out / (label + '-outputs.json')).write_text(outputs.stdout)
    active = [item for item in json.loads(outputs.stdout) if item.get('active')]
    if (len(active) != 1 or active[0].get('scale') != 1 or
            active[0].get('transform') != 'normal' or active[0].get('rect') != OUTPUT_RECT):
        raise RuntimeError('Visual preedit fixture requires its verified scale-one output')
    smoke.screenshot(label)
    if geometry('-after') != rectangle:
        raise RuntimeError('App geometry changed across screenshot')
    image = smoke.out / (label + '.png')
    dimensions = smoke.run(['identify', '-format', '%w %h', str(image)],
                           capture_output=True, text=True).stdout
    if dimensions != '1280 900':
        raise RuntimeError('Unexpected screenshot dimensions or image count')
    image_sha256 = hashlib.sha256(image.read_bytes()).hexdigest()
    # All input is complete and the final image is now captured with
    # owned geometry and immutable provenance. End/drain the input collector
    # before passive OCR; no later input is allowed until analysis succeeds.
    if after_capture is not None:
        after_capture()
        if hashlib.sha256(image.read_bytes()).hexdigest() != image_sha256:
            raise RuntimeError('Screenshot changed while finalizing input')
    rows = smoke.words(label, psm=6, capture=False)
    validate_rows(rows)
    # Layout is chosen by the caller before capture, never as a response to OCR
    # content. Primary rows remain validated; only rendered presence may use 11.
    token_rows = rows
    token_region = None
    token_observations = [('whole-image-psm6', rows)]
    legacy_token_pair = None
    if REFERENCE_SHELL_HEALTH:
        token_rows, token_region = reference_token_rows(smoke,label,image,rectangle,token_psm)
        token_observations.append((f'whole-native-canvas-psm{token_psm}', token_rows))
    if token_psm == 11:
        if not REFERENCE_SHELL_HEALTH:
            token_rows = smoke.words(label, psm=11, capture=False)
        validate_rows(token_rows)
        primary = reference_token_rows(smoke,label,image,rectangle,6)[0] if REFERENCE_SHELL_HEALTH else rows
        primary_tokens = exact_matches(primary, rectangle)
        selected_tokens = exact_matches(token_rows, rectangle)
        if not (REFERENCE_SHELL_HEALTH and REFERENCE_INLINE_BODY) and (len(primary_tokens) > 1 or (primary_tokens and
                [box(r) for r in primary_tokens] != [box(r) for r in selected_tokens])):
            raise RuntimeError('Token layouts disagree on exact token geometry')
        if REFERENCE_SHELL_HEALTH and REFERENCE_INLINE_BODY:
            legacy_token_pair = primary_tokens, selected_tokens
            token_observations.append(('whole-native-canvas-psm6', primary))
    # The footer is sparse text regardless of the terminal's content layout.
    # Callers may select that layout before capture, for the entire lifecycle.
    health_rows = rows
    if health_psm == 11:
        health_rows = token_rows if token_psm == 11 else smoke.words(label, psm=11, capture=False)
        validate_rows(health_rows)
        if not present and exact_matches(health_rows, rectangle):
            raise RuntimeError('Sparse health layout still observes preedit token')
    footer_evidence = None
    health_ocr_psm = health_psm
    if health_footer:
        if REFERENCE_SHELL_HEALTH:
            footer_evidence = reference_header_health(smoke, label, image, rectangle)
            health_ocr_psm = None
        else:
            health_rows, footer_evidence = footer_health(smoke, label, image, rectangle)
            health_ocr_psm = 7
    native_anchor = footer_evidence['rectangle'] if REFERENCE_SHELL_HEALTH and health_footer else None
    anchors = [] if native_anchor is not None else exact_matches(health_rows, rectangle, HEALTH_ANCHOR)
    if (not health_footer and not anchors and health_fallback
            and health_psm == 6 and baseline is not None):
        # One alternate layout of the SAME immutable PNG, never a recapture or
        # an input retry. A known exact baseline box is required. Ambiguous or
        # displaced primary anchors cannot fall through to another analysis.
        alternate = smoke.words(label, psm=11, capture=False)
        validate_rows(alternate)
        anchors = exact_matches(alternate, rectangle, HEALTH_ANCHOR)
        if len(exact_matches(alternate, rectangle)) != int(present):
            raise RuntimeError('Alternate layout disagrees with preedit token count')
        health_ocr_psm = 11
    if hashlib.sha256(image.read_bytes()).hexdigest() != image_sha256:
        raise RuntimeError('Screenshot changed during OCR analysis')
    if native_anchor is None and len(anchors) != 1:
        raise RuntimeError('Missing or ambiguous exact OCR health anchor')
    anchor_box = native_anchor if native_anchor is not None else box(anchors[0])
    if baseline is not None and anchor_box != baseline['anchor_box']:
        raise RuntimeError('Native/OCR health anchor moved')
    body_evidence = token_groups = None
    body_contract = None
    spatial_witnesses = []
    native_plane_evidence = None
    if REFERENCE_SHELL_HEALTH and REFERENCE_INLINE_BODY:
        body_contract = inline_body_fixture_contract(smoke.config)
        body_observations, body_evidence = reference_body_observations(smoke, label, image, rectangle, body_contract)
        if legacy_token_pair is not None:
            require_legacy_token_geometry(*legacy_token_pair, rectangle, body_observations)
        token_observations.extend(body_observations)
        matches, token_groups = merge_exact_token_observations(token_observations, rectangle)
        registered = [word for _, body_rows in token_observations[1:]
                      for word in exact_matches(body_rows, rectangle)]
        for match in matches:
            if not any(box(word) == box(match) for word in registered):
                if native_plane_evidence is None:
                    canvas, planes, pixels_digest = reference_native_plane_witnesses(smoke, image, rectangle, body_contract)
                    native_plane_evidence = dict(canvas=canvas, planes=planes, canvas_rgb_sha256=pixels_digest,
                                                 scope='independent geometry witness only; no extra multi-line OCR')
                plane = require_native_plane_witness(match, rectangle, native_plane_evidence['canvas'],
                                                     native_plane_evidence['planes'])
                spatial_witnesses.append(dict(word=box(match), plane=plane))
    else:
        matches = exact_matches(token_rows, rectangle)
    if hashlib.sha256(image.read_bytes()).hexdigest() != image_sha256:
        raise RuntimeError('Screenshot changed during native-body analysis')
    if len(matches) != int(present):
        raise RuntimeError(f'Expected {int(present)} exact preedit token, observed {len(matches)}')
    result = {'rectangle': rectangle, 'anchor_box': anchor_box, 'image': str(image),
              'expected_token_count': int(present), 'observed_token_count': len(matches),
              'matches': matches, 'image_sha256': image_sha256, 'health_ocr_psm': health_ocr_psm,
              'token_ocr_psm': token_psm}
    if token_region is not None:
        result['token_region'] = token_region
    if body_evidence is not None:
        result['native_body_regions'] = body_evidence
        result['token_observation_groups'] = token_groups
        result['native_body_contract'] = body_contract
        result['whole_image_native_plane_witnesses'] = spatial_witnesses
        result['native_plane_geometry'] = native_plane_evidence
    if footer_evidence is not None:
        result['health_header' if REFERENCE_SHELL_HEALTH else 'health_footer'] = footer_evidence
    (smoke.out / (label + '-visual.json')).write_text(json.dumps(result, indent=2))
    return result


def verify_restoration(smoke, before, during, after):
    """Compare pixel bytes, never PNG metadata; no cursor masks or tolerances."""
    if (before['observed_token_count'] != 0 or during['observed_token_count'] != 1 or
            after['observed_token_count'] != 0 or len(during['matches']) != 1):
        raise RuntimeError('Invalid preedit lifecycle for pixel comparison')
    if any(item['rectangle'] != before['rectangle'] or item['anchor_box'] != before['anchor_box']
           for item in (during, after)):
        raise RuntimeError('Pixel comparison requires stable ownership and anchor')
    region = box(during['matches'][0])
    if not contained(region, before['rectangle']['rect']):
        raise RuntimeError('Pixel comparison region outside owned surface')
    crop = f"{region['width']}x{region['height']}+{region['x']}+{region['y']}"
    pixels = []
    for label, item in [('before', before), ('during', during), ('after', after)]:
        raw = smoke.run(['convert', item['image'], '-crop', crop, '+repage',
                         '-colorspace', 'sRGB', '-alpha', 'off', '-depth', '8', 'RGB:-'],
                        capture_output=True).stdout
        if len(raw) != region['width'] * region['height'] * 3:
            raise RuntimeError('Wrong raw RGB crop byte count')
        (smoke.out / ('preedit-region-' + label + '.rgb')).write_bytes(raw)
        pixels.append(raw)
    result = {'region': region, 'format': 'sRGB RGB 8-bit, row-major, no alpha',
              'sha256': dict(zip(('before', 'during', 'after'),
                                [hashlib.sha256(raw).hexdigest() for raw in pixels])),
              'during_differs': pixels[1] != pixels[0], 'after_restored': pixels[2] == pixels[0]}
    (smoke.out / 'preedit-region-comparison.json').write_text(json.dumps(result, indent=2))
    if not result['during_differs'] or not result['after_restored']:
        raise RuntimeError('Preedit region did not change and restore exact baseline pixels')
    return result
