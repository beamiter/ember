"""Registered glass-v1 test observation domains; no OCR, input or product state.

These are explicit fixture domains from the committed native layout. They are
not recovered widget bounds or an alpha/blur measurement. Each caller still
checks actual owned-window geometry and immutable source pixels independently.
"""
import hashlib
import json
from pathlib import Path
import re
import tomllib

NAME = 'glass-v1'
ROOT = Path(__file__).resolve().parents[1]


def selected(smoke):
    value = getattr(smoke, 'native_ui_profile', 'legacy-reference')
    if value not in ('legacy-reference', NAME):
        raise RuntimeError('Unregistered native UI observation profile')
    return value == NAME


def fixture_mode(smoke):
    value = tomllib.loads(smoke.config.read_text()).get('terminal_mode', 'block')
    if value == 'normal':
        value = 'block'
    if value not in ('block', 'chat'):
        raise RuntimeError('Unregistered glass fixture presentation')
    return value


def owned_window(smoke):
    """Read-only actual identity/focus/geometry; never activate a window."""
    if smoke.app.poll() is not None:
        raise RuntimeError('Glass application exited before observation')
    if smoke.backend == 'wayland':
        tree = json.loads(smoke.run(['swaymsg', '-t', 'get_tree', '-r'],
                                    capture_output=True, text=True).stdout)
        matches = []
        def visit(node):
            if node.get('app_id') == 'io.github.beamiter.ember':
                matches.append(node)
            for child in node.get('nodes', []) + node.get('floating_nodes', []):
                visit(child)
        visit(tree)
        if (len(matches) != 1 or matches[0].get('pid') != smoke.app.pid
                or matches[0].get('focused') is not True):
            raise RuntimeError('Glass owned window identity/focus mismatch')
        record = dict(id=matches[0]['id'], pid=smoke.app.pid, rect=matches[0]['rect'])
    elif smoke.backend == 'x11':
        window = str(smoke.recovery_window)
        pid = smoke.run(['xdotool', 'getwindowpid', window], capture_output=True, text=True).stdout.strip()
        focused = smoke.run(['xdotool', 'getactivewindow'], capture_output=True, text=True).stdout.strip()
        if pid != str(smoke.app.pid) or focused != window:
            raise RuntimeError('Glass owned window identity/focus mismatch')
        geometry = smoke.run(['xdotool', 'getwindowgeometry', '--shell', window],
                             capture_output=True, text=True).stdout
        fields = dict(line.split('=', 1) for line in geometry.splitlines() if '=' in line)
        try:
            rect = {key: int(fields[field]) for key, field in
                    [('x','X'),('y','Y'),('width','WIDTH'),('height','HEIGHT')]}
        except (KeyError, ValueError) as error:
            raise RuntimeError('Invalid glass X11 window geometry') from error
        record = dict(id=window, pid=smoke.app.pid, rect=rect)
    else:
        raise RuntimeError('Unregistered glass observation backend')
    layout(record['rect'])
    return record


def contained(inner, outer):
    return (inner['width'] > 0 and inner['height'] > 0 and
            inner['x'] >= outer['x'] and inner['y'] >= outer['y'] and
            inner['x'] + inner['width'] <= outer['x'] + outer['width'] and
            inner['y'] + inner['height'] <= outer['y'] + outer['height'])


def registration():
    source = ROOT / 'src/glass_shell_tokens.rs'
    data = source.read_bytes()
    tokens = {name: float(value) for name, value in re.findall(
        r'^pub const (\w+): f32 = ([0-9.]+);$', data.decode(), re.M)}
    names = ('TOPBAR_HEIGHT', 'MAIN_TOP', 'OUTER_LEFT', 'OUTER_RIGHT',
             'RAIL_WIDTH', 'NAVIGATION_WIDTH', 'LEFT_GAP', 'INSPECTOR_WIDTH',
             'INSPECTOR_GAP', 'MAIN_BOTTOM_GAP', 'HEADER_HEIGHT',
             'TERMINAL_INSET_LEFT', 'TERMINAL_INSET_RIGHT', 'TERMINAL_TOP_GAP',
             'TERMINAL_FOOTER_GAP', 'TOOLBAR_HEIGHT', 'INSPECTOR_DRAWER_BREAKPOINT',
             'SHORT_HEIGHT', 'SHORT_HEADER_HEIGHT', 'SHORT_BOTTOM_GAP',
             'TOOL_BUTTON_HEIGHT')
    if any(name not in tokens or not tokens[name].is_integer() for name in names):
        raise RuntimeError('Unregistered nonintegral glass geometry tokens')
    accent = re.search(r'^pub const ACCENT_TOP: \[u8; 4\] = \[([^]]+)\];$', data.decode(), re.M)
    if accent is None or [int(v.strip()) for v in accent[1].split(',')] != [62,108,255,255]:
        raise RuntimeError('Unregistered opaque glass selected-control palette')
    return {name: int(tokens[name]) for name in names}, {
        'profile': NAME, 'tokens_source': str(source.relative_to(ROOT)),
        'tokens_sha256': hashlib.sha256(data).hexdigest(),
        'layout_sources_sha256': {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest()
                                 for p in (ROOT/'src/glass_shell.rs', ROOT/'src/reference_workspace.rs')},
        'scope': 'Registered native-layout observation domains, not measured allocation/alpha',
    }


def layout(owned):
    if (set(owned) != {'x', 'y', 'width', 'height'} or
            any(type(value) is not int for value in owned.values()) or
            min(owned['x'], owned['y']) < 0 or
            owned['width'] < 700 or owned['height'] < 400):
        raise RuntimeError('Unregistered glass fixture window geometry')
    t, source = registration()
    width, height = owned['width'], owned['height']
    rail, nav = t['RAIL_WIDTH'], t['NAVIGATION_WIDTH']
    docked = width >= t['INSPECTOR_DRAWER_BREAKPOINT']
    available = max(0, width - t['OUTER_LEFT'] - rail - t['LEFT_GAP']
                    - t['OUTER_RIGHT'] - 300 - (t['INSPECTOR_GAP'] if docked else 0))
    if available < 180 + (260 if docked else 0):
        nav = 0
    else:
        nav = min(nav, max(180, available - (260 if docked else 0)))
    x = t['OUTER_LEFT'] + rail + nav + t['LEFT_GAP']
    inspector = (min(t['INSPECTOR_WIDTH'], max(260, available - nav))
                 + t['INSPECTOR_GAP']) if docked else 0
    right = width - t['OUTER_RIGHT'] - inspector
    short = height < t['SHORT_HEIGHT']
    bottom = height - (t['SHORT_BOTTOM_GAP'] if short else t['MAIN_BOTTOM_GAP'])
    header = t['SHORT_HEADER_HEIGHT'] if short else t['HEADER_HEIGHT']
    def rect(a, b, c, d):
        r = dict(x=owned['x'] + a, y=owned['y'] + b, width=c, height=d)
        if not contained(r, owned):
            raise RuntimeError('Glass observation domain outside owned window')
        return r
    center = rect(x, t['MAIN_TOP'] + header, right - x, bottom - t['MAIN_TOP'] - header)
    # The real projected input may extend into the toolbar's quiet top inset.
    # Observe through the start of actual controls, not the raw PTY bottom.
    # The compact native toolbar uses the same source control height and bottom
    # inset as the renderer. Source registration records both geometry owners.
    # This fixed input+output domain never follows an OCR word.
    adapter = (ROOT / 'src/glass_shell.rs').read_text()
    control_bottom = re.search(r'^pub const COMPOSER_CONTROL_BOTTOM: f32 = ([0-9.]+);$', adapter, re.M)
    if control_bottom is None or not float(control_bottom[1]).is_integer():
        raise RuntimeError('Unregistered glass toolbar control inset')
    body_y = t['MAIN_TOP'] + header + t['TERMINAL_TOP_GAP']
    body = rect(x + t['TERMINAL_INSET_LEFT'], body_y,
                right - x - t['TERMINAL_INSET_LEFT'] - t['TERMINAL_INSET_RIGHT'],
                bottom - int(float(control_bottom[1])) - t['TOOL_BUTTON_HEIGHT'] - body_y)
    terminal = dict(body, height=bottom - t['TOOLBAR_HEIGHT'] - t['TERMINAL_FOOTER_GAP'] - body_y)
    if terminal['height'] <= 0:
        raise RuntimeError('No registered glass native terminal area')
    return dict(header=rect(0, 0, width, t['TOPBAR_HEIGHT']), center=center,
                native_body=body, terminal=terminal, metadata=source)


def mode_slot(owned, mode):
    if mode not in ('block', 'chat'):
        raise RuntimeError('Unregistered glass mode for paint witness')
    layout(owned)
    settings_x = owned['width'] - 8 - 32 - 6 - 32
    x = settings_x - 8 - 64 - (70 if mode == 'block' else 0)
    return dict(x=owned['x']+x, y=owned['y']+5, width=64, height=32)


def header_paint(image, owned, mode=None):
    """Positive opaque selected-control paint; independent of desktop RGB.

    Scan only two preregistered slots, not token text. Exactly one must have
    the source's selected blue surface. No alpha tolerance or pixel repair.
    """
    domains = layout(owned)
    if owned['x']+owned['width'] > image.width or owned['y']+owned['height'] > image.height:
        raise RuntimeError('Glass owned window outside screenshot')
    observed = []
    records = {}
    for candidate in ('block', 'chat'):
        r = mode_slot(owned, candidate)
        crop = image.convert('RGB').crop((r['x'], r['y'], r['x']+r['width'], r['y']+r['height']))
        count = sum(pixel == (62,108,255) for pixel in crop.getdata())
        area = r['width']*r['height']
        records[candidate] = dict(rectangle=r, exact_selected_blue=count, area=area)
        if count > area*0.95:
            raise RuntimeError('Malformed overfull glass selected-control paint')
        if area*0.5 <= count:
            observed.append(candidate)
    if len(observed) != 1 or (mode is not None and observed != [mode]):
        raise RuntimeError('Absent, ambiguous or wrong-mode glass header paint')
    return dict(profile=NAME, rectangle=domains['header'], selected_mode=observed[0],
                mode_slots=records, registration=domains['metadata'],
                scope='Owned immutable native header paint; not backdrop alpha/blur or frame ack')
