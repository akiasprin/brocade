"""Cold stack upgrade tests with synthetic bundles and a mocked service boundary."""

from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
import cvm_stack as stack


class ColdStackTests(unittest.TestCase):
    roots = 'a' * 64

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.install = stack.Installation(self.root / 'managed', self.root / 'toolbox', self.root / 'units')
        self.patch(stack, 'STATE', self.root / 'storage')
        self.patch(stack, 'local_storage_backend', return_value='xfs')
        self.patch(stack, 'host_compatibility')
        self.boot = self.patch(stack, 'verify_stack_boot')
        self.host = self.patch(stack, 'host_manager').return_value
        self.host.state.return_value = {}
        self.host.mode.return_value = 'kvm'
        self.source_archive, self.source = self.bundle('source')
        self.target_archive, self.target = self.bundle('target')
        self.install.stage_release(self.source_archive, self.source['bundle_sha256'], lambda *_: None)
        self.source_id, self.target_id = self.source['bundle_sha256'], self.target['bundle_sha256']
        # An existing installation predates the remote installer receipt.
        (self.install.release(self.source_id) / stack.BUNDLE_RECEIPT).unlink()
        self.live(self.source)
        stack.atomic_json(self.install.state_path, {'current': self.source_id})
        self.agent_active = True
        self.run = self.patch(stack, 'run', side_effect=self.command)
        self.hash_process = self.patch(stack, 'running_cubelet', side_effect=self.process)
        self.cold = self.patch(stack, 'cold_evidence')
        self.activate = self.patch(self.install, 'activate', side_effect=self.switch)

    def patch(self, owner, name, *args, **kwargs):
        patcher = patch.object(owner, name, *args, **kwargs)
        self.addCleanup(patcher.stop)
        return patcher.start()

    def bundle(self, label):
        tree = self.root / label
        for name in stack.REQUIRED:
            path = tree / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(label + '\n')
            path.chmod(0o755 if '/bin/' in name else 0o644)
        artifacts = {
            'cubelet_sha256': stack.digest(tree / stack.CUBELET),
            'shim_sha256': stack.digest(tree / stack.SHIM),
            'storage_backend': 'xfs',
        }
        stack.atomic_json(
            tree / 'config/network.json',
            {
                'shim_sha256': artifacts['shim_sha256'],
                'shim_version': label,
                'state_dir': '/data/cubelet/network-attachments-v2',
            },
        )
        stack.atomic_json(tree / 'config/cube.json', {'cubelet_socket': stack.CUBELET_SOCKET})
        archive = self.root / (label + '.tar.gz')
        with redirect_stdout(io.StringIO()):
            stack.pack(tree, archive, 'x86_64', 'c' * 64, None)
        return archive, dict(artifacts, bundle_sha256=stack.digest(archive))

    @property
    def marker(self):
        return self.install.base / stack.STACK_MARKER

    def live(self, artifacts):
        identity = artifacts['bundle_sha256']
        release = self.install.release(identity)
        native = stack.read_json(release / 'config/network.json')
        stack.atomic_json(
            release / 'generated/cube.json',
            {'virtualization_mode': 'kvm', 'storage_backend': 'xfs', 'cubelet_socket': stack.CUBELET_SOCKET},
        )
        stack.atomic(
            release / 'generated/cubelet.toml',
            f'root = "{stack.STATE / "root"}"\nstate = "{stack.STATE / "state"}"\n[grpc]\naddress = "{stack.CUBELET_SOCKET}"\n'.encode(),
        )
        stack.atomic_json(release / 'generated/network.json', native)
        installed = (
            stack.STATE / 'root/component_versions/cube-shim' / native['shim_version'] / 'bin' / Path(stack.SHIM).name
        )
        stack.copy_immutable(release / stack.SHIM, installed)
        stack.point(self.install.base / 'current', release)
        stack.point(self.install.toolbox, release / 'runtime')
        stack.atomic(
            self.install.units / stack.UNITS[0],
            f'# Managed by Brocade CVM\nExecStart=/usr/bin/python3 {stack.MANAGER} start-cubelet --release {identity}\n'.encode(),
        )
        stack.atomic(
            self.install.units / 'brocade-agent.service.d/brocade-cvm.conf',
            (
                f'# Managed by Brocade CVM\nEnvironment=BROCADE_CVM_CONFIG={self.install.base}/current/generated/cube.json\n'
                'Environment=BROCADE_CVM_STATE_DIR=/var/lib/brocade/cvm-cube\n'
            ).encode(),
        )
        self.running = identity

    def command(self, args, **_):
        if args[:4] == ['systemctl', 'show', '--value', '-p']:
            key, unit = args[4:]
            active = self.agent_active if unit == stack.AGENT_UNIT else self.running is not None
            return {
                'LoadState': 'loaded',
                'ActiveState': 'active' if active else 'inactive',
                'MainPID': '123' if active else '0',
                'Job': '',
            }[key]
        if args == ['systemctl', 'stop', stack.AGENT_UNIT]:
            self.agent_active = False
            return ''
        if args == ['systemctl', 'start', stack.AGENT_UNIT]:
            self.agent_active = True
            return ''
        self.fail(f'unexpected command: {args}')

    def process(self, release, digest, pid):
        return pid == '123' and release.name == self.running and digest == stack.digest(release / stack.CUBELET)

    def switch(self, identity, **_):
        self.assertFalse(self.agent_active)
        self.live(self.target if identity == self.target_id else self.source)
        self.agent_active = True

    def stage_target(self):
        self.install.stack_stage(self.target_archive, self.source, self.target)

    def stage_and_fence(self):
        self.stage_target()
        stack.atomic_json(self.marker, {'job': 'approved'})

    def config(self, identity):
        return self.install.release(identity) / 'generated/cube.json'

    def test_stage_is_additive_and_inspect_reports_observed_identity(self):
        self.stage_target()
        self.assertEqual(self.install.stack_inspect(), self.source)
        self.assertEqual(self.install.state(), {'current': self.source_id})
        self.assertTrue(self.agent_active)
        self.assertIsNone(self.install.state().get('pending'))
        target = self.install.release(self.target_id)
        self.assertTrue((target / stack.BUNDLE_RECEIPT).exists())
        self.assertFalse((target / 'generated').exists())
        self.activate.assert_not_called()
        self.cold.assert_not_called()
        self.assertNotIn('stop', [call.args[0][1] for call in self.run.call_args_list])
        stack.atomic_json(self.marker, {'job': 'approved'})
        with (
            patch.object(stack, 'Installation', return_value=self.install),
            patch.object(sys, 'argv', ['cvm-stack.py', 'stack-inspect']),
        ):
            stdout = io.StringIO()
            with redirect_stdout(stdout):
                stack.main()
        self.assertEqual(json.loads(stdout.getvalue()), self.source)

    def test_inspect_rejects_an_unexpected_pid_shim_or_pending_mode_and_reuses_inventory_cache(self):
        self.assertEqual(self.install.stack_inspect(), self.source)
        guest = self.install.release(self.source_id) / 'runtime/cube-agent/cube-agent.ext4'
        with patch.object(stack, 'digest', wraps=stack.digest) as hashes:
            self.assertEqual(self.install.stack_inspect(), self.source)
        self.assertNotIn(guest, [call.args[0] for call in hashes.call_args_list])
        self.hash_process.return_value = False
        self.hash_process.side_effect = None
        with self.assertRaisesRegex(stack.StackError, 'running Cubelet'):
            self.install.stack_inspect()
        self.hash_process.side_effect = self.process
        self.host.state.return_value = {'pending': {'mode': 'kvm'}}
        with self.assertRaisesRegex(stack.StackError, 'no host mode transition'):
            self.install.stack_inspect()
        self.host.state.return_value = {}
        installed = stack.STATE / 'root/component_versions/cube-shim/source/bin' / Path(stack.SHIM).name
        installed.write_bytes(b'unexpected managed Shim')
        with self.assertRaisesRegex(stack.StackError, 'installed Shim differs'):
            self.install.stack_inspect()

    def test_stage_rejects_wrong_target_digest_before_publishing(self):
        target = {**self.target, 'shim_sha256': '0' * 64}
        with self.assertRaisesRegex(stack.StackError, 'approved artifacts'):
            self.install.stack_stage(self.target_archive, self.source, target)
        self.assertFalse(self.install.release(self.target_id).exists())
        self.assertTrue(self.agent_active)
        self.activate.assert_not_called()

    def test_activation_checks_target_binding_and_marker_before_stopping(self):
        self.stage_target()
        with self.assertRaisesRegex(stack.StackError, 'marker is required'):
            self.install.stack_activate(self.source, self.target, self.roots)
        stack.atomic_json(self.marker, {})
        with self.assertRaisesRegex(stack.StackError, 'no approved job'):
            self.install.stack_activate(self.source, self.target, self.roots)
        stack.atomic_json(self.marker, {'job': 'approved'})
        manifest = self.install.release(self.target_id) / 'manifest.json'
        stack.atomic_json(manifest, {**stack.read_json(manifest), 'provenance': 'different'})
        with self.assertRaisesRegex(stack.StackError, 'verified bundle'):
            self.install.stack_activate(self.source, self.target, self.roots)
        self.assertFalse(self.install.stack_checkpoint.exists())
        self.assertNotIn('stop', [call.args[0][1] for call in self.run.call_args_list])
        self.activate.assert_not_called()

    def test_activation_cold_checks_wrap_switch_and_marker_stays_owned_by_caller(self):
        self.stage_and_fence()
        checks = []

        def verify(config, roots, *, read_only=False):
            checks.append((config, roots, read_only, self.agent_active))

        self.cold.side_effect = verify
        self.install.stack_activate(self.source, self.target, self.roots)
        self.assertEqual(
            checks,
            [
                (self.config(self.source_id), self.roots, False, False),
                (self.config(self.target_id), self.roots, True, True),
            ],
        )
        self.activate.assert_called_once_with(self.target_id)
        self.assertEqual(self.install.stack_inspect(), self.target)
        self.assertFalse(self.install.stack_checkpoint.exists())
        self.assertTrue(self.marker.exists())
        self.boot.assert_called_once()

    def test_pending_boot_preflight_cannot_stop_the_agent(self):
        self.stage_and_fence()
        self.boot.side_effect = stack.StackError('a new host boot needs local initialization')
        with self.assertRaisesRegex(stack.StackError, 'host boot'):
            self.install.stack_activate(self.source, self.target, self.roots)
        self.assertTrue(self.agent_active)
        self.assertFalse(self.install.stack_checkpoint.exists())
        self.cold.assert_not_called()
        self.activate.assert_not_called()

    def test_failed_preswitch_check_only_restarts_agent_and_recovery_is_evidence_only(self):
        self.stage_and_fence()
        self.cold.side_effect = [stack.StackError('roots drifted'), None]
        with self.assertRaisesRegex(stack.StackError, 'roots drifted'):
            self.install.stack_activate(self.source, self.target, self.roots)
        self.assertEqual(self.install.state(), {'current': self.source_id})
        self.assertEqual(self.running, self.source_id)
        self.assertTrue(self.agent_active)
        with self.assertRaisesRegex(stack.StackError, 'stack-recover'):
            self.install.stack_activate(self.source, self.target, self.roots)
        self.install.stack_recover(self.source_id, self.roots)
        self.cold.assert_called_with(self.config(self.source_id), self.roots, read_only=True)
        self.activate.assert_not_called()
        self.assertFalse(self.install.stack_checkpoint.exists())
        self.assertTrue(self.marker.exists())

    def test_interrupted_candidate_is_recovered_to_source_once(self):
        self.stage_and_fence()

        def failure(identity, **_):
            self.live(self.target)
            self.running = None
            raise stack.StackError('candidate startup failed')

        self.activate.side_effect = failure
        with self.assertRaisesRegex(stack.StackError, 'startup failed'):
            self.install.stack_activate(self.source, self.target, self.roots)
        self.activate.assert_called_once_with(self.target_id)
        self.assertEqual(self.install.state()['pending'], self.target_id)
        self.activate.side_effect = self.switch
        self.install.stack_recover(self.source_id, self.roots)
        self.activate.assert_called_with(self.source_id, restore=True)
        self.assertEqual(self.activate.call_count, 2)
        self.cold.assert_called_with(self.config(self.source_id), self.roots, read_only=True)
        self.assertEqual(self.install.stack_inspect(), self.source)
        self.install.stack_recover(self.source_id, self.roots)
        self.assertEqual(self.activate.call_count, 2)

    def test_postcheck_failure_requires_new_cold_proof_before_reverting_live_target(self):
        self.stage_and_fence()
        self.cold.side_effect = [
            None,
            stack.StackError('target evidence unknown'),
            stack.StackError('target still unknown'),
            None,
            None,
        ]
        with self.assertRaisesRegex(stack.StackError, 'target evidence unknown'):
            self.install.stack_activate(self.source, self.target, self.roots)
        self.assertEqual(self.running, self.target_id)
        with self.assertRaisesRegex(stack.StackError, 'target still unknown'):
            self.install.stack_recover(self.source_id, self.roots)
        self.activate.assert_called_once_with(self.target_id)
        self.install.stack_recover(self.source_id, self.roots)
        self.assertEqual(self.activate.call_count, 2)
        self.assertEqual(self.running, self.source_id)

    def test_unknown_recovery_failure_does_not_retry_mutation(self):
        self.stage_and_fence()
        self.cold.side_effect = [None, stack.StackError('verification failed'), None]
        with self.assertRaisesRegex(stack.StackError, 'verification failed'):
            self.install.stack_activate(self.source, self.target, self.roots)
        self.activate.side_effect = stack.StackError('recovery could not commit')
        with self.assertRaisesRegex(stack.StackError, 'recovery could not commit'):
            self.install.stack_recover(self.source_id, self.roots)
        with self.assertRaisesRegex(stack.StackError, 'refusing to repeat'):
            self.install.stack_recover(self.source_id, self.roots)
        self.assertEqual(self.activate.call_count, 2)
        self.assertTrue(self.marker.exists())

    def test_recovery_refuses_wrong_roots_foreign_links_and_pvm(self):
        self.stage_and_fence()
        self.cold.side_effect = [None, stack.StackError('verification failed')]
        with self.assertRaises(stack.StackError):
            self.install.stack_activate(self.source, self.target, self.roots)
        with self.assertRaisesRegex(stack.StackError, 'recorded cold stack upgrade'):
            self.install.stack_recover(self.source_id, '0' * 64)
        self.host.mode.return_value = 'pvm'
        with self.assertRaisesRegex(stack.StackError, 'native KVM'):
            self.install.stack_recover(self.source_id, self.roots)
        self.host.mode.return_value = 'kvm'
        stack.point(self.install.base / 'current', self.root / 'foreign')
        with self.assertRaisesRegex(stack.StackError, 'runtime links'):
            self.install.stack_recover(self.source_id, self.roots)
        self.activate.assert_called_once_with(self.target_id)

    def test_ordinary_cli_mutators_reject_an_existing_marker(self):
        self.marker.symlink_to(
            self.root / 'missing'
        )  # Unreadable or foreign markers still fence ordinary CLI operations.
        self.patch(stack, 'Installation', return_value=self.install)
        self.patch(stack, 'ProgressReporter', return_value=Mock())
        for args in (
            ['install', '--bundle', 'no', '--sha256', self.target_id],
            ['check', '--bundle', 'no', '--sha256', self.target_id],
            ['rollback'],
            ['mode', 'kvm'],
            ['resume', '--release', self.source_id],
            ['cancel-mode'],
            ['recover'],
        ):
            with self.subTest(args=args), patch.object(sys, 'argv', ['cvm-stack.py', *args]):
                with self.assertRaisesRegex(stack.StackError, 'remote CVM stack maintenance'):
                    stack.main()
        self.run.assert_not_called()

    def test_runtime_identity_checks_the_actual_inode_and_digest(self):
        process = self.root / 'proc/123'
        process.mkdir(parents=True)
        release = self.install.release(self.source_id)
        binary = release / stack.CUBELET
        self.hash_process.side_effect = None
        with (
            patch.object(stack, 'PROC', process.parent),
            patch.object(stack, 'running_cubelet', original_running_cubelet),
        ):
            copy = self.root / 'same-content'
            copy.write_bytes(binary.read_bytes())
            os.link(copy, process / 'exe')
            self.assertFalse(stack.running_cubelet(release, self.source['cubelet_sha256'], '123'))
            (process / 'exe').unlink()
            os.link(binary, process / 'exe')
            self.assertTrue(stack.running_cubelet(release, self.source['cubelet_sha256'], '123'))
            self.assertFalse(stack.running_cubelet(release, '0' * 64, '123'))

    def test_external_cold_check_accepts_only_the_exact_digest_and_hides_stderr(self):
        config, caller = self.config(self.source_id), original_cold_evidence
        with patch.object(stack.subprocess, 'run') as agent:
            agent.return_value = subprocess.CompletedProcess([], 0, (self.roots + '\n').encode())
            caller(config, self.roots, read_only=True)
            self.assertEqual(
                agent.call_args.args[0],
                [str(stack.AGENT_BIN), 'cvm-check', '--stack-cold-read-only', '--config', str(config)],
            )
            self.assertEqual(agent.call_args.kwargs['stderr'], subprocess.DEVNULL)
            self.assertGreater(agent.call_args.kwargs['timeout'], 0)
            agent.return_value.stdout += b'extra text'
            with self.assertRaisesRegex(stack.StackError, 'did not match'):
                caller(config, self.roots)
            agent.side_effect = subprocess.TimeoutExpired('secret path', 1)
            with self.assertRaisesRegex(stack.StackError, '^Agent --stack-cold check did not complete$'):
                caller(config, self.roots)


class FirstInstallTests(unittest.TestCase):
    patch = ColdStackTests.patch
    bundle = ColdStackTests.bundle
    roots = '5fb434637d901f717fc49a9ea27d0e92390928297183e793610b1e22ce4ddde8'
    uuid = '095651a0-b544-4a9e-99e6-997035521fd2'

    def setUp(self):
        previous_umask = os.umask(0o077)
        self.addCleanup(os.umask, previous_umask)
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.install = stack.Installation(self.root / 'managed', self.root / 'toolbox', self.root / 'units')
        self.marker = self.install.base / stack.STACK_MARKER
        self.checkpoint = self.install.stack_install_checkpoint
        for name in ('STATE', 'EXECUTOR_STATE', 'CVM_CONFIG_DIR', 'AGENT_STATE', 'PROC', 'CUBELET_RUN', 'CUBELET_PID'):
            self.patch(stack, name, self.root / name.lower())
        stack.PROC.mkdir()
        self.patch(stack, 'FRESH_FOREIGN_PATHS', (self.root / 'foreign-host-config',))
        self.patch(stack, 'RUNTIME_DIRECTORIES', (self.root / 'runtime',))
        owned = stack.owned
        # The production installer requires root and private path ancestors. The
        # fixture is private, but lives below the test runner's shared temp root.
        self.patch(
            stack,
            'owned',
            side_effect=lambda path, *args: path.lstat() if path in self.root.parents else owned(path, *args),
        )
        self.host_check = self.patch(stack, 'fresh_native_host')
        self.patch(stack, 'host_compatibility')
        self.patch(stack, 'probe_storage')
        self.patch(stack, 'verify_stack_boot')
        self.patch(stack, 'local_storage_backend', return_value='xfs')
        host = self.patch(stack, 'host_manager').return_value
        host.state.return_value = {}
        host.mode.return_value = 'kvm'
        self.cold = self.patch(stack, 'cold_evidence')
        self.activate = self.patch(self.install, 'activate', side_effect=self.publish)
        self.patch(stack, 'running_cubelet', side_effect=lambda *args: self.running)
        self.run = self.patch(stack, 'run', side_effect=self.command)
        self.agent_active = True
        self.running = False
        self.units = ''
        self.sockets = ''
        self.filesystem = 'xfs'
        self.mounts = ''
        self.environment = ''
        self.envfiles = ''
        self.archive, self.target = self.bundle('target')
        self.identity = self.target['bundle_sha256']
        self.release = self.install.release(self.identity)

    def command(self, args, **_):
        if args[:4] == ['systemctl', 'show', '--value', '-p']:
            key, unit = args[4:]
            active = self.agent_active if unit == stack.AGENT_UNIT else self.running
            return {
                'LoadState': 'loaded',
                'ActiveState': 'active' if active else 'inactive',
                'MainPID': '123' if active else '0',
                'Job': '',
                'Environment': self.environment,
                'EnvironmentFiles': self.envfiles,
                'ExecStart': '',
                'DropInPaths': '',
                'FragmentPath': str(self.install.units / stack.UNITS[0]),
            }[key]
        if args[:2] in (['systemctl', 'list-unit-files'], ['systemctl', 'list-units']):
            return self.units
        if args[:2] == ['ss', '-H']:
            return self.sockets
        if args == ['findmnt', '-rn', '-o', 'TARGET']:
            return self.mounts
        if args[0] == 'findmnt':
            return f'{self.filesystem} {self.uuid} rw,relatime'
        if args == ['systemctl', 'stop', stack.AGENT_UNIT]:
            self.agent_active = False
            return ''
        self.fail(f'unexpected command: {args}')

    def publish(self, identity, *, install_job, restore):
        self.assertEqual(install_job, 21)
        self.assertFalse(self.agent_active)
        self.assertTrue(self.checkpoint.exists())
        self.assertEqual(identity, self.identity)
        ColdStackTests.live(self, self.target)
        generated = self.release / 'generated'
        stack.atomic_json(generated / stack.FRESH_CONFIG_RECEIPT, stack.fresh_config_receipt(generated, install_job))
        for path, data in self.install.activation_units(identity).items():
            stack.atomic(path, data, 0o644)
        self.running = True
        self.agent_active = True

    def stage(self):
        self.install.stack_install_stage(self.archive, self.target)

    def claim(self, *, id=21, phase='started', **fields):
        payload = {
            'contract': 'brocade-cvm-stack-fresh-install-v1',
            'architecture': 'x86_64',
            'target_sha256': self.identity,
            'target_cubelet_sha256': self.target['cubelet_sha256'],
            'target_shim_sha256': self.target['shim_sha256'],
            'storage_backend': self.target['storage_backend'],
            'target_bytes': self.archive.stat().st_size,
            **fields,
        }
        stack.atomic_json(
            self.marker,
            {
                'offer': {'id': id, 'status': 'applying', 'artifact': {'payload': json.dumps(payload)}},
                'roots_sha256': self.roots,
                'phase': phase,
            },
        )

    def apply(self, *, resume=False):
        self.install.stack_install_activate(self.target, self.roots, resume=resume)

    def test_empty_host_lists_units_without_systemd_globs(self):
        # Ubuntu's systemctl list-unit-files returns 1 for a glob with no matches.
        def list_units(args, **kwargs):
            if args[:2] == ['systemctl', 'list-unit-files']:
                if any('*' in arg for arg in args):
                    raise stack.StackError('systemctl failed (exit 1)')
                return 'brocade-agent.service enabled enabled\npostgresql.service enabled enabled'
            return self.command(args, **kwargs)

        self.run.side_effect = list_units
        self.assertEqual(self.install.stack_install_inspect(), 'xfs')
        self.units = 'cubelet.service loaded active running'
        with self.assertRaisesRegex(stack.StackError, 'Cube unit prevents fresh'):
            self.install.stack_install_inspect()

    def test_inspect_reports_json_string_after_staging_and_during_claim(self):
        self.assertEqual(self.install.stack_install_inspect(), 'xfs')
        self.assertFalse(stack.STATE.exists())
        self.stage()
        self.claim(phase='claiming')
        with (
            patch.object(stack, 'Installation', return_value=self.install),
            patch.object(sys, 'argv', ['cvm-stack.py', 'stack-install-inspect']),
            redirect_stdout(io.StringIO()) as output,
        ):
            stack.main()
        self.assertEqual(output.getvalue(), '"xfs"\n')
        self.assertTrue((self.release / stack.BUNDLE_RECEIPT).exists())
        self.assertTrue((stack.STATE / 'storage').is_dir())
        self.assertFalse((self.release / 'generated').exists())
        self.assertTrue(self.agent_active)
        self.activate.assert_not_called()

    def test_cli_mutators_forward_only_fresh_target_and_root_arguments(self):
        arguments = [
            '--target',
            self.identity,
            '--target-cubelet',
            self.target['cubelet_sha256'],
            '--target-shim',
            self.target['shim_sha256'],
            '--storage',
            'xfs',
        ]
        for command, extra, method, expected, keywords in (
            (
                'stack-install-stage',
                ['--bundle', str(self.archive)],
                'stack_install_stage',
                (self.archive, self.target),
                {},
            ),
            (
                'stack-install-activate',
                ['--expected-roots-sha256', self.roots],
                'stack_install_activate',
                (self.target, self.roots),
                {'resume': False},
            ),
            (
                'stack-install-resume',
                ['--expected-roots-sha256', self.roots],
                'stack_install_activate',
                (self.target, self.roots),
                {'resume': True},
            ),
        ):
            with (
                self.subTest(command=command),
                patch.object(stack, 'Installation', return_value=self.install),
                patch.object(stack, 'REPORTER', None),
                patch.object(stack, 'ProgressReporter'),
                patch.object(sys, 'argv', ['cvm-stack.py', command, *arguments, *extra]),
                patch.object(self.install, method) as call,
            ):
                stack.main()
                call.assert_called_once_with(*expected, **keywords)

    def test_inspect_supported_plain_mount_and_rejects_unknown_or_failed_checks(self):
        self.filesystem = 'ext4'
        (stack.STATE / 'storage/lost+found').mkdir(parents=True)
        self.mounts = str(stack.STATE / 'storage')
        self.assertEqual(self.install.stack_install_inspect(), 'plain')
        self.mounts += f'\n{stack.STATE / "state"}'
        with self.assertRaisesRegex(stack.StackError, 'metadata or nested storage mount'):
            self.install.stack_install_inspect()
        self.mounts = ''
        self.assertEqual(
            stack.empty_roots_digest('plain'), '96e8d03177e44127e21912f10459be3d20c15e8630465f7b11002ae9abba78d8'
        )
        self.filesystem = 'tmpfs'
        with self.assertRaisesRegex(stack.StackError, 'persistent'):
            self.install.stack_install_inspect()
        self.host_check.side_effect = OSError('cannot inspect KVM')
        with self.assertRaisesRegex(OSError, 'cannot inspect KVM'):
            self.install.stack_install_inspect()

    def test_inspect_rejects_managed_stale_and_unmanaged_footprints(self):
        for path in (
            self.install.state_path,
            self.install.base / 'host.json',
            self.install.stack_checkpoint,
            self.checkpoint,
            self.install.toolbox,
            stack.STATE / 'storage/disk.img',
            stack.STATE / 'network-attachments-v2/record',
            stack.EXECUTOR_STATE / 'deleted.json',
            stack.CVM_CONFIG_DIR / 'cvm-cube.json',
            stack.AGENT_STATE / 'cvm-instance-checkpoint.json',
            stack.CUBELET_RUN / 'cubelet.sock',
        ):
            with self.subTest(path=path):
                stack.atomic(path, b'{}')
                try:
                    with self.assertRaises(stack.StackError):
                        self.install.stack_install_inspect()
                finally:
                    path.unlink()
                    if path.parent.name == 'network-attachments-v2':
                        path.parent.rmdir()
        self.install.toolbox.symlink_to(self.root / 'missing-runtime')
        with self.assertRaisesRegex(stack.StackError, 'runtime links'):
            self.install.stack_install_inspect()

    def test_inspect_rejects_units_processes_sockets_and_agent_overrides(self):
        self.units = 'cube-sandbox-cubelet.service disabled disabled'
        with self.assertRaisesRegex(stack.StackError, 'Cube unit'):
            self.install.stack_install_inspect()
        self.units = ''
        self.sockets = 'u_str LISTEN 0 32 /custom/cubelet.sock 100 * 0'
        with self.assertRaisesRegex(stack.StackError, 'Cube socket'):
            self.install.stack_install_inspect()
        self.sockets = ''
        stack.atomic(stack.PROC / '42/comm', b'Cubelet\n')
        with self.assertRaisesRegex(stack.StackError, 'Cube process'):
            self.install.stack_install_inspect()
        (stack.PROC / '42/comm').unlink()
        env = stack.CVM_CONFIG_DIR / 'env'
        stack.atomic(env, b'BROCADE_CVM_STATE_DIR=/custom/cube-state\n')
        self.envfiles = f'{env} (ignore_errors=no)'
        with self.assertRaisesRegex(stack.StackError, 'CVM configuration'):
            self.install.stack_install_inspect()

    def test_only_resource_empty_agent_bootstrap_records_are_allowed(self):
        stack.atomic(stack.AGENT_STATE / 'cvm-nics/controller.lock', b'')
        stack.atomic(stack.AGENT_STATE / 'cvm-nic-authority.sha256', b'a' * 64)
        path = stack.AGENT_STATE / 'cvm-nics-desired.json'
        intent = dict(
            node_id='node', generation=3, bindings=[], fences=[], budgets={}, management_networks=['192.0.2.8/32']
        )
        stack.atomic_json(path, intent)
        self.assertEqual(self.install.stack_install_inspect(), 'xfs')
        stack.atomic_json(path, {**intent, 'bindings': [{'id': 'stale'}]})
        with self.assertRaisesRegex(stack.StackError, 'CVM records'):
            self.install.stack_install_inspect()

    def test_empty_network_bootstrap_matches_the_agent_only_host(self):
        files = {
            'cvm-entries/rules.nft': b'add table ip brocade_cvm_entries\ndelete table ip brocade_cvm_entries\n',
            'cvm-entries/phantun.nft': b'add table inet brocade_entry_phantun\ndelete table inet brocade_entry_phantun\n',
            'cvm-entries/tcp-transports.json': b'{"ports":[],"targets":[]}',
            'cvm-entries/wireguard-ports.json': b'[]',
            'cvm-entry-flow-cleanup.json': b'{"intent":[],"pending":[],"completed":0}',
            'cvm-network-diagnostics.json': (
                b'{"collected_at":5,"dropped_events":0,"resources":[],"events":'
                b'[{"at":5,"resource_id":null,"source":"agent","level":"info","message":"ready"}]}'
            ),
            'cvm-nic-identity.json': json.dumps(
                {
                    'certificate': '-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----\n',
                    'key': '-----BEGIN PRIVATE KEY-----\nfixture\n-----END PRIVATE KEY-----\n',
                }
            ).encode(),
        }
        for name, data in files.items():
            stack.atomic(stack.AGENT_STATE / name, data)
        self.assertEqual(self.install.stack_install_inspect(), 'xfs')
        changes = {
            'cvm-entries/rules.nft': files['cvm-entries/rules.nft'] + b'add table ip unexpected\n',
            'cvm-entries/wireguard-ports.json': b'[51820]',
            'cvm-entries/tcp-transports.json': b'{"ports":[],"targets":["192.0.2.1"]}',
            'cvm-entries/tunnel.json': b'{}',
            'cvm-entry-flow-cleanup.json': b'{"intent":[],"pending":[{}],"completed":0}',
            'cvm-network-diagnostics.json': files['cvm-network-diagnostics.json'].replace(b'null', b'"tunnel"'),
            'cvm-nic-identity.json': b'{"certificate":"unrecognized","key":"unrecognized"}',
        }
        for name, data in changes.items():
            with self.subTest(name=name):
                path = stack.AGENT_STATE / name
                stack.atomic(path, data)
                with self.assertRaisesRegex(stack.StackError, 'CVM records'):
                    self.install.stack_install_inspect()
                if name in files:
                    stack.atomic(path, files[name])
                else:
                    path.unlink()
        self.assertEqual(self.install.stack_install_inspect(), 'xfs')

    def test_positive_stage_and_authorized_activation_are_required_before_mutation(self):
        bad_target = {**self.target, 'shim_sha256': '0' * 64}
        with self.assertRaisesRegex(stack.StackError, 'approved artifacts'):
            self.install.stack_install_stage(self.archive, bad_target)
        self.assertFalse(self.release.exists())
        self.stage()
        with self.assertRaisesRegex(stack.StackError, 'marker is required'):
            self.apply()
        for fields in ({'expires_at': 3600}, {'target_bytes': True}, {'target_bytes': 0}):
            with self.subTest(fields=fields):
                self.claim(**fields)
                with self.assertRaisesRegex(stack.StackError, 'authorized Agent job'):
                    self.apply()
        self.claim()
        with self.assertRaisesRegex(stack.StackError, 'canonical empty roots'):
            self.install.stack_install_activate(self.target, '0' * 64)
        with self.assertRaisesRegex(stack.StackError, 'authorized Agent job'):
            self.install.stack_install_activate(bad_target, self.roots)
        with self.assertRaisesRegex(stack.StackError, 'no matching interrupted'):
            self.apply(resume=True)
        self.assertFalse(self.checkpoint.exists())
        self.assertTrue(self.agent_active)
        self.activate.assert_not_called()
        self.apply()
        self.activate.assert_called_once_with(self.identity, install_job=21, restore=False)
        self.cold.assert_called_once_with(self.release / 'generated/cube.json', self.roots, read_only=True)
        self.assertEqual(self.install.stack_inspect(), self.target)
        self.assertFalse(self.checkpoint.exists())
        self.assertTrue(self.marker.exists())
        self.assertIsNone(self.install.state()['previous'])
        self.apply(resume=True)  # A lost response can prove the same committed target.
        self.assertEqual(self.activate.call_count, 1)

    def test_resume_requires_matching_job_and_refuses_unknown_partial_data(self):
        self.stage()
        self.claim()
        self.activate.side_effect = stack.StackError('interrupted before rendering')
        with self.assertRaisesRegex(stack.StackError, 'before rendering'):
            self.apply()
        self.assertEqual(self.install.state()['pending'], self.identity)
        self.assertTrue(self.checkpoint.exists())
        with self.assertRaisesRegex(stack.StackError, 'stack-install-resume'):
            self.install.stack_inspect()
        with self.assertRaisesRegex(stack.StackError, 'stack-install-resume'):
            self.install.stack_install_inspect()
        self.claim(id=22, phase='recovering')
        with self.assertRaisesRegex(stack.StackError, 'recorded fresh installation'):
            self.apply(resume=True)
        self.claim(phase='recovering')
        stale_path = stack.STATE / 'state/unknown.db'
        stack.atomic(stale_path, b'preserve this')
        with self.assertRaisesRegex(stack.StackError, 'existing CVM contents'):
            self.apply(resume=True)
        self.assertEqual(stale_path.read_bytes(), b'preserve this')
        self.assertEqual(self.activate.call_count, 1)
        stale_path.unlink()
        self.activate.side_effect = self.publish
        self.apply(resume=True)
        self.assertEqual(self.activate.call_count, 2)
        self.assertFalse(self.checkpoint.exists())

    def test_resume_reuses_only_verified_rendered_target(self):
        self.stage()
        self.claim()

        def interrupt_before_start(*args, **kwargs):
            self.publish(*args, **kwargs)
            self.running = False
            self.agent_active = False
            raise stack.StackError('interrupted before service start')

        self.activate.side_effect = interrupt_before_start
        with self.assertRaisesRegex(stack.StackError, 'before service start'):
            self.apply()
        pending_shim = stack.STATE / 'root/component_versions/cube-shim/target/bin' / Path(stack.SHIM).name
        packaged = pending_shim.read_bytes()
        pending_shim.write_bytes(b'foreign bytes')
        self.claim(phase='recovering')
        with self.assertRaisesRegex(stack.StackError, 'Shim version conflicts'):
            self.apply(resume=True)
        self.assertEqual(pending_shim.read_bytes(), b'foreign bytes')
        pending_shim.write_bytes(packaged)
        generated = self.release / 'generated/network.json'
        original = generated.read_bytes()
        generated.write_bytes(original + b'\n')
        with self.assertRaisesRegex(stack.StackError, 'configuration differs from the recorded first installation'):
            self.apply(resume=True)
        self.assertEqual(generated.read_bytes(), original + b'\n')
        generated.write_bytes(original)
        self.activate.side_effect = self.publish
        self.apply(resume=True)
        self.activate.assert_called_with(self.identity, install_job=21, restore=True)
        self.assertFalse(self.checkpoint.exists())

    def test_resume_proves_live_pending_target_without_restarting_services(self):
        self.stage()
        self.claim()

        def interrupt_after_publication(*args, **kwargs):
            self.publish(*args, **kwargs)
            raise stack.StackError('interrupted after service restart')

        self.activate.side_effect = interrupt_after_publication
        with self.assertRaisesRegex(stack.StackError, 'after service restart'):
            self.apply()
        self.claim(phase='recovering')
        self.cold.side_effect = [stack.StackError('could not prove empty roots'), None]
        with self.assertRaisesRegex(stack.StackError, 'empty roots'):
            self.apply(resume=True)
        self.assertEqual(self.install.state()['pending'], self.identity)
        self.assertTrue(self.checkpoint.exists())
        self.apply(resume=True)
        self.assertEqual(self.install.state()['current'], self.identity)
        self.assertFalse(self.checkpoint.exists())
        self.assertEqual(self.activate.call_count, 1)


original_running_cubelet = stack.running_cubelet
original_cold_evidence = stack.cold_evidence


if __name__ == '__main__':
    unittest.main()
