#!/usr/bin/env python3
"""Real X11 pointer/keyboard Mission lifecycle on an isolated installed Ember.

AT-SPI is a read-only target observer. Every operation uses xdotool input; no
application message, host RPC or database write creates or edits a Mission.
Five disposable local repositories are not the real delivery combination.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


public = load('mission_public', 'native-block-search-atspi-interaction.py')
probe = public.probe
TITLE = 'Missions · delivery evidence'
REPOSITORIES = ('jterm_core', 'anvil', 'forge', 'ember', 'frost')
FREEZE = 'Freeze observed candidates'
INSPECT = 'Inspect repositories (read-only)'


def owned_nodes(tree, dialog=True):
    probe.validate_tree(tree['nodes'], tree['root'], tree['owner'], tree['pid'], None)
    nodes = {node['path']: node for node in tree['nodes']}
    frames = [node for node in nodes.values() if node['role'] == 23 and
              (node['name'] == TITLE if dialog else node['parent'] == tree['root'])]
    if len(frames) != 1:
        raise RuntimeError('Missing or ambiguous owned Mission frame')
    frame = frames[0]
    if dialog:
        parent = nodes.get(frame['parent'])
        if parent is None or parent['role'] != 23 or parent['parent'] != tree['root']:
            raise RuntimeError('Mission frame is not inside the owned application')
        if not public.inside(frame.get('screen_extents', []), parent.get('screen_extents', [])):
            raise RuntimeError('Mission frame escapes its owned application')
    def descendant(node):
        while node['path'] != frame['path']:
            if node['parent'] not in nodes:
                return False
            node = nodes[node['parent']]
        return True
    return frame, [node for node in nodes.values() if descendant(node)]


def visible(node, frame):
    return (public.COMPONENT in node['interfaces'] and
            all(public.state(node, bit) for bit in (25, 30)) and
            public.inside(node.get('screen_extents', []), frame.get('screen_extents', [])))


def content_frame(frame, nodes):
    """Exclude the observed title bar and a conservative bottom border inset."""
    bars = [node for node in nodes if node['role'] == 67 and node['parent'] == frame['path']]
    if len(bars) != 1 or not visible(bars[0], frame):
        raise RuntimeError('Missing or ambiguous owned Mission title bar')
    x, y, width, height = frame['screen_extents']
    top = bars[0]['screen_extents'][1] + bars[0]['screen_extents'][3]
    if not y < top < y + height - 8:
        raise RuntimeError('Invalid Mission content viewport')
    return dict(frame, screen_extents=[x, top, width, y + height - 8 - top])


def button(tree, label, enabled=True, dialog=True):
    frame, nodes = owned_nodes(tree, dialog)
    if dialog:
        frame = content_frame(frame, nodes)
    choices = [node for node in nodes if node['name'] == label and
               node['role'] in (43, 62) and visible(node, frame)]
    if len(choices) != 1:
        raise RuntimeError('Missing or ambiguous visible Mission button: ' + label)
    node = choices[0]
    # AT-SPI ENABLED is 8, SENSITIVE is 24. Both must agree with the required
    # state; disabled negative clicks never masquerade as normal operations.
    if any(public.state(node, bit) != enabled for bit in (8, 24)):
        raise RuntimeError('Mission button enablement mismatch: ' + label)
    if enabled:
        public.click_index(node)
    return node


def editor(tree, label):
    frame, nodes = owned_nodes(tree)
    frame = content_frame(frame, nodes)
    labels = [node for node in nodes if node['name'] == label and node['role'] == 29
              and visible(node, frame)]
    if len(labels) != 1:
        raise RuntimeError('Missing or ambiguous Mission field label: ' + label)
    anchor = labels[0]
    x, y, _, height = anchor['screen_extents']
    choices = [node for node in nodes if node['role'] == 79 and visible(node, frame)
               and public.TEXT in node['interfaces'] and public.state(node, 7)
               and abs(node['screen_extents'][0] - x) <= 8
               and 0 <= node['screen_extents'][1] - y - height <= 24]
    if len(choices) != 1:
        raise RuntimeError('Missing or ambiguous labelled Mission editor: ' + label)
    return choices[0]


def fixture_repositories(parent):
    heads = {}
    for name in REPOSITORIES:
        checkout = parent / name
        checkout.mkdir()
        def git(*args):
            return subprocess.check_output(['git', '-C', str(checkout), *args], text=True).strip()
        git('init', '-b', 'master')
        git('config', 'user.name', 'Native Mission fixture')
        git('config', 'user.email', 'native-fixture@example.invalid')
        manifest = f'[package]\nname="{name}"\nversion="0.0.0"\n'
        lock = 'version = 4\n'
        if name != 'jterm_core':
            core = heads['beamiter/jterm_core']
            manifest += ('[dependencies]\njterm_core = { git="https://github.com/beamiter/jterm_core.git", '
                         f'rev="{core}" }}\n')
            lock += ('[[package]]\nname="jterm_core"\nversion="0.2.0"\n'
                     f'source="git+https://github.com/beamiter/jterm_core.git?rev={core}#{core}"\n')
        (checkout / 'Cargo.toml').write_text(manifest)
        (checkout / 'Cargo.lock').write_text(lock)
        git('add', 'Cargo.toml', 'Cargo.lock')
        git('commit', '-m', 'Disposable native Mission observation')
        heads['beamiter/' + name] = git('rev-parse', 'HEAD')
    return heads


def stored(home):
    records = []
    for db in home.rglob('*.sqlite*'):
        if db.name.endswith(('-wal', '-shm')):
            continue
        try:
            with sqlite3.connect(db.as_uri() + '?mode=ro', uri=True, timeout=.2) as connection:
                rows = connection.execute('SELECT plan_json, report_json FROM workspace_missions').fetchall()
            records.extend((json.loads(plan), json.loads(report) if report else None) for plan, report in rows)
        except sqlite3.OperationalError:
            continue
    if len(records) > 1:
        raise RuntimeError('A visible Create action created duplicate Missions')
    return records[0] if records else None


class Driver:
    def __init__(self, smoke):
        self.smoke = smoke
        self.serial = 0
        self.owner = None
        self.window = smoke.app_window()
        self.actions = []

    def read(self, label):
        if self.smoke.app.poll() is not None or self.smoke.app_window() != self.window:
            raise RuntimeError('Owned application/window changed during Mission proof')
        self.serial += 1
        stem = f'{self.serial:03d}-{label}'
        tree = probe.discovery(self.smoke.app.pid, self.smoke.out / (stem + '.json'), None)
        if self.owner is not None and self.owner != tree['owner']:
            raise RuntimeError('Mission accessibility owner changed')
        self.owner = tree['owner']
        self.smoke.screenshot(stem)
        return tree

    def click(self, node, label):
        x, y, width, height = node['screen_extents']
        # No --sync on an unchanged pointer location: that would wait for an
        # impossible motion event on repeat/disabled-control checks.
        self.smoke.run(['xdotool', 'mousemove', str(x + width // 2), str(y + height // 2), 'click', '1'])
        self.actions.append(dict(label=label, path=node['path'], bounds=node['screen_extents'],
                                 input='X11 pointer button 1', monotonic_ns=time.monotonic_ns()))
        (self.smoke.out / 'pointer-actions.json').write_text(json.dumps(self.actions, indent=2))
        time.sleep(.3)

    def press(self, text, enabled=True, dialog=True):
        before = self.locate(text, (43, 62), dialog=dialog)
        node = button(before, text, enabled, dialog)
        after = self.read('settled-button')
        fresh = button(after, text, enabled, dialog)
        if (node['path'], node['screen_extents']) != (fresh['path'], fresh['screen_extents']):
            raise RuntimeError('Mission control moved before click: ' + text)
        self.click(fresh, text)

    def locate(self, label, roles, dialog=True, below=0):
        # Egui exposes offscreen children in its public tree. Their extents
        # locate a scroll direction, never authorize clicking through clipping.
        for attempt in range(9):
            tree = self.read('locate-control')
            frame, nodes = owned_nodes(tree, dialog)
            area = content_frame(frame, nodes) if dialog else frame
            matches = [node for node in nodes if node['name'] == label and node['role'] in roles]
            if len(matches) != 1:
                raise RuntimeError('Missing or ambiguous exact Mission target: ' + label)
            node = matches[0]
            bounds = node.get('screen_extents', [])
            if len(bounds) != 4 or min(bounds[2:]) <= 0:
                raise RuntimeError('Invalid Mission target geometry: ' + label)
            x, y, width, height = bounds
            ax, ay, aw, ah = area['screen_extents']
            if visible(node, area) and y + height + below <= ay + ah:
                return tree
            if not dialog or x < ax or x + width > ax + aw or attempt == 8:
                raise RuntimeError('Mission target is not safely visible: ' + label)
            direction = '5' if y + height + below > ay + ah else '4'
            # Use the owned dialog's right gutter, away from multiline inputs.
            self.smoke.run(['xdotool', 'mousemove', str(ax + aw - 12), str(ay + ah // 2),
                            'click', '--repeat', '3', '--delay', '60', direction])
            self.actions.append(dict(label=label, input='X11 wheel', direction=direction,
                                     viewport=area['screen_extents'], target_bounds=bounds,
                                     monotonic_ns=time.monotonic_ns()))
            (self.smoke.out / 'pointer-actions.json').write_text(json.dumps(self.actions, indent=2))
            time.sleep(.25)
        raise RuntimeError('Mission target scroll budget exhausted')

    def fill(self, label, value):
        node = editor(self.locate(label, (29,), below=100), label)
        self.click(node, label)
        self.smoke.key('a', ('ctrl',))
        self.smoke.text(value)
        fresh = editor(self.read('editor-readback'), label)
        if fresh['path'] != node['path'] or fresh.get('text') != value or not public.state(fresh, 12):
            raise RuntimeError('Exact focused Mission input readback failed: ' + label)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    binary = args.binary.resolve(strict=True)
    native = load('mission_native', 'native-desktop-smoke.py')
    smoke = native.Smoke(binary, 'x11', out)
    smoke.env = probe.child_environment(smoke.env)
    result = dict(status='running', backend='x11', binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  assertions=[], not_covered=['real five-repository delivery', 'CI evidence ingestion',
                  'Wayland Mission UI', 'physical keyboard or GPU', 'external provider execution'])
    started = time.monotonic()
    daemon = None
    old_bus = os.environ.get('DBUS_SESSION_BUS_ADDRESS')
    temp = tempfile.TemporaryDirectory(prefix='ember-mission-fixture-', dir='/tmp')
    try:
        parent = Path(temp.name)
        heads = fixture_repositories(parent)
        result['fixture_heads'] = heads
        log = (out / 'private-dbus.log').open('w')
        smoke.logs.append(log)
        daemon = subprocess.Popen(['dbus-daemon', '--session', '--nofork', '--print-address=1'],
                                  env=smoke.env, stdout=log, stderr=log, start_new_session=True)
        smoke.processes.append(daemon)
        def bus_ready():
            if daemon.poll() is not None:
                raise RuntimeError('Private D-Bus exited')
            lines = (out / 'private-dbus.log').read_text().splitlines()
            return lines[0] if lines and lines[0].startswith('unix:') else None
        smoke.env['DBUS_SESSION_BUS_ADDRESS'] = native.wait_for(bus_ready, 'private D-Bus')
        # Observer connects only to the freshly created private bus.
        os.environ['DBUS_SESSION_BUS_ADDRESS'] = smoke.env['DBUS_SESSION_BUS_ADDRESS']
        smoke.start_desktop()
        probe.enable_accessibility()
        smoke.config.write_text('shell="/bin/bash"\nterminal_mode="block"\nrestore_session=false\n'
                                'initial_width=1200\ninitial_height=850\nagent_workspace_enabled=true\n')
        smoke.app = smoke.spawn([str(binary)], 'ember-mission')
        native.wait_for(smoke.app_window, 'owned Mission application')
        time.sleep(2)
        driver = Driver(smoke)
        driver.press('Missions · 0 need you', dialog=False)
        driver.press('Create a five-repository Mission')
        driver.fill('Title', 'EmberMissionFixture')
        goal = 'Inspect exact local commits without claiming delivery'
        driver.fill('Goal and acceptance intent', goal)
        driver.fill('Absolute parent directory containing jterm_core, anvil, forge, ember and frost', str(parent))
        driver.press('Create Mission')
        saved, report = native.wait_for(lambda: stored(out / 'home'), 'native Mission creation persisted')
        assert saved['title'] == 'EmberMissionFixture' and saved['goal'] == goal and saved['revision'] == 1
        assert report is None
        assert [repo['identity']['full_name'] for repo in saved['repositories']] == ['beamiter/' + name for name in REPOSITORIES]
        assert [repo['path'] for repo in saved['repositories']] == [str(parent / name) for name in REPOSITORIES]
        result['mission_id'] = saved['id']
        result['assertions'].append('Real native inputs created exactly one five-repository Mission with exact persisted fields')
        driver.press('Create a five-repository Mission')
        driver.press(FREEZE, enabled=False)
        assert stored(out / 'home')[0]['revision'] == 1
        result['assertions'].append('Uninspected Freeze is disabled and a real click cannot change the saved revision')
        driver.press(INSPECT)
        def inspected(revision):
            value = stored(out / 'home')
            return value if value and value[1] and value[1]['revision'] == revision else None
        saved, report = native.wait_for(lambda: inspected(1), 'first native inspection')
        assert len(report['repositories']) == 5
        for repo in report['repositories']:
            assert repo['observed']['head'] == heads[repo['identity']['full_name']]
            assert not repo['observed']['dirty']
            assert not repo['pushed'] and not repo['integrated'] and not repo['validated']
        result['assertions'].append('Inspect observed all five clean exact Git heads without inventing remote or validation success')
        # Host persistence may precede the rendered acknowledgement by one frame.
        def freeze_ready():
            try:
                return button(driver.read('freeze-ready'), FREEZE)
            except RuntimeError as error:
                if str(error) == 'Mission button enablement mismatch: ' + FREEZE:
                    return None
                raise
        native.wait_for(freeze_ready, 'enabled fresh Freeze')
        driver.press(FREEZE)
        def frozen():
            value = stored(out / 'home')
            return value if value and value[0]['revision'] == 2 else None
        saved, _ = native.wait_for(frozen, 'native candidate freeze')
        assert all(repo['candidate'] == heads[repo['identity']['full_name']] for repo in saved['repositories'])
        assert all(repo['required_core_pin'] == heads['beamiter/jterm_core']
                   for repo in saved['repositories'] if repo['identity']['full_name'] != 'beamiter/jterm_core')
        result['assertions'].append('One explicit native Freeze recorded the five observed candidates at revision 2')
        time.sleep(.5)
        driver.press(INSPECT)
        native.wait_for(lambda: inspected(2), 'frozen candidate inspection')
        time.sleep(.5)
        driver.press('Refresh saved Missions')
        time.sleep(1)
        driver.press(FREEZE, enabled=False)
        assert stored(out / 'home')[0]['revision'] == 2
        result['assertions'].append('Refresh revoked fresh observation permission; disabled Freeze click could not create revision 3')
        driver.fill('Title', 'Unsaved Mission title')
        driver.press(INSPECT, enabled=False)
        driver.press(FREEZE, enabled=False)
        driver.press('Refresh saved Missions')
        time.sleep(1)
        assert editor(driver.read('draft-retained'), 'Title')['text'] == 'Unsaved Mission title'
        assert stored(out / 'home')[0]['title'] == 'EmberMissionFixture'
        driver.press('Discard draft changes')
        assert editor(driver.read('draft-discarded'), 'Title')['text'] == 'EmberMissionFixture'
        result['assertions'].append('Unsaved draft disabled Inspect/Freeze, survived Refresh, and required explicit discard')
        result['saved_plan'], result['last_host_report'] = stored(out / 'home')
        # Leave by the native window manager, not SIGTERM success accounting.
        smoke.key('F4', ('alt',))
        native.wait_for(lambda: smoke.app.poll() is not None, 'native normal close')
        assert smoke.app.returncode == 0
        result.update(status='pass', normal_quit=True)
    except BaseException as error:
        result.update(status='fail', error=repr(error))
        try:
            result['failure_saved_state'] = stored(out / 'home')
            smoke.screenshot('failure-before-cleanup')
        except Exception:
            pass
        raise
    finally:
        try:
            probe.close_resources(daemon, smoke)
        except Exception as error:
            result.update(status='fail', cleanup_error=repr(error))
            raise
        finally:
            if old_bus is None:
                os.environ.pop('DBUS_SESSION_BUS_ADDRESS', None)
            else:
                os.environ['DBUS_SESSION_BUS_ADDRESS'] = old_bus
            temp.cleanup()
            result['elapsed_seconds'] = time.monotonic() - started
            (out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    main()
