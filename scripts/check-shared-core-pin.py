#!/usr/bin/env python3
"""Check repository release provenance without building or changing dependencies."""

import argparse
import copy
import json
import re
from pathlib import Path
import tomllib
import unittest

REMOTE = "https://github.com/beamiter/jterm_core.git"


def verify(manifest, lock, config):
    core = manifest["dependencies"]["jterm_core"]
    revision = core.get("rev", "")
    if core.get("git") != REMOTE or not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("jterm_core must use the exact published Git revision")
    if "path" in core:
        raise ValueError("release dependency must not use a local path")
    development = manifest.get("dev-dependencies", {}).get("jterm_core", core)
    if any(development.get(key) != core.get(key) for key in ("git", "rev", "path")):
        raise ValueError("development and production core pins differ")
    for document in (manifest, config):
        for replacements in document.get("patch", {}).values():
            for name, dependency in replacements.items():
                if name == "jterm_core" or dependency.get("package") == "jterm_core":
                    raise ValueError("a checked-in patch masks the published core pin")
    packages = [item for item in lock["package"] if item["name"] == "jterm_core"]
    expected = f"git+{REMOTE}?rev={revision}#{revision}"
    if len(packages) != 1 or packages[0].get("source") != expected:
        raise ValueError("Cargo.lock does not identify the same exact Git candidate")
    return {"status": "passed", "revision": revision, "source": expected,
            "scope": "manifest, lockfile and checked-in Cargo configuration; not runtime validation"}


class ProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.revision = "a" * 40
        self.manifest = {"dependencies": {"jterm_core": {"git": REMOTE, "rev": self.revision}}}
        self.lock = {"package": [{"name": "jterm_core", "source":
                                 f"git+{REMOTE}?rev={self.revision}#{self.revision}"}]}

    def test_matching_candidate(self):
        self.assertEqual(verify(self.manifest, self.lock, {})["revision"], self.revision)

    def test_stale_lockfile(self):
        self.lock["package"][0]["source"] = self.lock["package"][0]["source"].replace("a" * 40, "b" * 40)
        with self.assertRaisesRegex(ValueError, "Cargo.lock"):
            verify(self.manifest, self.lock, {})

    def test_hidden_sibling_override(self):
        config = {"patch": {REMOTE: {"jterm_core": {"path": "../jterm_core"}}}}
        with self.assertRaisesRegex(ValueError, "masks"):
            verify(self.manifest, self.lock, config)

    def test_development_pin_mismatch(self):
        development = copy.deepcopy(self.manifest["dependencies"])
        development["jterm_core"]["rev"] = "b" * 40
        self.manifest["dev-dependencies"] = development
        with self.assertRaisesRegex(ValueError, "development"):
            verify(self.manifest, self.lock, {})

    def test_local_lockfile_is_not_a_published_candidate(self):
        self.lock["package"][0].pop("source")
        with self.assertRaisesRegex(ValueError, "Cargo.lock"):
            verify(self.manifest, self.lock, {})


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        unittest.main(argv=[__file__])
    else:
        root = Path(__file__).resolve().parents[1]
        def read(relative):
            path = root / relative
            return tomllib.loads(path.read_text()) if path.exists() else {}
        print(json.dumps(verify(read("Cargo.toml"), read("Cargo.lock"), read(".cargo/config.toml"))))
