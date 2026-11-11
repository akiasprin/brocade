"""Execute the production POSIX installer dispatch without installing software."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
INSTALLER = ROOT / 'crates/brocade-console/templates/install.sh'


class InstallModeTests(unittest.TestCase):
    def dispatch(self, mode, result):
        script = INSTALLER.read_text()
        begin = script.index('    case "$CVM_MODE" in', script.index('# No one-time enrollment credential'))
        end = script.index('\nfi\n\n\n# linux mode', begin)
        dispatch = script[begin:end]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'python3').write_text('#!/bin/sh\nprintf \'%s\\n\' "$@" > "$CVM_TEST_ARGS"\nexit "$CVM_TEST_EXIT"\n')
            (root / 'python3').chmod(0o755)
            environment = {**os.environ, 'PATH': str(root) + os.pathsep + os.environ['PATH'],
                           'CVM_MODE': mode, 'cvm_bundle_file': '/verified/bundle with spaces.tar.gz',
                           'CVM_BUNDLE_SHA256': 'a'*64, 'AGENT_BIN': '/usr/local/bin/brocade-agent',
                           'CVM_TEST_ARGS': str(root / 'args'), 'CVM_TEST_EXIT': str(result)}
            source = 'set -eu\nCVM_REBOOT_PENDING=0\n' + dispatch + '\nprintf \'%s\' "$CVM_REBOOT_PENDING"\n'
            response = subprocess.run(['sh', '-c', source], env=environment, text=True, capture_output=True, timeout=5)
            args = (root / 'args').read_text().splitlines() if (root / 'args').exists() else []
            return response, args

    def test_waiting_for_reboot_continues_agent_enrollment_and_preserves_explicit_mode(self):
        response, args = self.dispatch('pvm', 75)
        self.assertEqual(response.returncode, 0, response.stderr)
        self.assertEqual(response.stdout, '1')
        self.assertEqual(args, ['/usr/local/lib/brocade/cvm-stack.py', 'install', '--bundle',
                                '/verified/bundle with spaces.tar.gz', '--sha256', 'a'*64, '--mode', 'pvm'])

    def test_ordinary_upgrade_does_not_overwrite_a_saved_mode(self):
        response, args = self.dispatch('', 0)
        self.assertEqual(response.returncode, 0, response.stderr)
        self.assertEqual(response.stdout, '0')
        self.assertNotIn('--mode', args)

    def test_real_failure_and_invalid_mode_cannot_be_reported_as_waiting_for_reboot(self):
        response, _ = self.dispatch('auto', 1)
        self.assertEqual(response.returncode, 1)
        response, args = self.dispatch('unknown', 0)
        self.assertEqual(response.returncode, 1)
        self.assertEqual(args, [])


if __name__ == '__main__':
    unittest.main()
