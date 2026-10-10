#!/usr/bin/env python3
"""Bounded monitor CLI regressions; no Ember build or real sleep required."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile


monitor = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent / "monitor.sh"

with tempfile.TemporaryDirectory(prefix="ember-monitor-test-") as directory:
    root = Path(directory)
    tools = root / "tools"
    tools.mkdir()
    calls = root / "sleep-calls"
    sleep = tools / "sleep"
    sleep.write_text('#!/bin/sh\nprintf "%s\\n" "$1" >> "$SLEEP_CALLS"\nexit 1\n')
    sleep.chmod(0o755)
    env = dict(os.environ, PATH=f"{tools}:/usr/bin:/bin", SLEEP_CALLS=str(calls))

    def run(interval, log=None, target=None):
        args = ["/bin/bash", str(monitor), str(target or os.getpid()), interval]
        if log is not None:
            args.append(str(log))
        return subprocess.run(args, env=env, capture_output=True, text=True, timeout=2)

    log = root / "history.log"
    for interval in ["bad", "0", "0.000", "-1", ".", "1e3", "2s", " 1"]:
        log.write_text("keep existing log\n")
        result = run(interval, log)
        assert result.returncode != 0, interval
        assert log.read_text() == "keep existing log\n", interval
        assert not calls.exists(), interval

    for interval in ["1", ".5", "1.", "00.10"]:
        calls.unlink(missing_ok=True)
        result = run(interval)
        assert result.returncode != 0, interval
        assert calls.read_text().splitlines() == [interval], interval
        assert "监控等待失败" in result.stderr, result.stderr

    calls.unlink()
    result = run("1", root)
    assert result.returncode != 0
    assert "无法写入日志" in result.stderr
    assert not calls.exists()

    pgrep = tools / "pgrep"
    pgrep.write_text('#!/bin/sh\n[ "$1" = -f ] && [ "$2" = -- ] && [ "$3" = -probe ] || exit 1\nprintf "%s\\n" "$MONITOR_TEST_PID"\n')
    pgrep.chmod(0o755)
    env["MONITOR_TEST_PID"] = str(os.getpid())
    result = run("1", target="-probe")
    assert "监控等待失败" in result.stderr, result.stderr

print("monitor interval, log-error and option-boundary contracts: ok")
