import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest


PATH = Path(__file__).resolve().with_name('release_kvm_deploy.py')
SPEC = importlib.util.spec_from_file_location('release_kvm_deploy', PATH)
DEPLOY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DEPLOY)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


class PinnedInputTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.source = self.root / 'source.dat'
        self.target = self.root / 'target.dat'
        self.source.write_bytes(b'pinned input')

    def tearDown(self):
        self.temporary.cleanup()

    def test_copies_and_validates_a_pinned_input(self):
        DEPLOY.copy_pinned_input(self.source, digest(self.source), self.target)
        self.assertEqual(self.target.read_bytes(), b'pinned input')
        self.assertEqual(self.target.stat().st_mode & 0o777, 0o600)

    def test_rejects_a_changed_source_without_creating_output(self):
        with self.assertRaisesRegex(RuntimeError, 'differs from its SHA-256'):
            DEPLOY.copy_pinned_input(self.source, '0' * 64, self.target)
        self.assertFalse(self.target.exists())

    def test_preserves_a_mismatched_existing_output(self):
        self.target.write_bytes(b'preserve mismatch')
        with self.assertRaisesRegex(RuntimeError, 'differs from its SHA-256'):
            DEPLOY.copy_pinned_input(self.source, digest(self.source), self.target)
        self.assertEqual(self.target.read_bytes(), b'preserve mismatch')


if __name__ == '__main__':
    unittest.main()
