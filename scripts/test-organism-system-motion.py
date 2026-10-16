#!/usr/bin/env python3
"""Hosted-only motion preference integration on a disposable owned session bus."""
import argparse
import os
from pathlib import Path
import secrets
import signal
import subprocess
import sys
import tempfile
import time


def cleanup_process_group(child, grace=2.0):
    """Terminate our entire group even when its leader has already exited."""
    def send(sig):
        try:
            os.killpg(child.pid, sig)
            return True
        except ProcessLookupError:
            return False

    if send(signal.SIGTERM):
        deadline = time.monotonic() + grace
        while time.monotonic() < deadline:
            child.poll()  # Reap the leader independently of descendants.
            if not send(0):
                break
            time.sleep(0.05)
        else:
            send(signal.SIGKILL)
    try:
        child.wait(timeout=5)
    except subprocess.TimeoutExpired:
        send(signal.SIGKILL)
        child.wait(timeout=5)


def verify_completion(marker, token):
    if not marker.is_file() or marker.read_text() != token:
        raise RuntimeError('Selected native test did not complete this run')


def run_session(scenario, token):
    # Imports deliberately require the distro Python (python3-dbus/python3-gi).
    import dbus
    import dbus.service
    from dbus.mainloop.glib import DBusGMainLoop
    from gi.repository import GLib

    address = os.environ.get('DBUS_SESSION_BUS_ADDRESS', '')
    if not address or address == os.environ.get('EMBER_MOTION_PARENT_BUS_ADDRESS'):
        raise RuntimeError('Private session bus address was not established')
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    name = dbus.service.BusName('org.freedesktop.portal.Desktop', bus,
                                do_not_queue=True)
    settings = 'org.freedesktop.portal.Settings'
    control = 'io.github.beamiter.Ember.MotionFixture'
    appearance = 'org.freedesktop.appearance'
    gnome = 'org.gnome.desktop.interface'

    class Portal(dbus.service.Object):
        def __init__(self):
            super().__init__(name, '/org/freedesktop/portal/desktop')
            self.reads = 0
            self.reduced = True

        @dbus.service.method(settings, in_signature='ss', out_signature='v')
        def Read(self, namespace, key):
            self.reads += 1
            if scenario == 'appearance' and (namespace, key) == (appearance, 'reduced-motion'):
                return dbus.UInt32(1 if self.reduced else 0, variant_level=1)
            if scenario == 'gnome' and (namespace, key) == (gnome, 'enable-animations'):
                return dbus.Boolean(not self.reduced, variant_level=1)
            raise dbus.exceptions.DBusException('Preference unavailable',
                                               name='org.freedesktop.portal.Error.NotFound')

        @dbus.service.signal(settings, signature='ssv')
        def SettingChanged(self, namespace, key, value):
            pass

        @dbus.service.method(control, in_signature='s', out_signature='u')
        def Reads(self, supplied):
            assert supplied == token
            return dbus.UInt32(self.reads)

        @dbus.service.method(control, in_signature='sb', out_signature='s')
        def Emit(self, supplied, reduced):
            assert supplied == token
            self.reduced = bool(reduced)
            if scenario == 'gnome':
                self.SettingChanged(gnome, 'enable-animations', dbus.Boolean(not reduced))
            else:
                self.SettingChanged(appearance, 'reduced-motion', dbus.UInt32(1 if reduced else 0))
            return token

    portal = Portal()
    env = os.environ.copy()
    env['EMBER_MOTION_PRIVATE_BUS_ADDRESS'] = address
    env['EMBER_MOTION_FIXTURE_TOKEN'] = token
    env['EMBER_MOTION_FIXTURE_SCENARIO'] = scenario
    command = ['cargo', 'test', '--locked', '--all-features', '--bin', 'ember',
               'organism_ui::system_motion_native_tests::owned_portal_events_preserve_host_policy',
               '--', '--ignored', '--exact', '--nocapture', '--test-threads=1']
    child = subprocess.Popen(command, env=env)
    loop = GLib.MainLoop()

    def finished():
        if child.poll() is None:
            return True
        loop.quit()
        return False

    GLib.timeout_add(100, finished)
    try:
        loop.run()
    finally:
        if child.poll() is None:
            child.terminate()
            child.wait(timeout=10)
        portal.remove_from_connection()
        bus.close()
    if child.returncode == 0:
        verify_completion(Path(env['EMBER_MOTION_COMPLETION_MARKER']), token)
    return child.returncode


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--session', choices=['appearance', 'gnome', 'missing'])
    parser.add_argument('--token')
    args = parser.parse_args()
    if args.session:
        if not args.token or os.environ.get('EMBER_MOTION_OWNED_BUS') != args.token:
            raise RuntimeError('Refusing to use an unowned session bus')
        return run_session(args.session, args.token)
    for scenario in ['appearance', 'gnome', 'missing']:
        token = secrets.token_hex(24)
        env = os.environ.copy()
        env['EMBER_MOTION_PARENT_BUS_ADDRESS'] = env.pop('DBUS_SESSION_BUS_ADDRESS', '')
        env.pop('DBUS_SESSION_BUS_PID', None)
        env['EMBER_MOTION_OWNED_BUS'] = token
        with tempfile.TemporaryDirectory(prefix='ember-motion-') as directory:
            env['EMBER_MOTION_COMPLETION_MARKER'] = str(Path(directory) / 'completed')
            child = subprocess.Popen(
                ['dbus-run-session', '--', '/usr/bin/python3',
                 str(Path(__file__).resolve()), '--session', scenario, '--token', token],
                env=env, start_new_session=True)
            try:
                status = child.wait(timeout=180)
                if status != 0:
                    raise RuntimeError(f'{scenario} fixture failed: {status}')
                verify_completion(Path(env['EMBER_MOTION_COMPLETION_MARKER']), token)
            finally:
                # Includes nonzero/early leader exits with surviving descendants.
                cleanup_process_group(child)

    return 0


if __name__ == '__main__':
    sys.exit(main())
