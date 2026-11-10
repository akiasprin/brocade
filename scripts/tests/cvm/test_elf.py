"""Exercise the libc floor check against real versioned ELF dependencies."""
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from cvm_elf import validate


class ElfCompatibilityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.architecture = platform.machine()
        (self.root / 'provider.c').write_text('int compatibility_probe(void) { return 0; }\n')
        (self.root / 'consumer.c').write_text(
            'extern int compatibility_probe(void);\n'
            'int call_probe(void) { return compatibility_probe(); }\n')

    def dependency(self, version):
        (self.root / 'versions.map').write_text(f'GLIBC_{version} {{ global: compatibility_probe; }};\n')
        for command in [
            ['cc', '-shared', '-fPIC', '-nostdlib', 'provider.c', '-Wl,--version-script=versions.map',
             '-Wl,-soname,libprovider.so', '-o', 'libprovider.so'],
            ['cc', '-shared', '-fPIC', '-nostdlib', 'consumer.c', '-L.', '-lprovider', '-o', 'libconsumer.so'],
        ]:
            subprocess.run(command, cwd=self.root, check=True, capture_output=True, timeout=30)
        return self.root / 'libconsumer.so'

    def test_newer_imported_glibc_is_rejected(self):
        with self.assertRaisesRegex(ValueError, r'requires GLIBC_2\.38.*declared glibc 2\.35'):
            validate(self.dependency('2.38'), self.architecture, '2.35')

    def test_compatible_imported_glibc_is_recorded(self):
        self.assertEqual(validate(self.dependency('2.34'), self.architecture, '2.35'), '2.34')

    def test_exported_versions_do_not_impose_host_requirements(self):
        self.dependency('2.38')
        self.assertIsNone(validate(self.root / 'libprovider.so', self.architecture, '2.35'))

    def test_wrong_architecture_is_rejected(self):
        other = 'aarch64' if self.architecture == 'x86_64' else 'x86_64'
        with self.assertRaisesRegex(ValueError, f'architecture does not match {other}'):
            validate(self.dependency('2.34'), other, '2.35')

    def test_invalid_baseline_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'invalid glibc baseline'):
            validate(self.root / 'unused', 'x86_64', '2..35')


if __name__ == '__main__':
    unittest.main()
