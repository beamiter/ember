#!/usr/bin/env python3
"""Exercise the embedded host in actual frontend binaries, using isolated state.

Usage: python3 scripts/verify_workspace_host.py target/debug/ember ../frost/target/debug/frost
No provider credentials, network calls, or user configuration are used.
"""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time


def main():
    binaries = [Path(arg).resolve(strict=True) for arg in sys.argv[1:]]
    if not binaries:
        raise SystemExit("Pass at least one built frontend executable")
    with tempfile.TemporaryDirectory(prefix="jterm-smoke-") as temporary:
        state = Path(temporary) / "state"
        state.mkdir(mode=0o700)
        endpoint = state / "run" / "host.sock"
        env = dict(os.environ, JTERM_WORKSPACE_STATE_DIR=str(state))
        process = None

        def start(binary):
            nonlocal process
            process = subprocess.Popen([str(binary), "--jterm-session-host"], env=env,
                                       stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE)
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise AssertionError(process.stderr.read().decode())
                if endpoint.exists():
                    try:
                        with socket.socket(socket.AF_UNIX) as probe:
                            probe.connect(str(endpoint))
                        return
                    except (ConnectionRefusedError, FileNotFoundError):
                        pass
                time.sleep(0.03)
            raise AssertionError("embedded host did not become ready")

        def stop():
            nonlocal process
            if process is not None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                process = None

        def request(command, *, error=False):
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(10)
                stream.connect(str(endpoint))
                stream.sendall((json.dumps({"Command": {"version": 1, "command": command}}) + "\n").encode())
                value = json.loads(stream.makefile("rb").readline())
            reply = value.get("Reply", {})
            if error:
                assert "Err" in reply, value
                return reply["Err"]
            assert "Ok" in reply, value
            return reply["Ok"]

        def chat(command):
            return request(command)["Chat"]["chat"]

        try:
            start(binaries[0])
            current = chat({"CreateChat": {"title": "Cross-frontend workspace evidence"}})
            identity = current["row"]["id"]
            snapshot = json.loads(current["snapshot_json"])
            numeric_id = snapshot["active_chat_id"]
            assert numeric_id >= 1 << 48
            snapshot["chats"][0]["draft"] = "first question"
            current = chat({"SaveChat": {"id": identity, "expected_revision": current["revision"],
                                          "snapshot_json": json.dumps(snapshot)}})
            submitted_revision = current["draft_revision"]
            submission = {"BeginSubmission": {"id": identity, "expected_revision": current["revision"],
                                               "draft_revision": submitted_revision, "text": "first question"}}
            receipt = request(submission)["Submission"]["receipt"]
            request(submission, error=True)  # Exact nonce is already owned; duplicate dispatch is rejected.
            current = chat({"SetDraft": {"id": identity, "expected_revision": submitted_revision,
                                          "text": "newer unsent draft"}})
            current = chat({"CompleteSubmission": {"receipt": receipt, "text": "real protocol completion"}})
            snapshot = json.loads(current["snapshot_json"])
            row = snapshot["chats"][0]
            assert row["draft"] == "newer unsent draft"
            assert [turn["text"] for turn in row["turns"]] == ["first question", "real protocol completion"]
            request({"CompleteSubmission": {"receipt": receipt, "text": "late duplicate"}}, error=True)
            request({"SetReadPosition": {"id": identity, "view": "api-transcript-relative", "position": 0.35}})
            request({"Pin": {"id": identity, "pinned": True}})
            current = chat({"LoadChat": {"id": identity}})
            assert current["read_position"]["position"] == 0.35
            assert current["row"]["pinned"]
            print(f"PASS {binaries[0].name}: embedded entrypoint, revisioned draft, single submission owner, newer draft preservation, stale receipt rejection, read position")
            stop()
            for binary in binaries[1:] or binaries[:1]:
                start(binary)
                restored = chat({"LoadChat": {"id": identity}})
                restored_row = json.loads(restored["snapshot_json"])["chats"][0]
                assert restored_row["id"] == numeric_id
                assert restored_row["draft"] == "newer unsent draft"
                assert len(restored_row["turns"]) == 2
                assert restored["read_position"]["position"] == 0.35
                request({"CompleteSubmission": {"receipt": receipt, "text": "duplicate after restart"}}, error=True)
                print(f"PASS {binary.name}: shared history and reading state survive host replacement; retired receipt stays retired")
                stop()
        finally:
            stop()


if __name__ == "__main__":
    main()
