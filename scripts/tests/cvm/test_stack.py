"""Archive trust and recoverable activation tests; never touch host services or disks."""
from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(SCRIPTS))
import cvm_stack as stack
import cvm_host


class StackTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.addCleanup(self.temporary.cleanup)
        state = patch.object(stack, 'STATE', self.root / 'persistent')
        state.start()
        self.addCleanup(state.stop)
        self.detect_backend = stack.local_storage_backend
        backend = patch.object(stack, 'local_storage_backend', return_value='xfs')
        self.backend = backend.start()
        self.addCleanup(backend.stop)
        self.detect_uuid = stack.local_storage_uuid
        filesystem = patch.object(stack, 'local_storage_uuid', return_value='aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa')
        self.filesystem = filesystem.start()
        self.addCleanup(filesystem.stop)

    def tree(self, name='tree', variant='one'):
        tree = self.root / name
        for name in stack.REQUIRED:
            path = tree / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name + variant)
            path.chmod(0o755 if '/bin/' in name else 0o644)
        return tree

    def bundle(self, tree, name='bundle.tar.gz'):
        archive = self.root / name
        with redirect_stdout(io.StringIO()):
            stack.pack(tree, archive, 'x86_64', 'a' * 64, '2.38')
        return archive, stack.digest(archive)

    def stage(self, archive, identity, installation):
        installation.base.mkdir(parents=True, exist_ok=True, mode=0o700)
        release = installation.release(identity)
        release.mkdir(parents=True, mode=0o700)
        stack.inspect_archive(archive, identity, release)
        stack.atomic_json(release / 'generated/cube.json', {'storage_backend': 'xfs'})
        return release

    def installation(self):
        return stack.Installation(self.root / 'managed', self.root / 'toolbox', self.root / 'units')

    def test_reproducible_archive_and_exact_member_digests(self):
        tree = self.tree()
        first, sha = self.bundle(tree)
        second, other = self.bundle(tree, 'second.tar.gz')
        self.assertEqual(sha, other)
        destination = self.root / 'unpacked'
        destination.mkdir()
        manifest = stack.inspect_archive(first, sha, destination)
        self.assertEqual(set(manifest['files']), set(stack.REQUIRED))
        for name in stack.REQUIRED:
            self.assertEqual((tree / name).read_bytes(), (destination / name).read_bytes())

    def test_new_host_boot_removes_only_dead_transports_and_preserves_databases(self):
        metadata = stack.STATE / 'state'; metadata.mkdir(parents=True)
        boot = self.root / 'boot-id'; boot.write_text('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa')
        runtime = metadata / 'io.containerd.runtime.v2.task'
        database = metadata / 'io.cubelet.internal.v1.cubebox/db/meta.db'
        database.parent.mkdir(parents=True); database.write_bytes(b'original metadata')
        with patch.object(stack, 'HOST_BOOT_ID', boot), patch.object(stack, 'run', return_value=''):
            stack.prepare_boot_runtime_state()
            runtime.mkdir(mode=0o700); (runtime / 'shim-address').write_text('old socket')
            stack.prepare_boot_runtime_state()
            self.assertTrue((runtime / 'shim-address').exists(), 'daemon upgrades must preserve live transports')
            boot.write_text('bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb')
            stack.prepare_boot_runtime_state()
            self.assertFalse(runtime.exists())
            self.assertEqual(database.read_bytes(), b'original metadata')

    def test_unknown_or_still_mounted_runtime_state_is_never_removed(self):
        metadata = stack.STATE / 'state'; metadata.mkdir(parents=True)
        boot = self.root / 'boot-id'; boot.write_text('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa')
        runtime = metadata / 'io.containerd.runtime.v2.task'; runtime.mkdir(mode=0o700)
        (runtime / 'owned-data').write_text('keep')
        with patch.object(stack, 'HOST_BOOT_ID', boot):
            with self.assertRaises(stack.StackError): stack.prepare_boot_runtime_state()
            stack.atomic_json(metadata / '.brocade-runtime-boot.json', {'host_boot_id':'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb'})
            with patch.object(stack, 'run', return_value=str(runtime / 'default/guest/rootfs')):
                with self.assertRaises(stack.StackError): stack.prepare_boot_runtime_state()
        self.assertEqual((runtime / 'owned-data').read_text(), 'keep')

    def test_release_receipt_skips_unchanged_images_but_rechecks_same_size_rewrites(self):
        import os
        tree = self.tree(); archive, identity = self.bundle(tree)
        release = self.stage(archive, identity, self.installation())
        original = stack.digest
        with patch.object(stack, 'digest', wraps=original) as hashes:
            stack.verify_release(release)
            first = hashes.call_count
            self.assertGreater(first, len(stack.REQUIRED))
            stack.verify_release(release)
            self.assertEqual(hashes.call_count, first + 1, 'restart should only hash the small manifest')
        image = release / 'runtime/cube-agent/cube-agent.ext4'
        before = image.stat(); content = image.read_bytes()
        image.write_bytes(b'X' * len(content))
        os.utime(image, ns=(before.st_atime_ns, before.st_mtime_ns))
        with self.assertRaises(stack.StackError): stack.verify_release(release)
        image.write_bytes(content)
        stack.verify_release(release)
        receipt = release / '.verified-files-v1.json'
        receipt.chmod(0o666)
        with self.assertRaises(stack.StackError): stack.verify_release(release)
        receipt.unlink(); receipt.symlink_to(release / 'manifest.json')
        with self.assertRaises(stack.StackError): stack.verify_release(release)

    def test_bad_digest_or_writable_archive_is_refused(self):
        archive, sha = self.bundle(self.tree())
        with self.assertRaises(stack.StackError):
            stack.inspect_archive(archive, 'b' * 64)
        archive.chmod(0o666)
        with self.assertRaises(stack.StackError):
            stack.inspect_archive(archive, sha)

    def test_cubelet_start_refuses_volatile_metadata_before_launching_a_process(self):
        with patch.object(stack, 'run', return_value='tmpfs'), patch.object(stack.subprocess, 'Popen') as process:
            with self.assertRaises(stack.StackError):
                stack.start_cubelet(self.root / 'release')
        process.assert_not_called()

    def test_symlink_and_unlisted_or_duplicate_members_are_refused(self):
        original, _ = self.bundle(self.tree())
        for scenario in ('link', 'traversal', 'extra', 'duplicate'):
            archive = self.root / (scenario + '.tar.gz')
            with tarfile.open(original, 'r:gz') as source, tarfile.open(archive, 'w:gz') as output:
                for member in source:
                    output.addfile(member, source.extractfile(member))
                member = tarfile.TarInfo({'traversal': '../outside', 'duplicate': stack.REQUIRED[0]}.get(scenario, 'runtime/unlisted'))
                if scenario == 'link':
                    member.type, member.linkname = tarfile.SYMTYPE, '/etc/shadow'
                    output.addfile(member)
                else:
                    member.size = 4
                    output.addfile(member, io.BytesIO(b'evil'))
            archive.chmod(0o600)
            with self.subTest(scenario=scenario), self.assertRaises(stack.StackError):
                stack.inspect_archive(archive, stack.digest(archive))
            self.assertFalse((self.root / 'outside').exists())

    def test_pack_requires_every_runtime_component_and_rejects_symlinks(self):
        tree = self.tree()
        (tree / stack.REQUIRED[0]).unlink()
        with self.assertRaises(stack.StackError):
            self.bundle(tree)
        (tree / stack.REQUIRED[0]).symlink_to('/bin/sh')
        with self.assertRaises(stack.StackError):
            self.bundle(tree)

    def test_existing_foreign_cube_installation_is_preserved(self):
        install = self.installation()
        install.toolbox.mkdir()
        marker = install.toolbox / 'user-data'
        marker.write_text('keep')
        with self.assertRaises(stack.StackError):
            install.check_ownership({})
        self.assertEqual(marker.read_text(), 'keep')

    def test_activation_requires_matching_rust_agent_before_any_service_change(self):
        install = self.installation()
        with patch.object(stack, 'run', return_value='old-agent'), patch.object(stack, 'verify_release') as verify:
            with self.assertRaisesRegex(stack.StackError, 'upgrade Brocade Agent'):
                install.activate('a' * 64)
        verify.assert_not_called()
        self.assertFalse(install.units.exists())

    def test_activation_preserves_foreign_agent_dropin(self):
        install = self.installation()
        dropin = install.units / 'brocade-agent.service.d/brocade-cvm.conf'
        dropin.parent.mkdir(parents=True)
        dropin.write_text('administrator configuration\n')
        with patch.object(stack, 'run', side_effect=['brocade-cube-rust-v1','cubevs-attachments-v2']), patch.object(stack, 'verify_release') as verify:
            with self.assertRaisesRegex(stack.StackError, 'not installer-owned'):
                install.activate('a' * 64)
        verify.assert_not_called()
        self.assertEqual(dropin.read_text(), 'administrator configuration\n')


    def test_readiness_requires_actual_rust_agent_capabilities(self):
        config = self.root / 'config.json'
        from subprocess import CompletedProcess
        response = CompletedProcess([], 0, json.dumps({'executor': 'brocade-cube-rust-v1', 'capabilities': {'create': True, 'stable_storage': True}}).encode(), b'')
        with patch.object(stack.subprocess, 'run', return_value=response) as run:
            stack.ready(timeout=1, config=config)
        self.assertEqual(run.call_args.args[0], [str(stack.AGENT_BIN), 'cvm-check', '--config', str(config)])
        self.assertGreater(run.call_args.kwargs['timeout'], 0)
        response.stdout = json.dumps({'executor': 'legacy', 'capabilities': {'create': True, 'stable_storage': True}}).encode()
        with patch.object(stack.subprocess, 'run', return_value=response), self.assertRaises(stack.StackError):
            stack.ready(timeout=.01, config=config)

    def test_unmanaged_persistent_cube_state_cannot_be_implicitly_adopted(self):
        (stack.STATE / 'root').mkdir(parents=True)
        marker = stack.STATE / 'root/important-state'
        marker.write_text('keep')
        with self.assertRaises(stack.StackError):
            self.installation().check_ownership({})
        self.assertEqual(marker.read_text(), 'keep')

    def test_upgrade_failure_restores_old_release_and_keeps_new_evidence(self):
        archive, old = self.bundle(self.tree())
        candidate_archive, candidate = self.bundle(self.tree('new', 'two'), 'new.tar.gz')
        install = self.installation()
        self.stage(archive, old, install)
        self.stage(candidate_archive, candidate, install)
        stack.atomic_json(install.state_path, {'current': old, 'previous': None})
        stack.point(install.toolbox, install.release(old) / 'runtime')
        calls = []
        def activate(identity):
            calls.append(identity)
            stack.point(install.toolbox, install.release(identity) / 'runtime')
            if identity == candidate:
                raise stack.StackError('injected readiness failure')
        with patch.object(install, 'activate', side_effect=activate), self.assertRaises(stack.StackError):
            install.change(candidate, 'install')
        self.assertEqual(calls, [candidate, old])
        self.assertEqual(install.state()['current'], old)
        self.assertNotIn('pending', install.state())
        self.assertTrue(install.release(candidate).is_dir())

    def test_interrupted_upgrade_is_recovered_by_the_frozen_previous_identity(self):
        archive, old = self.bundle(self.tree())
        install = self.installation()
        self.stage(archive, old, install)
        stack.atomic_json(install.state_path, {'current': old, 'pending': 'b' * 64})
        with patch.object(install, 'activate') as activate:
            install.recover()
        activate.assert_called_once_with(old)
        self.assertEqual(install.state()['current'], old)
        self.assertNotIn('pending', install.state())

    def test_initial_failed_install_can_retry_the_same_bundle_identity(self):
        archive, identity = self.bundle(self.tree())
        install = self.installation()
        self.stage(archive, identity, install)
        stack.atomic_json(install.state_path, {'pending': identity})
        stack.point(install.toolbox, install.release(identity) / 'runtime')
        with patch.object(install, 'activate'):
            install.change(identity, 'install')
        self.assertEqual(install.state()['current'], identity)
        self.assertNotIn('pending', install.state())

    def test_corrupt_retained_binary_cannot_be_reactivated(self):
        archive, identity = self.bundle(self.tree())
        install = self.installation()
        release = self.stage(archive, identity, install)
        (release / stack.REQUIRED[0]).write_text('changed')
        with self.assertRaises(stack.StackError), patch.object(install, 'activate') as activate:
            install.change(identity, 'rollback')
        activate.assert_not_called()

    def test_mode_retry_preserves_the_previous_runtime_instead_of_pointing_rollback_to_itself(self):
        archive, identity = self.bundle(self.tree())
        install = self.installation()
        self.stage(archive, identity, install)
        stack.atomic_json(install.state_path, {'current': identity, 'previous': 'b'*64, 'pending': identity})
        with patch.object(install, 'activate'):
            install.change(identity, 'virtualization', force=True)
        self.assertEqual(install.state()['previous'], 'b'*64)
        self.assertNotIn('pending', install.state())

    def configured_tree(self, name, variant):
        tree = self.tree(name, variant)
        versions = {}
        for component in ('cube-shim', 'cube-kernel-scf', 'cube-image', 'cube-agent'):
            version = variant + '-' + component
            (tree / 'runtime' / component / 'version').write_text(version + '\n')
            versions[component] = version
        config = tree / 'config'
        (config / 'system.json').write_text(json.dumps({'system': variant}))
        capabilities = json.loads((SCRIPTS.parent / 'crates/brocade-cube/examples/cvm-cube.example.json').read_text())['capabilities']
        capabilities['images'] = [{'id': variant, 'sha256': '0' * 64}]
        (config / 'cube.json').write_text(json.dumps({
            'cubelet_socket': '/run/cubelet/cubelet.sock',
            'image_files': {variant: '@CVM_RELEASE@/generated/system.json'},
            'capabilities': capabilities}))
        (config / 'network.json').write_text(json.dumps({'shim_version': versions['cube-shim'],
            'shim_sha256': stack.digest(tree / 'runtime/cube-shim/bin/containerd-shim-cube-rs'),
            'max_interfaces': 1, 'bridge_sources': {}}))
        (config / 'cubelet.toml').write_text(f'root = "{stack.STATE / "root"}"\nstate = "{stack.STATE / "state"}"\n'
            '[grpc]\naddress = "/run/cubelet/cubelet.sock"\ntcp_address = ""\n'
            '[http]\naddress = "127.0.0.1:8080"\n[debug]\naddress = "127.0.0.1:9966"\n'
            '[plugins."io.cubelet.internal.v1.images"]\n'
            '[plugins."io.cubelet.internal.v1.storage"]\nstorage_backend = "cubecow"\n')
        stack.atomic_json(tree / 'manifest.json', {'storage_backends': ['xfs', 'plain']})
        return tree

    def test_one_gib_host_reserves_runtime_memory_and_keeps_cpu_capacity(self):
        gib = 1024**3
        with patch.object(stack, 'STATE', self.root / 'persistent'), patch.object(stack, 'run', return_value='[{"dev":"eth0"}]'), \
                patch.object(stack.os, 'sysconf', side_effect=lambda name: gib // 4096 if name == 'SC_PHYS_PAGES' else 4096), \
                patch.object(stack.os, 'cpu_count', return_value=2):
            (stack.STATE / 'storage').mkdir(parents=True)
            tree = self.configured_tree('full-capacity', 'one')
            stack.render_configuration(tree)
            caps = stack.read_json(tree / 'generated/cube.json')['capabilities']
            self.assertEqual(caps['allocatable_vcpus'], 2)
            self.assertEqual(caps['allocatable_memory_bytes'], 640*1024**2)
            self.assertEqual(caps['resource_limits']['max_memory_bytes'], 640*1024**2)
            self.assertEqual(caps['allocatable_disk_bytes'], stack.shutil.disk_usage(stack.STATE / 'storage').free // gib * gib)

    def test_managed_resource_upgrade_preserves_network_storage_and_custom_sections(self):
        reference = (SCRIPTS.parent / 'components/cube/Cubelet/config/config.toml').read_text()
        old = reference.replace('pool_size = 1', 'pool_size = 3000').replace('tap_init_num = 1', 'tap_init_num = 500')
        old = old.replace('concurrent = 1', 'concurrent = 100').replace('eth_name = "eth0"', 'eth_name = "ens9"')
        old += '\n[operator]\nkeep = "unchanged"\n'
        upgraded = stack.apply_resource_defaults(old, reference)
        parsed = stack.tomllib.loads(upgraded)
        self.assertEqual(parsed['plugins']['io.cubelet.internal.v1.cgroup']['pool_size'], 1)
        self.assertEqual(parsed['plugins']['io.cubelet.internal.v1.network']['tap_init_num'], 1)
        self.assertEqual(parsed['plugins']['io.cubelet.internal.v1.network']['eth_name'], 'ens9')
        self.assertEqual(parsed['plugins']['io.cubelet.workflow.v1.workflow']['flows']['create']['concurrent'], 1)
        self.assertEqual(parsed['operator']['keep'], 'unchanged')
        self.assertEqual(stack.apply_resource_defaults(upgraded, reference), upgraded)

    def test_upgrade_clamps_retained_capacity_to_one_gib_host_budget(self):
        gib = 1024**3
        with patch.object(stack, 'run', return_value='[{"dev":"eth0"}]'), \
                patch.object(stack.os, 'sysconf', side_effect=lambda name: gib // 4096 if name == 'SC_PHYS_PAGES' else 4096):
            (stack.STATE / 'storage').mkdir(parents=True)
            old, new = self.configured_tree('large-budget', 'old'), self.configured_tree('small-budget', 'new')
            stack.render_configuration(old)
            cfg = stack.read_json(old / 'generated/cube.json')
            cfg['capabilities']['allocatable_memory_bytes'] = gib
            cfg['capabilities']['resource_limits']['max_memory_bytes'] = gib
            stack.atomic_json(old / 'generated/cube.json', cfg)
            stack.render_configuration(new, old)
            caps = stack.read_json(new / 'generated/cube.json')['capabilities']
            self.assertEqual(caps['allocatable_memory_bytes'], 640*1024**2)
            self.assertEqual(caps['resource_limits']['max_memory_bytes'], 640*1024**2)

    def test_upgrade_retains_images_and_replaces_the_local_network_catalogue(self):
        with patch.object(stack, 'STATE', self.root / 'persistent'), patch.object(stack, 'run', return_value='[{"dev":"eth0"}]'):
            (stack.STATE / 'storage').mkdir(parents=True)
            old, new = self.configured_tree('old', 'old'), self.configured_tree('next', 'new')
            stack.render_configuration(old)
            configured = stack.read_json(old / 'generated/network.json')
            configured['bridge_sources'] = {'home': {'bridge': 'br-home'}}
            stack.atomic_json(old / 'generated/network.json', configured)
            old_config = stack.read_json(old / 'generated/cube.json')
            old_config['capabilities']['unknown_extension'] = True
            stack.atomic_json(old / 'generated/cube.json', old_config)
            stack.render_configuration(new, old)
            new_cfg = stack.read_json(new / 'generated/cube.json')
            self.assertNotIn('unknown_extension', new_cfg['capabilities'])
            self.assertTrue(new_cfg['capabilities']['stable_storage'])
            self.assertEqual(set(new_cfg['image_files']), {'old', 'new'})
            self.assertNotIn('bridge_sources', stack.read_json(new / 'generated/network.json'))
            self.assertEqual(stack.read_json(new / 'generated/network.json')['state_dir'], '/data/cubelet/network-attachments-v2')
            stack.render_configuration(old, new)
            self.assertEqual(set(stack.read_json(old / 'generated/cube.json')['image_files']), {'old', 'new'})
            # Rendered host paths do not rewrite or invalidate the immutable payload.
            self.assertIn('@CVM_RELEASE@', (old / 'config/cube.json').read_text())

    def test_plain_selection_capacity_and_upgrade_identity(self):
        self.backend.return_value = 'plain'
        with patch.object(stack, 'run', return_value='[{"dev":"eth0"}]'), patch.object(stack.shutil, 'disk_usage', return_value=stack.shutil._ntuple_diskusage(100*1024**3, 80*1024**3, 20*1024**3)):
            old, new = self.configured_tree('old-plain', 'old'), self.configured_tree('new-plain', 'new')
            stack.render_configuration(old)
            cfg = stack.read_json(old / 'generated/cube.json')
            self.assertEqual(cfg['storage_backend'], 'plain')
            self.assertEqual(cfg['capabilities']['allocatable_disk_bytes'], 20*1024**3)
            import tomllib
            storage = tomllib.loads((old / 'generated/cubelet.toml').read_text())['plugins']['io.cubelet.internal.v1.storage']
            self.assertEqual(storage['local_backend'], 'plain')
            self.assertEqual(storage['plain_reserve_bytes'], 0)
            stack.render_configuration(new, old)
            self.assertEqual(stack.read_json(new / 'generated/cube.json')['capabilities']['allocatable_disk_bytes'], 20*1024**3)
            self.filesystem.return_value = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb'
            with self.assertRaises(stack.StackError): stack.render_configuration(old, new)
            with patch.object(stack.subprocess, 'Popen') as process, self.assertRaises(stack.StackError):
                stack.start_cubelet(new)
            process.assert_not_called()
            self.backend.return_value = 'xfs'
            with patch.object(stack.subprocess, 'Popen') as process, self.assertRaises(stack.StackError):
                stack.start_cubelet(new)
            process.assert_not_called()
            with self.assertRaises(stack.StackError): stack.render_configuration(old, new)

    def test_plain_rollback_to_xfs_only_release_is_rejected_before_activation(self):
        archive, identity = self.bundle(self.tree())
        install = self.installation()
        self.stage(archive, identity, install)
        self.backend.return_value = 'plain'
        with patch.object(install, 'activate') as activate, self.assertRaises(stack.StackError):
            install.change(identity, 'rollback')
        activate.assert_not_called()
        self.assertNotIn('pending', install.state())

    def test_backend_detection_and_malformed_inventory(self):
        for filesystem, expected in [('ext4', 'plain'), ('xfs', 'xfs')]:
            with patch.object(stack, 'run', return_value=filesystem):
                self.assertEqual(self.detect_backend(), expected)
        with patch.object(stack, 'run', return_value='btrfs'), self.assertRaises(stack.StackError):
            self.detect_backend()
        with patch.object(stack, 'run', return_value='AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA'):
            self.assertEqual(self.detect_uuid(), 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa')
        with patch.object(stack, 'run', return_value=''), self.assertRaises(stack.StackError):
            self.detect_uuid()
        stack.require_storage_support({'storage_backends': ['xfs', 'plain']}, 'plain')
        with self.assertRaises(stack.StackError): stack.require_storage_support({}, 'plain')
        archive, sha = self.bundle(self.tree())
        manifest = stack.inspect_archive(archive, sha)
        for inventory in [[{}], ['plain', 'plain'], []]:
            with self.assertRaises(stack.StackError): stack.validate_manifest(dict(manifest, storage_backends=inventory))

    def test_pvm_archive_binds_both_kernels_and_the_exact_resume_program(self):
        tree = self.tree()
        sources = {'runtime/bin/cvm-host.py': SCRIPTS / 'cvm_host.py',
                   'runtime/bin/cvm-stack.py': SCRIPTS / 'cvm_stack.py'}
        for name, source in sources.items():
            path = tree / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(source.read_bytes())
        for name in ('runtime/pvm/host.deb', 'runtime/cube-kernel-pvm/vmlinux',
                     'runtime/cube-kernel-pvm/version', 'config/cube-pvm.json', 'config/system-pvm.json'):
            path = tree / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('fixed test asset')
        pvm = {'host_kernel_release': '6.6.117-opencloudos9.cubesandbox.pvm.host',
               'host_package': 'runtime/pvm/host.deb', 'guest_kernel': 'runtime/cube-kernel-pvm/vmlinux',
               'boot_args': ['nopti', 'pti=off']}
        archive = self.root / 'pvm.tar.gz'
        with redirect_stdout(io.StringIO()):
            stack.pack(tree, archive, 'x86_64', 'a'*64, '2.35', pvm=pvm)
        install = self.installation()
        release = self.stage(archive, stack.digest(archive), install)
        manager = stack.host_manager(install, release)
        self.assertEqual(manager.pvm, pvm)
        (release / 'runtime/bin/cvm-host.py').write_text('untrusted code')
        with self.assertRaisesRegex(stack.StackError, 'immutable inventory'):
            stack.host_manager(install, release)
        manifest = stack.inspect_archive(archive, stack.digest(archive))
        self.assertEqual(manifest['contract'], stack.CONTRACT)
        with self.assertRaisesRegex(stack.StackError, 'unsupported CVM'):
            stack.validate_manifest({**manifest, 'contract': 'brocade-cvm-stack-v1'})
        for mutation in ({'guest_kernel': 'runtime/cube-kernel-scf/vmlinux'},
                         {'host_package': '../outside.deb'}, {'boot_args': ['ipv6.disable=1']},
                         {'boot_args': [{}]}):
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                cvm_host.validate({**pvm, **mutation}, manifest['files'])

    def test_mode_rendering_uses_distinct_guest_images_without_rewriting_original_kernel(self):
        (stack.STATE / 'storage').mkdir(parents=True)
        tree = self.configured_tree('paired', 'native')
        cfg = stack.read_json(tree / 'config/cube.json')
        cfg['capabilities']['images'][0]['id'] = 'pvm-system'
        cfg['image_files'] = {'pvm-system': '@CVM_RELEASE@/generated/system-pvm.json'}
        stack.atomic_json(tree / 'config/cube-pvm.json', cfg)
        stack.atomic_json(tree / 'config/system-pvm.json', {'kernel': 'paired-pvm'})
        (tree / 'runtime/cube-kernel-pvm').mkdir()
        (tree / 'runtime/cube-kernel-pvm/version').write_text('pvm-version\n')
        (tree / 'runtime/cube-kernel-pvm/vmlinux').write_bytes(b'PVM Guest kernel')
        with patch.object(stack, 'run', return_value='[{"dev":"eth0"}]'):
            stack.render_configuration(tree)
            original = (stack.STATE / 'root/component_versions/cube-kernel-scf/native-cube-kernel-scf/vmlinux').read_bytes()
            stack.render_configuration(tree, tree, mode='pvm')
            current = stack.read_json(tree / 'generated/cube.json')
            self.assertEqual(current['virtualization_mode'], 'pvm')
            images = stack.tomllib.loads((tree / 'generated/cubelet.toml').read_text())['plugins']['io.cubelet.internal.v1.images']
            self.assertEqual(images['shared_kernel_path'], str(tree / 'runtime/cube-kernel-pvm/vmlinux'))
            self.assertEqual(images['image_base_path'], str(stack.STATE / 'system-images/pvm'))
            self.assertEqual([image['id'] for image in current['capabilities']['images']], ['pvm-system'])
            self.assertEqual((stack.STATE / 'root/component_versions/cube-kernel-scf/pvm-version/vmlinux').read_bytes(), b'PVM Guest kernel')
            self.assertEqual((stack.STATE / 'root/component_versions/cube-kernel-scf/native-cube-kernel-scf/vmlinux').read_bytes(), original)
            stack.render_configuration(tree, tree, mode='kvm')
            current = stack.read_json(tree / 'generated/cube.json')
            self.assertEqual([image['id'] for image in current['capabilities']['images']], ['native'])
            images = stack.tomllib.loads((tree / 'generated/cubelet.toml').read_text())['plugins']['io.cubelet.internal.v1.images']
            self.assertEqual(images['shared_kernel_path'], str(tree / 'runtime/cube-kernel-scf/vmlinux'))
            self.assertEqual(images['image_base_path'], str(stack.STATE / 'system-images'))


if __name__ == '__main__':
    unittest.main()
