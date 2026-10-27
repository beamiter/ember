#!/usr/bin/env python3
"""Hosted-only discovery of public AT-SPI on an isolated synthetic Ember instance."""
import argparse
import hashlib
import importlib.util
import json
import os
import signal
import subprocess
from pathlib import Path
import time

ACCESSIBLE = 'org.a11y.atspi.Accessible'
ROOT = '/org/a11y/atspi/accessible/root'
LIMIT = 512


def child_environment(generated):
    # No host DISPLAY, bus, backend, preload, Python path, credentials or proxies.
    keys = ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME',
            'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR', 'EMBER_SMOKE_SHELL_TRACE',
            'EMBER_SMOKE_CAPTURE', 'EMBER_SMOKE_RAW_ROOT', 'EMBER_SMOKE_SENTINELS')
    return dict({k: generated[k] for k in keys}, PATH='/usr/bin:/bin',
                LANG='C.UTF-8', LC_ALL='C.UTF-8', SHELL='/bin/bash',
                XDG_DATA_DIRS='/usr/local/share:/usr/share',
                LIBGL_ALWAYS_SOFTWARE='1', WGPU_BACKEND='vulkan',
                RUST_LOG='info', NO_AT_BRIDGE='0')


def validate_address(address, expected_bus):
    bus, path = map(str, address)
    if bus != expected_bus or not path.startswith('/org/a11y/atspi/accessible/'):
        raise RuntimeError('Foreign or invalid accessibility node')
    return bus, path


def validate_tree(nodes, root, owner, pid, expected_text='E03DISCOVERY'):
    # Entry is the public AT-SPI mapping for AccessKit TextInput (role 79).
    # GetState returns two uint32 bitfields; Editable is bit 7, not a list
    # of state enum values. EditableText mutation support is not required.
    by_path = {n['path']: n for n in nodes}
    if len(by_path) != len(nodes) or root not in by_path:
        raise RuntimeError('Missing root or duplicate tree path')
    pending = [(root, None)]; visited = set()
    while pending:
        path, parent = pending.pop()
        if path in visited or path not in by_path:
            raise RuntimeError('Invalid tree edge')
        visited.add(path)
        node = by_path[path]
        validate_address((node['owner'], path), owner)
        if node['pid'] != pid or node['parent'] != parent:
            raise RuntimeError('Foreign PID or inconsistent parent')
        pending.extend((child, path) for child in node['children'])
    if len(visited) != len(nodes):
        raise RuntimeError('Disconnected tree')
    if expected_text is None:
        return None  # Owned, connected tree only; no picker assertion.
    names = [n['name'] for n in nodes]
    for label in ('Search scope: All', 'Search scope: Cmd', 'Search scope: Out'):
        if names.count(label) != 1:
            raise RuntimeError('Missing or ambiguous picker scope: ' + label)
    editors = [n for n in nodes if 'org.a11y.atspi.Text' in n['interfaces']
               and n['role'] == 79 and len(n['states']) == 2
               and n['states'][0] & (1 << 7)
               and n.get('text') == expected_text]
    if len(editors) != 1:
        raise RuntimeError('Missing or ambiguous exact editable Entry')
    return editors[0]['path']


def save_tree(nodes, root, owner, pid, output, expected_text='E03DISCOVERY'):
    editor = validate_tree(nodes, root, owner, pid, expected_text)
    result = dict(pid=pid, owner=owner, root=root, nodes=nodes,
                  editor_path=editor,
                  picker_common_ancestor='unverified', focus_acceptance='unverified')
    output.write_text(json.dumps(result, indent=2))
    return result


def close_resources(daemon, smoke):
    errors = []
    try:
        # Signal the group while our original leader is live. Activated-service
        # group membership/exit remains unverified until hosted evidence exists.
        if daemon is not None and daemon.poll() is None:
            try:
                os.killpg(daemon.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass  # The group exited between poll and signal.
    except Exception as error:
        errors.append('private bus: ' + repr(error))
    finally:
        try:
            smoke.close()
        except Exception as error:
            errors.append('smoke: ' + repr(error))
    if errors:
        raise RuntimeError('; '.join(errors))


def discovery(pid, output, expected_text='E03DISCOVERY'):
    import dbus  # Official system package, only loaded in isolated child.
    session = dbus.SessionBus()
    bus_proxy = dbus.Interface(session.get_object('org.a11y.Bus', '/org/a11y/bus'), 'org.a11y.Bus')
    bus = dbus.bus.BusConnection(str(bus_proxy.GetAddress(timeout=3)))
    broker = dbus.Interface(bus.get_object('org.freedesktop.DBus', '/org/freedesktop/DBus'), 'org.freedesktop.DBus')
    registry = dbus.Interface(bus.get_object('org.a11y.atspi.Registry', ROOT), ACCESSIBLE)
    started = time.monotonic()
    last_error = None
    while time.monotonic() - started < 20:
        try:
            children = registry.GetChildren(timeout=3)
            if len(children) > 32:
                raise RuntimeError('Unexpected private desktop application count')
            owned = []
            for address in children:
                unique = str(broker.GetNameOwner(str(address[0]), timeout=3))
                if int(broker.GetConnectionUnixProcessID(unique, timeout=3)) == pid:
                    owned.append((unique, str(address[1])))
            if len(owned) != 1:
                raise RuntimeError('No unique PID-owned accessibility root')
            owner, root = owned[0]
            queue = [((owner, root), None)]; seen = set(); nodes = []
            while queue:
                if len(seen) >= LIMIT or time.monotonic() - started >= 20:
                    raise RuntimeError('Bounded accessibility discovery exhausted')
                raw_address, parent = queue.pop(0)
                address = validate_address(raw_address, owner)
                if address in seen:
                    raise RuntimeError('Cyclic or duplicate accessibility child')
                seen.add(address)
                obj = bus.get_object(*address)
                accessible = dbus.Interface(obj, ACCESSIBLE)
                props = dbus.Interface(obj, 'org.freedesktop.DBus.Properties')
                interfaces = list(map(str, accessible.GetInterfaces(timeout=3)))
                node = dict(path=address[1], parent=parent, owner=owner, pid=pid, name=str(props.Get(ACCESSIBLE, 'Name', timeout=3)),
                            description=str(props.Get(ACCESSIBLE, 'Description', timeout=3)),
                            role=int(accessible.GetRole(timeout=3)),
                            states=list(map(int, accessible.GetState(timeout=3))), interfaces=interfaces)
                if 'org.a11y.atspi.Component' in interfaces:
                    node['screen_extents'] = list(map(int, dbus.Interface(obj, 'org.a11y.atspi.Component').GetExtents(0, timeout=3)))
                if 'org.a11y.atspi.Action' in interfaces:
                    node['actions'] = [list(map(str, x)) for x in dbus.Interface(obj, 'org.a11y.atspi.Action').GetActions(timeout=3)]
                if 'org.a11y.atspi.Text' in interfaces:
                    text_iface = dbus.Interface(obj, 'org.a11y.atspi.Text')
                    count = int(props.Get('org.a11y.atspi.Text', 'CharacterCount', timeout=3))
                    if not 0 <= count <= 4096:
                        raise RuntimeError('Unexpected oversized synthetic text node')
                    node['text'] = str(text_iface.GetText(0, count, timeout=3))
                nodes.append(node)
                kids = accessible.GetChildren(timeout=3)
                if len(queue) + len(kids) + len(seen) > LIMIT:
                    raise RuntimeError('Accessibility node budget exceeded')
                children = [validate_address(a, owner) for a in kids]
                node['children'] = [a[1] for a in children]
                queue.extend((a, address[1]) for a in children)
            if int(broker.GetConnectionUnixProcessID(owner, timeout=3)) != pid:
                raise RuntimeError('Accessibility ownership changed')
            return save_tree(nodes, root, owner, pid, output, expected_text)
        except (RuntimeError, dbus.DBusException) as error:
            last_error = str(error)
            time.sleep(0.1)
    raise RuntimeError('AT-SPI discovery unproven: ' + str(last_error))


def enable_accessibility():
    import dbus
    session = dbus.SessionBus()
    props = dbus.Interface(session.get_object('org.a11y.Bus', '/org/a11y/bus'), 'org.freedesktop.DBus.Properties')
    props.Set('org.a11y.Status', 'ScreenReaderEnabled', dbus.Boolean(True), timeout=3)
    if not bool(props.Get('org.a11y.Status', 'ScreenReaderEnabled', timeout=3)):
        raise RuntimeError('Private accessibility activation failed')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--discover-pid', type=int)
    parser.add_argument('--enable', action='store_true')
    args = parser.parse_args()
    if args.enable:
        enable_accessibility(); return
    if args.discover_pid:
        discovery(args.discover_pid, args.output); return
    spec = importlib.util.spec_from_file_location('native', Path(__file__).with_name('native-desktop-smoke.py'))
    native = importlib.util.module_from_spec(spec); spec.loader.exec_module(native)
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    binary = args.binary.resolve(strict=True)
    smoke = native.Smoke(binary, 'x11', out)
    smoke.env = child_environment(smoke.env)
    result = dict(binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), stage='AT-SPI discovery only')
    daemon = None
    try:
        log = open(out/'private-dbus.log', 'w'); smoke.logs.append(log)
        daemon = subprocess.Popen(['dbus-daemon', '--session', '--nofork', '--print-address=1'],
                                  env=smoke.env, stdout=log, stderr=log, start_new_session=True)
        smoke.processes.append(daemon)
        def bus_ready():
            if daemon.poll() is not None: raise RuntimeError('Private D-Bus exited')
            lines = (out/'private-dbus.log').read_text().splitlines()
            return lines[0] if lines and lines[0].startswith('unix:') else None
        smoke.env['DBUS_SESSION_BUS_ADDRESS'] = native.wait_for(bus_ready, 'private D-Bus')
        smoke.start_desktop()
        smoke.run(['/usr/bin/python3', str(Path(__file__).resolve()), '--enable', '--output', str(out/'enable.json')])
        smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "block"\ninitial_width = 1200\ninitial_height = 800\nrestore_session = false\n')
        smoke.app = smoke.spawn([str(binary)], 'ember')
        window = native.wait_for(smoke.app_window, 'owned X11 application')
        time.sleep(2)
        identity = smoke.command('E03_DISCOVERY_READY')
        smoke.key('g', ('ctrl', 'shift'))
        time.sleep(0.5)
        smoke.text('E03DISCOVERY')
        smoke.key('a', ('ctrl',))
        smoke.own_clipboard('E03DISCOVERYDECOY', 'query-readback')
        smoke.key('c', ('ctrl',))
        native.wait_for(lambda: smoke.run(['xclip','-selection','clipboard','-o'], capture_output=True).stdout == b'E03DISCOVERY', 'exact GUI query copy')
        smoke.key('Right')
        smoke.screenshot('picker-query')
        smoke.run(['/usr/bin/python3', str(Path(__file__).resolve()), '--discover-pid', str(smoke.app.pid), '--output', str(out/'owned-tree.json')])
        tree = json.loads((out/'owned-tree.json').read_text())
        result.update(status='pass', shell_identity=identity, app_pid=smoke.app.pid, x11_window=window, nodes=len(tree['nodes']))
    except Exception as error:
        result.update(status='fail', error=str(error))
        raise
    finally:
        try:
            close_resources(daemon, smoke)
        except Exception as error:
            result.update(status='fail', cleanup_error=str(error)); raise
        finally: (out/'result.json').write_text(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
