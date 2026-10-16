#!/usr/bin/env python3
"""Pure fixture tests. These are not real-app native evidence."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


fixture = load('fixture', 'native-block-search-output.py')
driver = load('driver', 'native-block-search.py')


class BoundedSink:
    def __init__(self):
        self.maximum = self.total = 0
        self.digest = hashlib.sha256()

    def write(self, data):
        self.maximum = max(self.maximum, len(data))
        self.total += len(data)
        self.digest.update(data)

    def flush(self):
        pass


def row(text, left=0, line=1, block=1):
    return dict(text=text, left=str(left), top='10', width='10', height='10',
                page_num='1', block_num=str(block), par_num='1', line_num=str(line))


class Tests(unittest.TestCase):
    def test_raw_real_size_and_bounded_emit(self):
        sink = BoundedSink()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'manifest.json'
            result = fixture.emit('raw', 0, sink, path, require_tty=False)
            self.assertEqual(result['bytes'], 9700000)
            self.assertEqual(result['lines'], 100000)
            self.assertEqual(result['sha256'], sink.digest.hexdigest())
            self.assertEqual(json.loads(path.read_text()), result)
            self.assertLessEqual(sink.maximum, 128 * 97)
            with self.assertRaises(FileExistsError):
                fixture.emit('flush', 0, sink, path, require_tty=False)

    def test_snapshots_fit_real_retention_and_exceed_search_window(self):
        size = fixture.SNAPSHOT_LINES * 97
        self.assertLess(size, 256 * 1024)
        self.assertGreater(size * fixture.SNAPSHOTS, 8 * 1024 * 1024)
        self.assertLess(size * fixture.SNAPSHOTS, 16 * 1024 * 1024)
        self.assertLess(fixture.SNAPSHOT_LINES * fixture.SNAPSHOTS + 1000, 160000)

    def test_tokens_are_unique_context_not_command_arguments(self):
        expected = {16: b'E03EARLYHIT', 50000: b'E03MIDDLEHIT', 99950: b'E03LATEHIT'}
        counts = {value: 0 for value in expected.values()}
        for i in range(100000):
            data = fixture.line('raw', i)
            self.assertEqual(len(data), 97)
            self.assertEqual(data.count(b'\n'), 1)
            for token in counts:
                counts[token] += token in data
            if i in expected:
                self.assertTrue(data.startswith(expected[i] + b' '))
        self.assertEqual(list(counts.values()), [1, 1, 1])

    def test_non_tty_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises((RuntimeError, io.UnsupportedOperation)):
                fixture.emit('raw', 0, io.BytesIO(), Path(tmp) / 'x')

    def test_bad_modes_and_record_bounds(self):
        with tempfile.TemporaryDirectory() as tmp:
            for mode, record in [('other', 0), ('raw', -1), ('snapshot', 64)]:
                with self.assertRaises(ValueError):
                    fixture.emit(mode, record, BoundedSink(), Path(tmp) / 'x', False)

    def test_phrase_cannot_cross_lines_or_blocks(self):
        for rows in [[row('Stop'), row('search', 20, line=2)],
                     [row('Stop'), row('search', 20, block=2)]]:
            self.assertFalse(driver.exact_phrase(rows, 'Stop search'))
        self.assertEqual(driver.phrase_center([row('Stop'), row('search', 20)], 'Stop search'), (15, 15))

    def test_missing_duplicate_substring_are_not_success(self):
        for rows in [[], [row('Restart')], [row('Stop'), row('Stop', line=2)]]:
            with self.assertRaises(RuntimeError):
                driver.phrase_center(rows, 'Stop')
        self.assertFalse(driver.exact_phrase([row('Stopped')], 'Stop'))

    def test_active_hit_requires_one_observation_with_both_controls(self):
        hit = [row(word, i * 30, line=2) for i, word in enumerate(
            ['Retained', 'row', '#123', 'literal', 'match:', 'E03EARLYHIT'])]
        stop = [row('Stop'), row('search', 30)]
        claim = driver.active_hit_claim(stop + hit, 'E03EARLYHIT')
        self.assertEqual(claim, dict(token='E03EARLYHIT', retained_row=123))
        for incomplete in [stop, hit, [], stop + [row('E03EARLYHIT', line=3)]]:
            with self.assertRaises(RuntimeError):
                driver.active_hit_claim(incomplete, 'E03EARLYHIT')

    def test_hit_claim_rejects_ambiguous_wrong_or_missing_anchor(self):
        hit = [row(word, i * 30, line=2) for i, word in enumerate(
            ['Retained', 'row', '#123', 'literal', 'match:', 'E03EARLYHIT'])]
        duplicate = [dict(r, line_num='3') for r in hit]
        for invalid in [hit + duplicate, hit[3:]]:
            with self.assertRaises(RuntimeError):
                driver.raw_hit_claim(invalid, 'E03EARLYHIT')
        with self.assertRaises(RuntimeError):
            driver.raw_hit_claim(hit, 'OTHER')
        changed = [dict(r, text='#124') if r['text'] == '#123' else r for r in hit]
        self.assertNotEqual(driver.raw_hit_claim(hit, 'E03EARLYHIT'),
                            driver.raw_hit_claim(changed, 'E03EARLYHIT'))

    def test_manifest_exact_stream_identity_positive_and_negative(self):
        count = 256
        digest = hashlib.sha256()
        for i in range(count):
            digest.update(fixture.line('flush', i))
        valid = dict(mode='flush', record=0, lines=count, bytes=count * 97,
                     sha256=digest.hexdigest(), shell_pid=123, tty='/dev/pts/9', columns=100)
        driver.validate_manifest(valid, 'flush', 0, '123:/dev/pts/9')
        for key, wrong in [('sha256', '0' * 64), ('shell_pid', 124),
                           ('tty', '/dev/pts/10'), ('columns', 99),
                           ('mode', 'snapshot'), ('record', 1),
                           ('bytes', count * 97 - 1), ('lines', count - 1)]:
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                driver.validate_manifest(dict(valid, **{key: wrong}), 'flush', 0, '123:/dev/pts/9')

    def test_source_uses_real_binary_no_geometry_harness_or_controller(self):
        source = Path(__file__).with_name('native-block-search.py').read_text()
        self.assertIn('self.spawn([str(self.binary)]', source)
        self.assertNotIn('native-wayland-geometry', source)
        self.assertNotIn('refresh_block_search_hits', source)
        self.assertIn("choices=['x11']", source)
        self.assertIn("'input p95 and presentation latency'", source)


if __name__ == '__main__':
    unittest.main()
