#!/usr/bin/env python3
"""Keep a read-only RustSec receipt available for every candidate commit."""
from pathlib import Path
import re
import unittest


def block(source, key):
    match = re.search(r'^' + re.escape(key) + r':\n(.*?)(?=^\S|\Z)',
                      source, re.MULTILINE | re.DOTALL)
    if match is None:
        raise ValueError(f'Missing top-level {key} block')
    return match.group(1)


def validate(source):
    events = block(source, 'on')
    if re.search(r'^\s+(?:paths|paths-ignore):', events, re.MULTILINE):
        raise ValueError('Path filters leave commits without exact-head audit receipts')
    for trigger in ('  pull_request:\n    branches: [master]\n',
                    '  push:\n    branches: [master, candidate/zbus6]\n'):
        if trigger not in events:
            raise ValueError('The master push and pull-request triggers must remain present')
    if block(source, 'permissions').strip() != 'contents: read':
        raise ValueError('The security workflow must keep read-only repository permissions')
    if '        run: scripts/security-check.sh --audit\n' not in source:
        raise ValueError('The locked RustSec audit must remain present')


class Coverage(unittest.TestCase):
    def setUp(self):
        self.source = (Path(__file__).resolve().parents[1]
                       / '.github/workflows/security.yml').read_text()

    def test_repository_workflow(self):
        validate(self.source)

    def test_changed_path_filters_are_rejected(self):
        for event in ('push', 'pull_request'):
            for key in ('paths', 'paths-ignore'):
                with self.subTest(event=event, key=key), self.assertRaises(ValueError):
                    validate(self.source.replace(f'  {event}:\n',
                             f'  {event}:\n    {key}: [Cargo.lock]\n'))

    def test_master_branch_removal_is_rejected(self):
        with self.assertRaises(ValueError):
            validate(self.source.replace('branches: [master]', 'branches: [other]'))

    def test_write_permission_is_rejected(self):
        with self.assertRaises(ValueError):
            validate(self.source.replace('contents: read', 'contents: write'))

    def test_audit_removal_is_rejected(self):
        with self.assertRaises(ValueError):
            validate(self.source.replace('scripts/security-check.sh --audit', 'true'))


if __name__ == '__main__':
    unittest.main()
