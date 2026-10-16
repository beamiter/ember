"""Fail-closed rendered-preedit evidence in a stable owned scale-one surface."""
import hashlib
import json
import math

PREEDIT_TOKEN = 'EMBERPREEDITMARKER'
HEALTH_ANCHOR = 'master'
OUTPUT_RECT = {'x': 0, 'y': 0, 'width': 1280, 'height': 900}


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


def snapshot(smoke, label, baseline, present):
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
    rows = smoke.words(label, psm=6, capture=False)
    validate_rows(rows)
    anchors = exact_matches(rows, rectangle, HEALTH_ANCHOR)
    if len(anchors) != 1:
        raise RuntimeError('Missing or ambiguous exact OCR health anchor')
    anchor_box = box(anchors[0])
    if baseline is not None and anchor_box != baseline['anchor_box']:
        raise RuntimeError('OCR health anchor moved')
    matches = exact_matches(rows, rectangle)
    if len(matches) != int(present):
        raise RuntimeError(f'Expected {int(present)} exact preedit token, observed {len(matches)}')
    result = {'rectangle': rectangle, 'anchor_box': anchor_box, 'image': str(image),
              'expected_token_count': int(present), 'observed_token_count': len(matches),
              'matches': matches}
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
