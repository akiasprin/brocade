import subprocess
import unittest
from unittest.mock import patch

from install_kvm import remote_boot_id


class RebootDetectionTests(unittest.TestCase):
    def test_ssh_host_key_warning_cannot_change_the_observed_boot_identity(self):
        boot = '1b545aea-7a5a-4f4b-8adc-7fa344aa51e4'
        response = subprocess.CompletedProcess([], 0, stdout=(boot + '\n').encode(),
                                               stderr=b'Warning: Permanently added host to known hosts.\n')
        with patch('install_kvm.subprocess.run', return_value=response):
            self.assertEqual(remote_boot_id(['ssh', 'owned-test-vm']), boot)

    def test_ssh_timeout_during_reboot_is_not_a_new_boot(self):
        with patch('install_kvm.subprocess.run', side_effect=subprocess.TimeoutExpired(['ssh'], 10)):
            self.assertIsNone(remote_boot_id(['ssh', 'owned-test-vm']))

    def test_empty_or_invalid_output_is_not_a_new_boot(self):
        for output in (b'', b'Warning: connection is starting\n'):
            with self.subTest(output=output), patch('install_kvm.subprocess.run',
                    return_value=subprocess.CompletedProcess([], 0, stdout=output, stderr=b'')):
                self.assertIsNone(remote_boot_id(['ssh', 'owned-test-vm']))


if __name__ == '__main__':
    unittest.main()
