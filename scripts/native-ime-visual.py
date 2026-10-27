"""Fail-closed rendered-preedit evidence in a stable owned scale-one surface."""
import hashlib
import json
import math
import os
import re

PREEDIT_TOKEN = 'EMBERPREEDITMARKER'
HEALTH_ANCHOR = 'master'
REFERENCE_SHELL_HEALTH = False
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
    matches = re.findall(rb'^preedit-switch-ready:([0-9]+):(/dev/pts/[0-9]+)$', transcript, re.M)
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
    if REFERENCE_SHELL_HEALTH:
        token_rows, token_region = reference_token_rows(smoke,label,image,rectangle,token_psm)
    if token_psm == 11:
        if not REFERENCE_SHELL_HEALTH:
            token_rows = smoke.words(label, psm=11, capture=False)
        validate_rows(token_rows)
        primary = reference_token_rows(smoke,label,image,rectangle,6)[0] if REFERENCE_SHELL_HEALTH else rows
        primary_tokens = exact_matches(primary, rectangle)
        selected_tokens = exact_matches(token_rows, rectangle)
        if len(primary_tokens) > 1 or (primary_tokens and
                [box(r) for r in primary_tokens] != [box(r) for r in selected_tokens]):
            raise RuntimeError('Token layouts disagree on exact token geometry')
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
    matches = exact_matches(token_rows, rectangle)
    if len(matches) != int(present):
        raise RuntimeError(f'Expected {int(present)} exact preedit token, observed {len(matches)}')
    result = {'rectangle': rectangle, 'anchor_box': anchor_box, 'image': str(image),
              'expected_token_count': int(present), 'observed_token_count': len(matches),
              'matches': matches, 'image_sha256': image_sha256, 'health_ocr_psm': health_ocr_psm,
              'token_ocr_psm': token_psm}
    if token_region is not None:
        result['token_region'] = token_region
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
