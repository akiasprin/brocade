import importlib.util
from pathlib import Path
import sys
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location('cvm_package', SCRIPTS / 'cvm_package.py')
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


class GuestContractTests(unittest.TestCase):
    def setUp(self):
        self.files = {
            '/etc/passwd': b'root:x:0:0:root:/root:/bin/bash\ncube:x:1000:1000::/home/cube:/bin/bash\n',
            '/etc/ssh/sshd_config': b'Include /etc/ssh/sshd_config.d/*.conf\n',
        }
        self.stats = {
            '/home/cube': ('directory', 0o755, None),
            '/bin/bash': ('regular', 0o755, None),
            **{path: ('regular', 0o755, None) for path in PACKAGE.GUEST_EXECUTABLES},
        }
        self.stats['/bin/sh'] = ('symlink', 0o777, 'bash')

    def validate(self):
        PACKAGE.validate_guest_contract(
            Path('/guest.img'),
            read_file=lambda _image, path: self.files[path],
            stat_file=lambda _image, path: self.stats[path],
        )

    def test_accepts_complete_brocade_guest_contract(self):
        self.validate()

    def test_rejects_guest_without_cube_account(self):
        self.files['/etc/passwd'] = b'root:x:0:0:root:/root:/bin/bash\n'
        with self.assertRaisesRegex(PACKAGE.stack.StackError, 'cube account'):
            self.validate()

    def test_rejects_guest_without_required_executable(self):
        del self.stats['/usr/sbin/sshd']
        with self.assertRaisesRegex(PACKAGE.stack.StackError, '/usr/sbin/sshd'):
            self.validate()

    def test_rejects_non_executable_guest_command(self):
        self.stats['/usr/bin/ssh-keygen'] = ('regular', 0o644, None)
        with self.assertRaisesRegex(PACKAGE.stack.StackError, 'not executable'):
            self.validate()


if __name__ == '__main__':
    unittest.main()
