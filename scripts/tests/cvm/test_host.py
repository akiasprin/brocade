"""Virtualization choice, one-shot boot and recovery without touching host services."""
import errno
import json
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
import cvm_host as host
import cvm_stack as stack


KERNEL = '6.6.117-opencloudos9.cubesandbox.pvm.host'
NATIVE = '6.12-debian-test'


def entry(kernel):
    return f"""menuentry 'Debian GNU/Linux {kernel}' --id 'linux-{kernel}' {{
    insmod part_gpt
    search --fs-uuid --set=root original-root-uuid
    linux /boot/vmlinuz-{kernel} root=UUID=original-root-uuid ro net.ifnames=1
    initrd /boot/initrd.img-{kernel}
}}
"""


class HostTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.install = stack.Installation(self.root / 'managed', self.root / 'toolbox', self.root / 'units')
        self.release = self.install.release('a' * 64)
        self.release.mkdir(parents=True)
        self.install.base.chmod(0o700)
        self.boot = self.root / 'boot-id'
        self.boot.write_text('first-boot')
        self.cpu = self.root / 'cpuinfo'
        self.cpu.write_text('flags: vmx')
        self.cmdline = self.root / 'cmdline'
        self.cmdline.write_text('root=UUID=original-root-uuid nopti pti=off')
        self.grub = self.root / 'grub.cfg'
        self.grub.write_text(entry(NATIVE) + entry(KERNEL))
        self.manifest = {'files': {}, 'pvm': {'host_kernel_release': KERNEL, 'host_package': 'runtime/pvm/host.deb',
                                           'guest_kernel': 'runtime/cube-kernel-pvm/vmlinux', 'boot_args': ['nopti', 'pti=off']}}
        self.calls = []
        def run(args, **kwargs):
            self.calls.append(args)
            if args[0] == 'dpkg-deb': return 'amd64' if args[-1] == 'Architecture' else 'linux-image-' + KERNEL
            if '--maintenance-contract' in args: return 'brocade-cvm-maintenance-v1'
            if '--empty' in args: return 'brocade-cvm-empty-v1'
            if 'LoadState' in args: return 'loaded'
            if 'ActiveState' in args: return 'active'
            return ''
        patches = [patch.object(stack, 'verify_release', return_value=self.manifest),
                   patch.object(stack, 'run', side_effect=run), patch.object(stack, 'HOST_BOOT_ID', self.boot),
                   patch.object(stack.shutil, 'which', return_value='/tool'),
                   patch.object(host, 'probe', return_value=None), patch.object(host.Host, 'modules'),
                   patch.object(host.platform, 'release', return_value=NATIVE),
                   patch.object(host, 'GRUB_CONFIG', self.grub), patch.object(host, 'GRUB_SCRIPT', self.root / '42_brocade_cvm'),
                   patch.object(host, 'GRUB_DEFAULT', self.root / '99-brocade-cvm.cfg'),
                   patch.object(host, 'CPUINFO', self.cpu), patch.object(host, 'CMDLINE', self.cmdline)]
        self.mocks = [p.start() for p in patches]
        for p in patches: self.addCleanup(p.stop)
        self.probe = self.mocks[4]
        self.kernel = self.mocks[6]
        self.manager = host.Host(stack, self.install, self.release)

    def test_auto_uses_working_native_kvm_without_changing_boot(self):
        self.probe.return_value = 'kvm'
        self.assertFalse(self.manager.prepare())
        self.assertEqual(self.manager.state()['effective'], 'kvm')
        self.assertEqual(self.manager.state()['desired'], 'auto')
        self.assertEqual(self.calls, [])

    def test_auto_fallback_is_durable_and_waits_for_manual_reboot(self):
        self.assertTrue(self.manager.prepare())
        state = self.manager.state()
        self.assertEqual(state['pending']['mode'], 'pvm')
        self.assertEqual(state['pending']['phase'], 'awaiting_reboot')
        self.assertIn(['grub-reboot', 'brocade-cvm-pvm'], self.calls)
        self.assertFalse(any('reboot' in command for command in self.calls))
        generated = (self.root / '42_brocade_cvm').read_text()
        self.assertIn('net.ifnames=1 nopti pti=off', generated)
        self.assertNotIn('ipv6.disable', generated)
        self.assertIn('original-root-uuid', generated)
        self.assertNotIn('Before=brocade-cvm-cubelet.service', (self.install.units / 'brocade-cvm-resume.service').read_text())
        before = list(self.calls)
        self.assertTrue(self.manager.prepare())
        self.assertEqual(before, self.calls, 'retry while awaiting reboot must not reinstall the kernel')

    def test_reboot_continues_only_the_frozen_kernel_pair(self):
        self.manager.prepare()
        self.boot.write_text('second-boot')
        self.probe.return_value = 'pvm'
        self.kernel.return_value = KERNEL
        self.assertFalse(self.manager.prepare())
        self.manager.commit()
        self.assertEqual(self.manager.state()['effective'], 'pvm')
        self.assertNotIn('pending', self.manager.state())
        self.assertIn(['grub-set-default', 'brocade-cvm-pvm'], self.calls)

    def test_wrong_kernel_or_missing_required_arguments_never_commits(self):
        self.manager.prepare()
        self.boot.write_text('second-boot')
        self.probe.return_value = 'pvm'
        with self.assertRaisesRegex(stack.StackError, 'pinned kernel pair'):
            self.manager.prepare()
        self.kernel.return_value = KERNEL
        self.cmdline.write_text('root=UUID=original-root-uuid')
        with self.assertRaisesRegex(stack.StackError, 'boot arguments'):
            self.manager.prepare()
        self.assertIn('pending', self.manager.state())

    def test_forced_native_mode_does_not_fallback(self):
        self.cpu.write_text('flags: aes')
        with self.assertRaisesRegex(stack.StackError, 'VMX/SVM'):
            self.manager.prepare('kvm')
        self.assertEqual(self.calls, [])
        self.assertFalse(self.manager.path.exists())

    def test_missing_pvm_payload_is_actionable_before_host_changes(self):
        self.manager.pvm = None
        with self.assertRaisesRegex(stack.StackError, 'no pinned PVM'):
            self.manager.prepare('auto')
        self.assertEqual(self.calls, [])
        self.assertFalse(self.manager.path.exists())

    def test_explicit_pvm_then_back_to_native_retains_the_original_boot_entry(self):
        self.probe.return_value = 'kvm'
        self.manager.prepare('pvm')
        self.boot.write_text('second-boot')
        self.probe.return_value = 'pvm'
        self.kernel.return_value = KERNEL
        self.manager.prepare()
        self.manager.commit()
        self.assertTrue(self.manager.prepare('kvm'))
        self.assertEqual(self.manager.state()['desired'], 'kvm')
        self.assertEqual(self.manager.state()['pending']['kernel'], NATIVE)
        self.assertIn(['grub-reboot', 'brocade-cvm-native'], self.calls)
        # PVM remains a bootable fallback until the native boot succeeds.
        self.assertIn('brocade-cvm-pvm', (self.root / '42_brocade_cvm').read_text())

    def test_pending_choice_cannot_be_replaced_by_another_mode_or_bundle(self):
        self.manager.prepare()
        with self.assertRaisesRegex(stack.StackError, 'transition is pending'):
            self.manager.prepare('kvm')
        self.manager.release = self.install.release('b' * 64)
        with self.assertRaisesRegex(stack.StackError, 'transition is pending'):
            self.manager.prepare()

    def test_interrupted_preparation_retries_the_same_intent(self):
        self.manager.prepare()
        state = self.manager.state()
        state['pending']['phase'] = 'preparing'
        self.manager.save(state)
        self.assertTrue(self.manager.prepare())
        self.assertEqual(self.manager.state()['pending']['phase'], 'awaiting_reboot')

    def test_foreign_boot_configuration_is_preserved(self):
        target = self.root / '42_brocade_cvm'
        target.write_text('administrator boot policy\n')
        with self.assertRaisesRegex(stack.StackError, 'not installer-owned'):
            self.manager.prepare()
        self.assertEqual(target.read_text(), 'administrator boot policy\n')
        self.assertFalse(self.manager.path.exists())

    def test_generated_boot_script_is_executable_and_does_not_expand_grub_variables(self):
        self.grub.write_text(entry(NATIVE).replace('    insmod part_gpt', '    echo "$root"\n    insmod part_gpt') + entry(KERNEL))
        self.manager.prepare()
        result = subprocess.check_output([str(self.root / '42_brocade_cvm')], text=True, timeout=5)
        self.assertIn('echo "$root"', result)
        self.assertIn('brocade-cvm-pvm', result)

    def test_resume_after_failed_candidate_boot_restores_original_configuration(self):
        stack.atomic_json(self.install.state_path, {'current': self.release.name})
        generated = self.release / 'generated/cube.json'
        stack.atomic_json(generated, {'keep': 'original'})
        self.manager.prepare('pvm')
        stack.atomic_json(generated, {'keep': 'candidate'})
        self.boot.write_text('after-failed-boot')
        self.probe.return_value = 'kvm'
        with patch.object(stack, 'host_manager', return_value=self.manager), patch.object(stack, 'preflight'), patch.object(self.install, 'activate') as activate:
            with self.assertRaisesRegex(stack.StackError, 'original mode restored'):
                self.install.set_mode(None, self.release.name)
        activate.assert_called_once_with(self.release.name)
        self.assertNotIn('pending', self.manager.state())
        self.assertEqual(stack.read_json(generated), {'keep': 'original'})

    def test_mode_switch_fences_then_stops_agent_and_proves_node_empty(self):
        stack.atomic_json(self.install.state_path, {'current': self.release.name})
        generated = self.release / 'generated/cube.json'
        stack.atomic_json(generated, {'keep': 'original'})
        self.manager.prepare('pvm')
        stop = ['systemctl', 'stop', 'brocade-agent.service']
        start = ['systemctl', 'start', 'brocade-agent.service']
        empty = next(command for command in self.calls if '--empty' in command)
        self.assertLess(self.calls.index(stop), self.calls.index(empty))
        self.assertLess(self.calls.index(empty), self.calls.index(start))
        stack.atomic_json(generated, {'keep': 'changed'})
        self.probe.return_value = 'kvm'
        self.manager.cancel()
        self.assertEqual(stack.read_json(generated), {'keep': 'original'})
        self.assertFalse((self.install.base / 'host-backup').exists())
        self.assertNotIn('pending', self.manager.state())

    def test_nonempty_node_leaves_no_boot_changes_and_agent_is_restarted(self):
        stack.atomic_json(self.install.state_path, {'current': self.release.name})
        original = stack.run.side_effect
        def nonempty(command, **kwargs):
            if '--empty' in command: raise stack.StackError('brocade-agent failed')
            return original(command, **kwargs)
        with patch.object(stack, 'run', side_effect=nonempty), self.assertRaises(stack.StackError):
            self.manager.prepare('pvm')
        self.assertIn(['systemctl', 'start', 'brocade-agent.service'], self.calls)
        self.assertFalse((self.root / '42_brocade_cvm').exists())
        self.assertNotIn('pending', self.manager.state(), 'a rejected drain must leave existing instances operable')


class ProbeTests(unittest.TestCase):
    def test_permission_errors_never_select_pvm(self):
        with patch('builtins.open', side_effect=PermissionError(errno.EACCES, 'denied')):
            with self.assertRaises(PermissionError): host.probe()
        with patch('builtins.open', side_effect=FileNotFoundError(errno.ENOENT, 'missing')):
            self.assertIsNone(host.probe())

    def test_device_visibility_does_not_replace_a_real_vm_creation_probe(self):
        from unittest.mock import mock_open
        with patch('builtins.open', mock_open()), patch.object(host.fcntl, 'ioctl', side_effect=[12, 123]) as ioctl, patch.object(host.os, 'close') as close, patch.object(host, 'PVM_MODULE', Path('/a-missing-test-module')):
            self.assertEqual(host.probe(), 'kvm')
        self.assertEqual(ioctl.call_args.args[1], 0xAE01)
        close.assert_called_once_with(123)

    def test_boot_entry_requires_the_exact_kernel_and_preserves_storage_identity(self):
        value = host.boot_entry(entry(NATIVE), NATIVE, 'brocade-cvm-native')
        self.assertIn('root=UUID=original-root-uuid', value)
        self.assertIn('initrd /boot/initrd.img-' + NATIVE, value)
        with self.assertRaises(ValueError): host.boot_entry(entry(NATIVE), KERNEL, 'candidate')


if __name__ == '__main__':
    unittest.main()
