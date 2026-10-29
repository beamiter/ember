"""Identity evidence must never silently attribute substituted or unstable files."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location(
    'environment', Path(__file__).with_name('native-render-environment.py'))
environment = importlib.util.module_from_spec(spec)
spec.loader.exec_module(environment)


class IdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.font = self.root / 'font.ttf'
        self.font.write_bytes(b'fixture-font-data')

    def test_exact_family_and_file_hash(self):
        result = environment.font_identity(
            'DejaVu Sans Mono',
            lambda _: f'DejaVu Sans Mono\tBook\t{self.font}\n', (self.root,))
        self.assertEqual(result['current_file']['sha256'],
                         hashlib.sha256(self.font.read_bytes()).hexdigest())

    def test_substitution_is_not_attributed_to_requested_family(self):
        with mock.patch.object(environment, 'file_identity') as identify:
            with self.assertRaisesRegex(ValueError, 'substituted'):
                environment.font_identity(
                    'Missing Mono', lambda _: f'DejaVu Sans\tBook\t{self.font}\n')
            identify.assert_not_called()

    def test_multiple_records_rejected(self):
        with self.assertRaisesRegex(ValueError, 'one complete'):
            environment.font_identity('Mono', lambda _: 'Mono\tBook\ta\nMono\tBook\tb\n')

    def test_symlink_outside_registered_root_rejected(self):
        allowed = self.root / 'allowed'
        allowed.mkdir()
        link = allowed / 'font.ttf'
        link.symlink_to(self.font)
        with self.assertRaisesRegex(ValueError, 'outside'):
            environment.file_identity(link, (allowed,))

    def test_changed_file_is_not_a_stable_identity(self):
        digest = hashlib.sha256()
        font = self.font

        class ChangedDuringHash:
            def update(self, data):
                digest.update(data)
                replacement = font.with_suffix('.changed')
                replacement.write_bytes(b'changed-file-data')
                replacement.replace(font)

            def hexdigest(self):
                return digest.hexdigest()

        with mock.patch.object(environment.hashlib, 'sha256', return_value=ChangedDuringHash()):
            with self.assertRaisesRegex(ValueError, 'changed while'):
                environment.file_identity(self.font, (self.root,))

    def test_model_identity_and_missing_model_rejected(self):
        model = self.root / 'eng.traineddata'
        model.write_bytes(b'fixture-model')
        listing = f'List of available languages in "{self.root}/" (1):\neng\n'
        result = environment.english_model_identity(lambda _: listing, (self.root,))
        self.assertEqual(result['sha256'], hashlib.sha256(model.read_bytes()).hexdigest())
        with self.assertRaisesRegex(ValueError, 'advertise'):
            environment.english_model_identity(lambda _: listing.replace('\neng\n', '\nfra\n'))

    def test_unavailable_remains_unavailable(self):
        def unavailable():
            raise ValueError('unsupported identity')
        self.assertEqual(environment.observe(unavailable),
                         {'status': 'unavailable', 'error': 'unsupported identity'})

    def test_empty_version_is_unavailable_not_an_unhandled_index_error(self):
        result = environment.observe(lambda: environment.tesseract_version(lambda _: ''))
        self.assertEqual(result['status'], 'unavailable')
        self.assertIn('empty', result['error'])

    def test_symlink_loop_is_unavailable(self):
        loop = self.root / 'loop'
        loop.symlink_to(loop)
        result = environment.observe(lambda: environment.file_identity(loop, (self.root,)))
        self.assertEqual(result['status'], 'unavailable')


if __name__ == '__main__':
    unittest.main()
