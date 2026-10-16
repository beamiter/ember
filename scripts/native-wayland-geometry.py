"""Measured single-output geometry for an isolated public-protocol pointer."""
from fractions import Fraction
import json
import math

PHYSICAL = (2400, 1800)
SCALES = (Fraction(1), Fraction(3, 2), Fraction(2))


def rect(value):
    keys = ('x', 'y', 'width', 'height')
    if not isinstance(value, dict) or any(type(value.get(k)) is not int for k in keys):
        raise RuntimeError('Non-integral geometry')
    result = {k: value[k] for k in keys}
    if result['width'] <= 0 or result['height'] <= 0:
        raise RuntimeError('Non-positive geometry')
    return result


def contains(outer, inner):
    return (inner['x'] >= outer['x'] and inner['y'] >= outer['y'] and
            inner['x'] + inner['width'] <= outer['x'] + outer['width'] and
            inner['y'] + inner['height'] <= outer['y'] + outer['height'])


def layout(outputs, tree, pid, scale, expected_rect, floating):
    scale = Fraction(str(scale))
    if scale not in SCALES:
        raise RuntimeError('Unsupported requested scale')
    active = [o for o in outputs if o.get('active') is True]
    if len(active) != 1:
        raise RuntimeError('Expected exactly one active output')
    output = active[0]
    if (Fraction(str(output.get('scale'))) != scale or output.get('transform') != 'normal' or
            output.get('current_mode', {}).get('width') != PHYSICAL[0] or
            output.get('current_mode', {}).get('height') != PHYSICAL[1]):
        raise RuntimeError('Actual output mode, transform or scale mismatch')
    extent = rect(output['rect'])
    if (extent['width'] * scale, extent['height'] * scale) != PHYSICAL:
        raise RuntimeError('Physical/logical output extent mismatch')
    matches = []
    def visit(node, is_floating=False):
        if node.get('app_id') == 'io.github.beamiter.ember':
            matches.append((node, is_floating))
        for child in node.get('nodes', []):
            visit(child, is_floating)
        for child in node.get('floating_nodes', []):
            visit(child, True)
    visit(tree)
    if len(matches) != 1:
        raise RuntimeError('Absent or ambiguous app surface')
    node, actual_floating = matches[0]
    owned = rect(node['rect'])
    if (node.get('pid') != pid or node.get('focused') is not True or
            type(node.get('id')) is not int or node.get('border') != 'none' or
            actual_floating != floating or owned != rect(expected_rect) or not contains(extent, owned)):
        raise RuntimeError('Owned window identity, focus or exact geometry mismatch')
    return {'output_name': output['name'], 'scale': str(scale), 'output_rect': extent,
            'physical': list(PHYSICAL), 'node_id': node['id'], 'pid': pid,
            'app_rect': owned, 'floating': actual_floating}


def observe(smoke, label, scale, expected_rect, floating):
    values = {}
    for kind in ('outputs', 'tree'):
        result = smoke.run(['swaymsg', '-t', 'get_' + kind, '-r'], capture_output=True, text=True)
        (smoke.out / (label + '-' + kind + '.json')).write_text(result.stdout)
        values[kind] = json.loads(result.stdout)
    return layout(values['outputs'], values['tree'], smoke.app.pid, scale, expected_rect, floating)


def capture(smoke, label, scale, expected_rect, floating, expected=None):
    before = observe(smoke, label + '-before', scale, expected_rect, floating)
    if expected is not None and before != expected:
        raise RuntimeError('Stale layout before capture')
    smoke.screenshot(label)
    after = observe(smoke, label + '-after', scale, expected_rect, floating)
    if before != after:
        raise RuntimeError('Layout changed across screenshot')
    path = smoke.out / (label + '.png')
    dimensions = smoke.run(['identify', '-format', '%w %h', str(path)], capture_output=True, text=True).stdout
    if dimensions != '2400 1800':
        raise RuntimeError('Unexpected actual screenshot dimensions')
    return before


def word_box(row):
    try:
        confidence = float(row['conf'])
        if row['level'] != '5' or not isinstance(row['text'], str) or not row['text'].strip() or not math.isfinite(confidence) or not 0 <= confidence <= 100:
            raise ValueError('Invalid OCR word')
        pixels = rect(dict(zip(('x', 'y', 'width', 'height'),
                               [int(row[k]) for k in ('left', 'top', 'width', 'height')])))
        if not contains({'x': 0, 'y': 0, 'width': PHYSICAL[0], 'height': PHYSICAL[1]}, pixels):
            raise RuntimeError('OCR word outside screenshot')
        return pixels
    except (KeyError, TypeError, ValueError) as error:
        raise RuntimeError('Malformed OCR word') from error


def logical_box(row, measured):
    pixels = word_box(row)
    if not contains({'x': 0, 'y': 0, 'width': PHYSICAL[0], 'height': PHYSICAL[1]}, pixels):
        raise RuntimeError('OCR word outside screenshot')
    scale = Fraction(measured['scale'])
    output = measured['output_rect']
    converted = {'x': output['x'] + Fraction(pixels['x'], 1) / scale,
                 'y': output['y'] + Fraction(pixels['y'], 1) / scale,
                 'width': Fraction(pixels['width'], 1) / scale,
                 'height': Fraction(pixels['height'], 1) / scale}
    if not contains(measured['app_rect'], converted):
        raise RuntimeError('OCR word crosses or lies outside owned app')
    return converted


def exact_words(rows, text, measured):
    if not rows:
        raise RuntimeError('Empty OCR evidence')
    matches = []
    for row in rows:
        word_box(row)  # malformed non-matches cannot make a healthy screenshot
        if row['text'] == text:
            logical_box(row, measured)
            matches.append(row)
    return matches


def pointer_target(row, measured):
    logical = logical_box(row, measured)
    # Floor the exact rational center, then prove the representable coordinate
    # remains strictly inside the recognized word; never clamp an edge.
    x = math.floor(logical['x'] + logical['width'] / 2)
    y = math.floor(logical['y'] + logical['height'] / 2)
    if not (logical['x'] < x < logical['x'] + logical['width'] and
            logical['y'] < y < logical['y'] + logical['height']):
        raise RuntimeError('No safe integer pointer point inside word')
    output = measured['output_rect']
    px, py = x - output['x'], y - output['y']
    if not (0 <= px < output['width'] and 0 <= py < output['height']):
        raise RuntimeError('Pointer point outside actual output layout')
    return (px, py, output['width'], output['height'])


def click(smoke, label, row, measured, scale, expected_rect, floating):
    target = pointer_target(row, measured)
    current = observe(smoke, label + '-preclick', scale, expected_rect, floating)
    if current != measured:
        raise RuntimeError('Stale captured geometry before pointer input')
    if smoke.pointer.poll() is not None:
        raise RuntimeError('Owned pointer exited before click')
    command = 'click ' + ' '.join(map(str, target)) + '\n'
    (smoke.out / (label + '-pointer.json')).write_text(json.dumps({
        'measured': measured, 'ocr_row': row, 'command': command}, indent=2))
    smoke.pointer.stdin.write(command)
    smoke.pointer.stdin.flush()
    smoke.pointer_ack('done')
    if observe(smoke, label + '-postclick', scale, expected_rect, floating) != measured:
        raise RuntimeError('Geometry changed across pointer input')
    return target


def shares_row(a, b):
    a, b = word_box(a), word_box(b)
    return max(a['y'], b['y']) < min(a['y'] + a['height'], b['y'] + b['height'])


def remote_tab(rows, measured):
    anchors = []
    for text in ('Appearance', 'Advanced'):
        found = exact_words(rows, text, measured)
        if len(found) != 1:
            raise RuntimeError('Absent or ambiguous Settings tab anchor')
        anchors.append(found[0])
    matches = [r for r in exact_words(rows, 'Remote', measured)
               if all(shares_row(r, anchor) for anchor in anchors)]
    if len(matches) != 1:
        raise RuntimeError('Absent or ambiguous Remote tab')
    if exact_words(rows, 'Hosts', measured):
        raise RuntimeError('Remote content already visible before positive-control click')
    return matches[0]


def remote_heading(rows, measured):
    hosts = exact_words(rows, 'Hosts', measured)
    if len(hosts) != 1:
        raise RuntimeError('Absent or ambiguous Remote Hosts heading')
    remote = [r for r in exact_words(rows, 'Remote', measured) if shares_row(r, hosts[0])]
    if len(remote) != 1 or word_box(remote[0])['x'] >= word_box(hosts[0])['x']:
        raise RuntimeError('Remote Hosts heading ordering/row mismatch')
    return [remote[0], hosts[0]]
