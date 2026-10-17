#!/usr/bin/env python3
"""Hosted public AT-SPI control proof only; not E03 search acceptance."""
import argparse
import hashlib
import importlib.util
import json
import subprocess
import time
from pathlib import Path


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


probe = load('probe', 'native-block-search-atspi-probe.py')
COMPONENT = 'org.a11y.atspi.Component'
ACTION = 'org.a11y.atspi.Action'
TEXT = 'org.a11y.atspi.Text'
FOCUSED = 12
PRESSED = 20


def state(node, bit):
    words = node['states']
    if len(words) != 2 or any(type(x) is not int or not 0 <= x < 2**32 for x in words):
        raise RuntimeError('Malformed AT-SPI state words')
    return bool(words[bit // 32] & (1 << (bit % 32)))


def inside(inner, outer):
    if len(inner) != 4 or len(outer) != 4:
        return False
    x, y, w, h = inner; ox, oy, ow, oh = outer
    return w > 0 and h > 0 and ow > 0 and oh > 0 and ox <= x and oy <= y and x+w <= ox+ow and y+h <= oy+oh


def controls(tree, expected):
    probe.validate_tree(tree['nodes'], tree['root'], tree['owner'], tree['pid'], expected)
    nodes = {n['path']: n for n in tree['nodes']}
    pickers = [n for n in nodes.values() if n['name'] == 'Block Search' and n['role'] == 23]
    if len(pickers) != 1:
        raise RuntimeError('Missing or ambiguous named Block Search frame')
    picker = pickers[0]
    window = nodes.get(picker['parent'])
    if window is None or window['role'] != 23 or window['parent'] != tree['root']:
        raise RuntimeError('Picker does not belong to the owned top-level frame')
    if not inside(picker.get('screen_extents', []), window.get('screen_extents', [])):
        raise RuntimeError('Picker outside owned window')
    editor = nodes[tree['editor_path']]
    found = {'editor': editor}
    for key in ('All', 'Cmd', 'Out'):
        choices = [n for n in nodes.values() if n['name'] == 'Search scope: '+key and n['role'] == 62]
        if len(choices) != 1:
            raise RuntimeError('Missing or ambiguous scope toggle')
        found[key] = choices[0]
    for node in found.values():
        if node['parent'] != picker['path'] or COMPONENT not in node['interfaces']:
            raise RuntimeError('Control outside picker or lacks component')
        if not state(node, 25) or not state(node, 30) or not inside(node.get('screen_extents', []), picker['screen_extents']):
            raise RuntimeError('Control not visibly bounded by picker')
    return found


def control_identity(tree, found):
    return (tree['pid'], tree['owner'], tree['root'],
            found['editor']['parent'],
            tuple((key, found[key]['path']) for key in ('editor', 'All', 'Cmd', 'Out')))


def click_index(node):
    if ACTION not in node['interfaces']:
        raise RuntimeError('Control has no public Action interface')
    matches = [i for i, action in enumerate(node.get('actions', [])) if action and action[0] == 'click']
    if len(matches) != 1:
        raise RuntimeError('Missing or ambiguous advertised click action')
    return matches[0]


def selected(found, target):
    return all(state(found[key], PRESSED) == (key == target) for key in ('All', 'Cmd', 'Out'))


def focus_is(found, target):
    return all(state(node, FOCUSED) == (key == target) for key, node in found.items())


def mutation(tree, expected, target, operation):
    # Every address comes from a freshly traversed owned tree, never CLI paths.
    import dbus
    found = controls(tree, expected)
    node = found[target]
    session = dbus.SessionBus()
    proxy = dbus.Interface(session.get_object('org.a11y.Bus', '/org/a11y/bus'), 'org.a11y.Bus')
    bus = dbus.bus.BusConnection(str(proxy.GetAddress(timeout=3)))
    broker = dbus.Interface(bus.get_object('org.freedesktop.DBus', '/org/freedesktop/DBus'), 'org.freedesktop.DBus')
    def owned():
        if int(broker.GetConnectionUnixProcessID(tree['owner'], timeout=3)) != tree['pid']:
            raise RuntimeError('Owner changed before/after public action')
    owned()
    obj = bus.get_object(tree['owner'], node['path'])
    if operation == 'click':
        index = click_index(node)
        interface = dbus.Interface(obj, ACTION)
        actions = [list(map(str, a)) for a in interface.GetActions(timeout=3)]
        if actions != node['actions']:
            raise RuntimeError('Actions changed since fresh traversal')
        ok = bool(interface.DoAction(index, timeout=3))
    elif operation == 'focus':
        index = None
        if not state(node, 11):
            raise RuntimeError('Target is not focusable')
        ok = bool(dbus.Interface(obj, COMPONENT).GrabFocus(timeout=3))
    else:
        raise RuntimeError('Unsupported public operation')
    owned()
    if not ok:
        raise RuntimeError('Public operation returned false')
    return dict(target=target, path=node['path'], operation=operation, action_index=index, returned=ok)


class Proof:
    def __init__(self, smoke, native, out):
        self.smoke, self.native, self.out = smoke, native, out
        self.query = 'E03DISCOVERY'
        self.trace = []
        self.serial = 0
        self.owner = None
        self.window = smoke.app_window()
        self.idle = None
        self.deadline = time.monotonic() + 90

    def guard(self):
        if time.monotonic() >= self.deadline:
            raise RuntimeError('Interaction proof deadline exhausted')
        if self.idle is not None:
            if (self.idle/'result.json').exists() or (self.idle/'live.bin').read_bytes() != b'':
                raise RuntimeError('Idle collector ended or received unexpected PTY bytes')

    def key(self, key, modifiers=()):
        self.guard()
        self.smoke.key(key, modifiers)

    def text(self, text):
        self.guard()
        self.smoke.text(text)

    def read(self, label):
        self.guard()
        if time.monotonic() >= self.deadline:
            raise RuntimeError('Interaction proof deadline exhausted')
        if self.smoke.app_window() != self.window:
            raise RuntimeError('Owned X11 window changed during proof')
        self.serial += 1
        tree = probe.discovery(self.smoke.app.pid, self.out/f'{self.serial:02d}-{label}.json', self.query)
        if self.owner is None:
            self.owner = tree['owner']
        if tree['owner'] != self.owner:
            raise RuntimeError('Application bus owner changed during proof')
        return tree, controls(tree, self.query)

    def act(self, target, operation, precondition, predicate, label):
        tree, before = self.read(label+'-before')
        if not precondition(before):
            raise RuntimeError('Fresh opposing precondition unproven: '+label)
        identity = control_identity(tree, before)
        self.guard()
        record = dict(label=label, time=time.monotonic(), **mutation(tree, self.query, target, operation))
        self.trace.append(record)
        (self.out/'operations.json').write_text(json.dumps(self.trace, indent=2))
        deadline = min(self.deadline, time.monotonic()+8)
        while time.monotonic() < deadline:
            after_tree, found = self.read(label+'-after')
            if control_identity(after_tree, found) != identity:
                raise RuntimeError('Control identity changed during public operation: '+label)
            if predicate(found):
                record['response_verified'] = True
                (self.out/'operations.json').write_text(json.dumps(self.trace, indent=2))
                self.smoke.screenshot(label)
                return
            time.sleep(0.1)
        raise RuntimeError('Public operation response unproven: '+label)

    def exact_copy(self, expected, label):
        self.key('a', ('ctrl',))
        self.smoke.own_clipboard('DECOY'+label, label)
        self.key('c', ('ctrl',))
        self.native.wait_for(lambda: self.smoke.read_clipboard() == expected.encode(), 'exact query clipboard '+label)
        self.key('Right')
        self.query = expected
        _, found = self.read(label)
        if not focus_is(found, 'editor'):
            raise RuntimeError('Query copy did not preserve editor focus')

    def exercise(self, identity):
        smoke = self.smoke
        # Prove this same foreground collector receives physical input first.
        positive = smoke.raw_start('atspi-positive')
        smoke.text('PTYCONTROL')
        self.native.wait_for(lambda: (positive/'live.bin').read_bytes() == b'PTYCONTROL', 'PTY positive control')
        smoke.raw_finish(positive, identity, b'PTYCONTROL')
        idle = smoke.raw_start('atspi-idle'); self.idle = idle
        self.key('g', ('ctrl', 'shift'))
        time.sleep(0.5)
        self.text(self.query)
        self.exact_copy(self.query, 'initial-query')
        _, found = self.read('baseline')
        if not selected(found, 'All'):
            raise RuntimeError('Initial scope is not uniquely All')
        self.act('Cmd', 'click', lambda f: selected(f, 'All'),
                 lambda f: selected(f, 'Cmd'), 'scope-cmd')
        self.act('Out', 'click', lambda f: selected(f, 'Cmd'),
                 lambda f: selected(f, 'Out'), 'scope-out')
        self.act('Out', 'focus', lambda f: focus_is(f, 'editor') and selected(f, 'Out'),
                 lambda f: focus_is(f, 'Out') and selected(f, 'Out'), 'focus-away')
        self.act('editor', 'focus', lambda f: focus_is(f, 'Out') and selected(f, 'Out'),
                 lambda f: focus_is(f, 'editor') and selected(f, 'Out'), 'focus-editor')
        self.key('a', ('ctrl',)); self.text('E03PUBLICQUERY')
        self.exact_copy('E03PUBLICQUERY', 'changed-query')
        if (idle/'live.bin').read_bytes() != b'':
            raise RuntimeError('Public control/query proof leaked PTY input')
        smoke.screenshot('public-query-proof')
        smoke.raw_finish(idle, identity, b'')
        # No Escape/Return after idle completion: never expose shell to challenge keys.
        return dict(public_actions='Cmd and Out click responses verified',
                    public_focus='scope then editor focus transitions verified',
                    query='E03PUBLICQUERY', pty_bytes=0, owner=self.owner,
                    search_acceptance='not exercised', activated_service_cleanup='unverified')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    native = load('native', 'native-desktop-smoke.py')
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    binary = args.binary.resolve(strict=True)
    smoke = native.Smoke(binary, 'x11', out)
    smoke.env = probe.child_environment(smoke.env)
    smoke.env['EMBER_SMOKE_CAPTURE'] = str(Path(__file__).with_name('native-block-search-pty-capture.py').resolve())
    result = dict(stage='Public AT-SPI control proof only', binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest())
    daemon = None
    try:
        log = open(out/'private-dbus.log', 'w'); smoke.logs.append(log)
        daemon = subprocess.Popen(['dbus-daemon', '--session', '--nofork', '--print-address=1'],
                                  env=smoke.env, stdout=log, stderr=log, start_new_session=True)
        smoke.processes.append(daemon)
        def ready():
            if daemon.poll() is not None: raise RuntimeError('Private D-Bus exited')
            lines = (out/'private-dbus.log').read_text().splitlines()
            return lines[0] if lines and lines[0].startswith('unix:') else None
        smoke.env['DBUS_SESSION_BUS_ADDRESS'] = native.wait_for(ready, 'private D-Bus')
        smoke.start_desktop()
        # Protocol calls run in one isolated subprocess environment, not host env.
        smoke.run(['/usr/bin/python3', str(Path(probe.__file__).resolve()), '--enable', '--output', str(out/'enable.json')])
        smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "block"\ninitial_width = 1200\ninitial_height = 800\nrestore_session = false\n')
        smoke.app = smoke.spawn([str(binary)], 'ember')
        window = native.wait_for(smoke.app_window, 'owned X11 application')
        time.sleep(2); identity = smoke.command('E03_PUBLIC_READY')
        # This process must use only the freshly generated private environment
        # before importing dbus. No default host session bus is consulted.
        import os
        os.environ.clear(); os.environ.update(smoke.env)
        proof = Proof(smoke, native, out)
        result.update(proof.exercise(identity), status='pass', app_pid=smoke.app.pid, x11_window=window, shell_identity=identity)
    except Exception as error:
        result.update(status='fail', error=str(error)); raise
    finally:
        try:
            probe.close_resources(daemon, smoke)
        except Exception as error:
            result.update(status='fail', cleanup_error=str(error)); raise
        finally:
            (out/'result.json').write_text(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
