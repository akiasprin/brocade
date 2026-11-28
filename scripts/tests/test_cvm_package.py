import hashlib
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location('cvm_package', SCRIPTS / 'cvm_package.py')
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


class SandboxLoginContractTests(unittest.TestCase):
    def setUp(self):
        self.files = {
            '/etc/passwd': b'root:x:0:0:root:/root:/bin/bash\ncube:x:1000:1000::/home/cube:/bin/bash\n',
            '/etc/ssh/sshd_config': b'Include /etc/ssh/sshd_config.d/*.conf\n',
        }
        self.stats = {
            '/home/cube': ('directory', 0o755, None),
            '/bin/bash': ('regular', 0o755, None),
            **{path: ('regular', 0o755, None) for path in PACKAGE.SANDBOX_EXECUTABLES},
        }
        self.stats['/bin/sh'] = ('symlink', 0o777, 'bash')

    def validate(self):
        PACKAGE.validate_sandbox_login_contract(
            Path('/root.ext4'),
            read_file=lambda _image, path: self.files[path],
            stat_file=lambda _image, path: self.stats[path],
        )

    def test_accepts_complete_brocade_sandbox_contract(self):
        self.validate()

    def test_rejects_sandbox_without_cube_account(self):
        self.files['/etc/passwd'] = b'root:x:0:0:root:/root:/bin/bash\n'
        with self.assertRaisesRegex(PACKAGE.stack.StackError, 'cube account'):
            self.validate()

    def test_rejects_sandbox_without_required_executable(self):
        del self.stats['/usr/sbin/sshd']
        with self.assertRaisesRegex(PACKAGE.stack.StackError, '/usr/sbin/sshd'):
            self.validate()

    def test_rejects_sandbox_without_network_reconcile_command(self):
        del self.stats['/sbin/ip']
        with self.assertRaisesRegex(PACKAGE.stack.StackError, '/sbin/ip'):
            self.validate()

    def test_rejects_sandbox_without_process_identity_command(self):
        del self.stats['/usr/bin/readlink']
        with self.assertRaisesRegex(PACKAGE.stack.StackError, '/usr/bin/readlink'):
            self.validate()

    def test_rejects_non_executable_sandbox_command(self):
        self.stats['/usr/bin/ssh-keygen'] = ('regular', 0o644, None)
        with self.assertRaisesRegex(PACKAGE.stack.StackError, 'not executable'):
            self.validate()


class SandboxBootTests(unittest.TestCase):
    def test_legacy_foreground_ssh_yields_to_credential_initialization(self):
        container = {
            'command': ['/bin/sh'],
            'args': ['-c', 'mkdir -p /run/sshd; ssh-keygen -A; exec /usr/sbin/sshd -D -e'],
        }
        self.assertTrue(PACKAGE.replace_legacy_sandbox_boot(container))
        self.assertEqual(container, {'command': ['/usr/bin/sleep'], 'args': ['infinity']})
        self.assertFalse(PACKAGE.replace_legacy_sandbox_boot(container))

    def test_preserves_custom_entrypoint(self):
        container = {'command': ['/bin/sh'], 'args': ['-c', 'exec /etc/custom-init']}
        self.assertFalse(PACKAGE.replace_legacy_sandbox_boot(container))
        self.assertEqual(container['args'], ['-c', 'exec /etc/custom-init'])


class AssetValidationTests(unittest.TestCase):
    def test_validates_login_contract_against_sandbox_root_image(self):
        with tempfile.TemporaryDirectory() as temporary:
            assets = Path(temporary)
            files = {}
            for name in (*PACKAGE.ASSETS, 'vmlinux-pvm', 'pvm-host.deb'):
                path = assets / name
                path.write_bytes(name.encode())
                files[name] = hashlib.sha256(name.encode()).hexdigest()
            manifest = {
                'architecture': 'x86_64',
                'files': files,
                'pvm': {'host_kernel_release': 'test', 'boot_args': []},
            }
            validated = []
            PACKAGE.validate_assets(assets, manifest, 'x86_64', login_validator=validated.append)
            self.assertEqual(validated, [assets / 'root.ext4'])

    def test_rejects_old_asset_set_when_only_guest_image_has_login_contract(self):
        with tempfile.TemporaryDirectory() as temporary:
            assets = Path(temporary)
            files = {}
            for name in (*PACKAGE.ASSETS, 'vmlinux-pvm', 'pvm-host.deb'):
                path = assets / name
                path.write_bytes(name.encode())
                files[name] = hashlib.sha256(name.encode()).hexdigest()
            manifest = {
                'architecture': 'x86_64',
                'files': files,
                'pvm': {'host_kernel_release': 'test', 'boot_args': []},
            }

            def login_validator(path):
                self.assertEqual(path, assets / 'root.ext4')
                raise PACKAGE.stack.StackError('Sandbox root image must contain one login-capable cube account')

            with self.assertRaisesRegex(PACKAGE.stack.StackError, 'Sandbox root image'):
                PACKAGE.validate_assets(assets, manifest, 'x86_64', login_validator=login_validator)


if __name__ == '__main__':
    unittest.main()
