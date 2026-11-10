"""Assemble the node-only CVM bundle from accepted builds and fixed Guest assets.

Called by scripts/build-release.py. This does not compile a second control plane,
fetch mutable images, or import a developer machine's running Cube installation.
"""
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

import cvm_stack as stack
from cvm_elf import validate as validate_elf


ASSETS = ('system.json', 'root.ext4', 'vmlinux', 'guest.img', 'agent.ext4')


@stack.progress_task('assemble node bundle')
def prepare(root, build_manifest, assets, output):
    spec = importlib.util.spec_from_file_location('cube_build', root / 'scripts/build-cube.py')
    builder = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(builder)
    build = stack.read_json(build_manifest)
    if build.get('result') != 'build_complete' or build.get('sourceSHA256') != builder.source_digest(root):
        raise stack.StackError('CVM runtime build is incomplete or does not match current source')
    architecture = build['architecture']
    asset_manifest = stack.read_json(assets / 'manifest.json')
    pvm = asset_manifest.get('pvm')
    if architecture == 'x86_64' and pvm is None:
        raise stack.StackError('new x86_64 CVM bundles require paired PVM host/Guest assets for automatic fallback')
    extra_assets = ('vmlinux-pvm', 'pvm-host.deb') if pvm is not None else ()
    if asset_manifest.get('architecture') != architecture or set(asset_manifest.get('files', {})) != set(ASSETS + extra_assets):
        raise stack.StackError('fixed Guest assets must match the runtime architecture and complete asset inventory')
    for name in ASSETS + extra_assets:
        source = assets / name
        if source.is_symlink() or not source.is_file() or stack.digest(source) != asset_manifest['files'][name]:
            raise stack.StackError('Guest asset SHA-256 mismatch')
    with tempfile.TemporaryDirectory(prefix='package-', dir=build_manifest.parent) as temporary:
        tree = Path(temporary)
        def copy(source, name):
            target = tree / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            target.chmod(0o755 if '/bin/' in name else 0o644)
            return target
        for script in ('cvm-stack.py', 'cvm-host.py'):
            copy(root / 'scripts' / script.replace('-', '_'), 'runtime/bin/' + script)
        for name, target in {
            'cubelet': 'runtime/Cubelet/bin/cubelet',
            'containerd-shim-cube-rs': 'runtime/cube-shim/bin/containerd-shim-cube-rs',
            'cube-runtime': 'runtime/cube-shim/bin/cube-runtime',
            'lan-inspect': 'runtime/bin/lan-inspect',
        }.items():
            source = build_manifest.parent / name
            if stack.digest(source) != build['artifacts'][name]['sha256']:
                raise stack.StackError('accepted CVM binary was changed after build')
            validate_elf(source, architecture, build['tools']['gnuLibcBaseline'])
            copy(source, target)
        for source, expected in build['nativeLibraryInputs'].items():
            source = Path(source)
            if stack.digest(source) != expected:
                raise stack.StackError('native library differs from the accepted build input')
            validate_elf(source, architecture, build['tools']['gnuLibcBaseline'])
            dynamic = subprocess.check_output(['readelf', '-d', str(source)], text=True)
            soname = re.search(r'\(SONAME\).*\[([A-Za-z0-9_.+-]+)\]', dynamic)
            if not soname:
                raise stack.StackError('native library has no safe SONAME')
            copy(source, 'runtime/lib/' + soname[1])
        components = {
            'cube-kernel-scf': ('vmlinux', 'vmlinux'),
            'cube-image': ('guest.img', 'cube-guest-image-cpu.img'),
            'cube-agent': ('agent.ext4', 'cube-agent.ext4'),
        }
        versions = {'cube-shim': build['shimVersion']}
        for component, (source, target) in components.items():
            copy(assets / source, f'runtime/{component}/{target}')
            versions[component] = 'sha256-' + stack.digest(assets / source)[:12]
        for component, version in versions.items():
            (tree / 'runtime' / component / 'version').write_text(version + '\n')
        (tree / 'runtime/cube-shim/conf').mkdir()
        (tree / 'runtime/cube-shim/conf/config-cube.toml').write_text('# Brocade managed runtime; boot assets are pinned per image.\n')
        root_sha = stack.digest(assets / 'root.ext4')
        root_id = 'cvm-root-' + root_sha[:32]
        copy(assets / 'root.ext4', f'runtime/cubebox_os_image/{root_id}/{root_id}.ext4')
        system = stack.read_json(assets / 'system.json')
        containers = system.get('containers', [])
        if len(containers) != 1 or containers[0].get('resources'):
            raise stack.StackError('system manifest must describe one system without CPU or memory capacity')
        for volume in system.get('volumes', []):
            empty = volume.get('volume_source', {}).get('empty_dir', {})
            if any(empty.get(key) for key in ('SizeLimit', 'size_limit', 'sizeLimit')):
                raise stack.StackError('system manifest must not bind system disk capacity')
        for owner in [system, containers[0], containers[0].get('image', {})]:
            annotations = owner.get('annotations', {})
            for key in list(annotations):
                if key.startswith('cube.master.rootfs.artifact.'):
                    del annotations[key]
                    continue
                if any(part in key.lower() for part in ('appsnapshot', 'token', 'password', 'secret', 'credential', 'username')):
                    if annotations[key]:
                        raise stack.StackError('system manifest contains a snapshot binding or credential')
                    del annotations[key]
        image = containers[0]['image']
        image['image'], image['storage_media'] = root_id, 'ext4'
        annotations = image.setdefault('annotations', {})
        for key in list(annotations):
            if key.startswith('cube.master.rootfs.artifact.'):
                del annotations[key]
        annotations.update({'cube.master.rootfs.artifact.sha256': root_sha,
                            'cube.master.rootfs.artifact.id': root_id})
        annotations = system.setdefault('annotations', {})
        for component, version in versions.items():
            key = f'cube.master.components.{component}.version'
            if component == 'cube-shim':
                annotations.pop(key, None)
            else:
                annotations[key] = version
        system_id = 'system-' + stack.digest(assets / 'system.json')[:20]
        config = tree / 'config'
        config.mkdir()
        (config / 'system.json').write_text(json.dumps(system, sort_keys=True) + '\n')
        source = (root / 'components/cube/Cubelet/config/config.toml').read_text()
        for before, after in [
            ('address = ":9998"', 'address = "127.0.0.1:8080"'),
            ('address = "/data/cubelet/cubelet.sock"', 'address = "/run/cubelet/cubelet.sock"'),
            ('tcp_address = ":9999"', 'tcp_address = ""'),
            ('address = ":9966"', 'address = "127.0.0.1:9966"'),
            ('eth_name = "eth0"', 'eth_name = "@CVM_UPLINK@"'),
            ('cube_egress_admin_url = "http://127.0.0.1:9091"', 'cube_egress_admin_url = ""'),
            ('vm_snapshot_specs_config = "/usr/local/services/cubetoolbox/cube-snapshot/spec.json"', 'vm_snapshot_specs_config = ""'),
            ('no_sync = true', 'no_sync = false'),
        ]:
            if source.count(before) != 1:
                raise stack.StackError('Cubelet config template changed; review the node packaging adaptation')
            source = source.replace(before, after)
        source = source.replace('[plugins."io.cubelet.internal.v1.images"]',
            '[plugins."io.cubelet.internal.v1.images"]\n    image_base_path = "/data/cubelet/system-images"')
        (config / 'cubelet.toml').write_text(source)
        (config / 'dynamic.yaml').write_bytes((root / 'components/cube/Cubelet/dynamicconf/conf.yaml').read_bytes())
        capabilities = dict(create=True, delete=True, start=True, stop=True,
            restart=True, reinstall=True, configure=True, stable_storage=True, retain_on_delete=False,
            max_interfaces=3, hot_attach=True, hot_detach=True,
            allocatable_vcpus=0, allocatable_memory_bytes=0, allocatable_disk_bytes=0,
            resource_limits=None,
            images=[dict(id=system_id, name=asset_manifest['name'], version=asset_manifest['version'],
                architecture=architecture, boot_mode='cube-oci', sha256='0'*64)],
            network_sources=[dict(id='cube-contact', name='主网卡网络', kind='public', description='公网访问需授权端口映射')])
        (config / 'cube.json').write_text(json.dumps(dict(storage_backend='xfs',
            cubelet_socket='/run/cubelet/cubelet.sock', network_inspection_url='http://127.0.0.1:8080/v1/network/taps',
            capabilities=capabilities, image_files={system_id: '@CVM_RELEASE@/generated/system.json'}), sort_keys=True) + '\n')
        if pvm is not None:
            if architecture != 'x86_64' or not isinstance(pvm, dict) or set(pvm) != {'host_kernel_release', 'boot_args'}:
                raise stack.StackError('PVM assets require an explicit x86_64 host kernel release and boot arguments')
            copy(assets / 'pvm-host.deb', 'runtime/pvm/host.deb')
            copy(assets / 'vmlinux-pvm', 'runtime/cube-kernel-pvm/vmlinux')
            pvm_version = 'sha256-' + stack.digest(assets / 'vmlinux-pvm')[:12]
            (tree / 'runtime/cube-kernel-pvm/version').write_text(pvm_version + '\n')
            pvm_system = json.loads(json.dumps(system))
            pvm_system['annotations']['cube.master.components.cube-kernel-scf.version'] = pvm_version
            (config / 'system-pvm.json').write_text(json.dumps(pvm_system, sort_keys=True) + '\n')
            pvm_cfg = stack.read_json(config / 'cube.json')
            pvm_id = system_id + '-pvm-' + stack.digest(assets / 'vmlinux-pvm')[:12]
            pvm_cfg['capabilities']['images'][0]['id'] = pvm_id
            pvm_cfg['image_files'] = {pvm_id: '@CVM_RELEASE@/generated/system-pvm.json'}
            (config / 'cube-pvm.json').write_text(json.dumps(pvm_cfg, sort_keys=True) + '\n')
            pvm = {**pvm, 'host_package': 'runtime/pvm/host.deb', 'guest_kernel': 'runtime/cube-kernel-pvm/vmlinux'}
        (config / 'network.json').write_text(json.dumps(dict(state_dir='/data/cubelet/network-attachments-v2',
            vmm_root='/run/vc/vm', max_interfaces=3, shim_version=build['shimVersion'],
            shim_sha256=build['artifacts']['containerd-shim-cube-rs']['sha256']), sort_keys=True) + '\n')
        if builder.source_digest(root) != build['sourceSHA256']:
            raise stack.StackError('CVM source changed while assembling the node bundle')
        stack.pack(tree, output, architecture, build['sourceSHA256'], build['tools']['gnuLibcBaseline'],
            {'guest_assets_manifest_sha256': stack.digest(assets / 'manifest.json'),
             'package_recipe_sha256': stack.digest(root / 'scripts/cvm_package.py'),
             'installer_sha256': stack.digest(root / 'scripts/cvm_stack.py'),
             'runtime_build_manifest_sha256': stack.digest(build_manifest),
             'host_transition_sha256': stack.digest(root / 'scripts/cvm_host.py'),
             'shim_version': build['shimVersion']}, storage_backends=['xfs', 'plain'], pvm=pvm)
