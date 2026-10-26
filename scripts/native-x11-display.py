"""Allocate an owned Xvfb display and verify connections before launching clients."""
import json
import os
import select
import subprocess
import time


def start_xvfb(spawn, env, out, geometry, timeout=30):
    deadline = time.monotonic() + timeout
    read_fd, write_fd = os.pipe()
    try:
        try:
            # -displayfd allocates without racing another suite for a fixed :99.
            # Readiness probes close their connections; -noreset prevents the last
            # one from resetting the server while the first real client connects.
            server = spawn(['Xvfb', '-screen', '0', geometry, '-nolisten', 'tcp',
                            '-noreset', '-displayfd', str(write_fd)],
                           'xvfb', pass_fds=(write_fd,))
        finally:
            os.close(write_fd)
        if not select.select([read_fd], [], [], max(0, deadline - time.monotonic()))[0]:
            raise RuntimeError('Xvfb display allocation timed out; see xvfb.log')
        number = os.read(read_fd, 64)
        if not number.endswith(b'\n') or not number[:-1].isdigit() or server.poll() is not None:
            raise RuntimeError('Xvfb exited or returned an invalid display; see xvfb.log')
        env['DISPLAY'] = ':' + number[:-1].decode('ascii')
    finally:
        os.close(read_fd)
    while time.monotonic() < deadline:
        if server.poll() is not None:
            raise RuntimeError('Xvfb exited before connection readiness; see xvfb.log')
        attempt = {'display': env['DISPLAY'], 'monotonic': time.monotonic()}
        try:
            probe = subprocess.run(['xdpyinfo'], env=env, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.PIPE, text=True,
                                   timeout=min(3, max(0.001, deadline - time.monotonic())))
            attempt.update(returncode=probe.returncode, stderr=probe.stderr[:512])
        except subprocess.TimeoutExpired:
            attempt['timeout'] = True
        with (out / 'x11-readiness.jsonl').open('a') as log:
            log.write(json.dumps(attempt) + '\n')
        if attempt.get('returncode') == 0 and server.poll() is None:
            return server
        time.sleep(min(0.08, max(0, deadline - time.monotonic())))
    raise RuntimeError('Xvfb connection readiness timed out; see x11-readiness.jsonl')
