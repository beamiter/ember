"""Owned-client configure/ack/commit barrier for one-time Sway setup commands."""
import json
import re
import subprocess
import time


def complete_lines(text):
    if len(text) > 16 * 1024 * 1024:
        raise RuntimeError('Protocol trace exceeds bounded setup evidence size')
    return text.splitlines() if text.endswith('\n') else text.splitlines()[:-1]


def committed_geometry(text, since=0):
    lines = complete_lines(text)
    surfaces, tops, apps = {}, {}, set()
    bindings, app_ids = {}, {}
    normalized = [line.replace('#', '@') for line in lines]
    for index, line in enumerate(normalized):
        match = re.search(r'get_xdg_surface\(new id xdg_surface@(\d+), wl_surface@(\d+)\)', line)
        if match:
            surfaces[match[1]] = match[2]
            bindings.setdefault(('xdg_surface', match[1]), []).append(index)
        match = re.search(r'xdg_surface@(\d+)\.get_toplevel\(new id xdg_toplevel@(\d+)\)', line)
        if match:
            tops[match[2]] = match[1]
            bindings.setdefault(('xdg_toplevel', match[2]), []).append(index)
        match = re.search(r'xdg_toplevel@(\d+)\.set_app_id\("([^"\n]*)"\)', line)
        if match:
            app_ids.setdefault(match[1], []).append((index, match[2]))
            if match[2] == 'io.github.beamiter.ember':
                apps.add(match[1])
        match = re.search(r'create_surface\(new id wl_surface@(\d+)\)', line)
        if match:
            bindings.setdefault(('wl_surface', match[1]), []).append(index)
    if len(apps) > 1:
        raise RuntimeError('Ambiguous Ember protocol surface')
    if not apps:
        return None
    top = next(iter(apps))
    surface = tops.get(top)
    wl = surfaces.get(surface)
    if surface is None or wl is None:
        return None
    chain = [('wl_surface', wl), ('xdg_surface', surface), ('xdg_toplevel', top)]
    if any(len(bindings.get(key, [])) != 1 for key in chain):
        raise RuntimeError('Missing or rebound owned protocol object')
    if len(app_ids[top]) != 1:
        raise RuntimeError('Owned protocol app_id changed or rebound')
    app_line = app_ids[top][0][0]
    creation = [bindings[key][0] for key in chain] + [app_line]
    if creation != sorted(set(creation)):
        raise RuntimeError('Owned protocol object creation order mismatch')
    for line in normalized:
        if any(re.search(kind + '@' + number + r'\.destroy\(', line) for kind, number in chain):
            raise RuntimeError('Owned protocol object destroyed during setup')
        if any(re.search(r'wl_display@\d+\.delete_id\(' + number + r'\)', line) for _, number in chain):
            raise RuntimeError('Owned protocol object ID released during setup')
    proposed = None
    proposed_line = -1
    latest = None
    committed = None
    current_geometry = None
    pending_geometry = None
    for index, line in enumerate(normalized):
        if index <= app_line:
            continue
        match = re.search(r'xdg_toplevel@' + top + r'\.configure\((\d+), (\d+), array\[\d+\]\)', line)
        if match:
            proposed = (int(match[1]), int(match[2]))
            proposed_line = index
            latest = None
            committed = None
        match = re.search(r'xdg_surface@' + surface + r'\.configure\((\d+)\)', line)
        if match:
            latest = {'serial': int(match[1]), 'size': proposed, 'configure_line': index,
                      'acked': False}
            committed = None
        match = re.search(r'-> xdg_surface@' + surface + r'\.ack_configure\((\d+)\)', line)
        if match and latest is not None and int(match[1]) == latest['serial']:
            latest['acked'] = True
            latest['ack_line'] = index
        geometry_request = r'-> xdg_surface@' + surface + r'\.set_window_geometry\('
        if re.search(geometry_request, line):
            match = re.search(geometry_request + r'(-?\d+), (-?\d+), (-?\d+), (-?\d+)\)', line)
            if match is None:
                raise RuntimeError('Malformed owned window geometry')
            x, y, width, height = map(int, match.groups())
            if (x, y) != (0, 0) or min(width, height) <= 0:
                raise RuntimeError('Unsupported owned window geometry')
            pending_geometry = (width, height)
            committed = None
        if re.search(r'-> wl_surface@' + wl + r'\.commit\(\)', line):
            # xdg_surface geometry is double-buffered persistent surface state,
            # not configure-local state. A same-size configure need not set it again.
            if pending_geometry is not None:
                current_geometry = pending_geometry
                pending_geometry = None
            if latest is None or not latest['acked']:
                continue
            size = latest['size']
            actual = current_geometry
            if (size is not None and actual is not None and min(actual) > 0 and
                    all(want == 0 or want == got for want, got in zip(size, actual)) and
                    proposed_line >= since and latest['configure_line'] >= since and committed is None):
                committed = {'serial': latest['serial'], 'width': actual[0], 'height': actual[1],
                             'configure_line': latest['configure_line'], 'ack_line': latest['ack_line'],
                             'commit_line': index, 'wl_surface': wl, 'xdg_surface': surface, 'toplevel': top}
    return committed


def owned_node(tree, pid):
    matches = []
    def visit(node, floating=False):
        if node.get('app_id') == 'io.github.beamiter.ember':
            matches.append((node, floating))
        for child in node.get('nodes', []):
            visit(child, floating)
        for child in node.get('floating_nodes', []):
            visit(child, True)
    visit(tree)
    if len(matches) != 1 or matches[0][0].get('pid') != pid or matches[0][0].get('focused') is not True:
        raise RuntimeError('Setup protocol surface ownership/focus mismatch')
    return matches[0]


def wait_committed(smoke, label, deadline, since, floating, border=None, size=None, position=None):
    """Two agreeing read-only samples, one shared deadline; no mutation/replay."""
    previous = None
    while time.monotonic() < deadline:
        if smoke.app.poll() is not None:
            raise RuntimeError('Ember exited during configure handshake')
        trace = (smoke.out / 'ember.log').read_text()
        protocol = committed_geometry(trace, since)
        reply = subprocess.run(['swaymsg', '-t', 'get_tree', '-r'], env=smoke.env,
                               check=True, capture_output=True, text=True,
                               timeout=max(0.001, deadline - time.monotonic()))
        tree = json.loads(reply.stdout)
        (smoke.out / (label + '-tree.json')).write_text(reply.stdout)
        node, is_floating = owned_node(tree, smoke.app.pid)
        rect, window = node['rect'], node['window_rect']
        latest_protocol = committed_geometry((smoke.out / 'ember.log').read_text(), since)
        valid = (protocol is not None and protocol == latest_protocol and is_floating == floating and
                 (border is None or node.get('border') == border) and
                 (window['width'], window['height']) == (protocol['width'], protocol['height']) and
                 (size is None or (rect['width'], rect['height']) == size) and
                 (position is None or (rect['x'], rect['y']) == position))
        sample = {'protocol': protocol, 'node_id': node['id'], 'pid': node['pid'],
                  'rect': rect, 'window_rect': window, 'floating': is_floating, 'border': node.get('border')}
        with (smoke.out / (label + '-samples.jsonl')).open('a') as log:
            log.write(json.dumps({'valid': valid, **sample}) + '\n')
        if valid and previous == sample:
            return sample
        previous = sample if valid else None
        time.sleep(min(0.1, max(0, deadline - time.monotonic())))
    raise RuntimeError('Timed out: committed configure handshake ' + label)
