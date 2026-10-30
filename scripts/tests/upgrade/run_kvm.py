#!/usr/bin/env python3
"""Run Console upgrade/restore drills with real systemd in a fresh KVM overlay."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import time
import tomllib
import uuid

V031_SHA256 = '9f92762df9486ec7103b7016a4d0f107fc09e60a5628b18a3d405f5d64226837'


def run(*args, timeout=60):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT, timeout=timeout).strip()


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--console', type=Path, required=True)
    parser.add_argument('--launcher', type=Path, required=True)
    parser.add_argument('--release-031', type=Path, required=True)
    parser.add_argument('--vm-image', type=Path, required=True)
    parser.add_argument('--vm-image-sha256', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--qemu-image', default='cube-qemu-lab:24.04')
    parser.add_argument('--adoption-script', type=Path, help='additional one-time manual deployment adoption rehearsal')
    args = parser.parse_args()
    if digest(args.release_031) != V031_SHA256:
        raise ValueError('the upgrade baseline must be the exact official v0.3.1 GNU archive')
    if digest(args.vm_image) != args.vm_image_sha256:
        raise ValueError('the Ubuntu cloud image does not match its verified checksum')
    if not Path('/dev/kvm').exists():
        raise RuntimeError('real upgrade acceptance requires /dev/kvm; no simulated success')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    inputs = output / 'inputs'
    inputs.mkdir()
    vm = output / 'vm'
    vm.mkdir()
    results = output / 'results'
    results.mkdir()
    candidate = inputs / 'candidate'
    candidate.mkdir()
    repo = Path(__file__).resolve().parents[3]
    version = tomllib.loads((repo / 'Cargo.toml').read_text())['workspace']['package']['version']
    if run(str(args.launcher.resolve()), '--version') != 'brocade ' + version:
        raise ValueError('candidate launcher must match this checkout version')
    (inputs / 'candidate-version').write_text(version + '\n')
    for source, target in [(args.console, candidate / 'brocade-console'),
                           (args.launcher, candidate / 'brocade-launcher'),
                           (repo / 'install.sh', candidate / 'install.sh'),
                           (args.release_031, inputs / 'v0.3.1.tar.gz'),
                           (Path(__file__).with_name('guest_systemd.py'), inputs / 'guest.py')]:
        shutil.copy2(source, target)
    (candidate / 'BROCADE_PLATFORM').write_text('x86_64-gnu\n')
    (candidate / 'SHA256SUMS').write_text(''.join(
        f'{digest(candidate / name)}  {name}\n'
        for name in ['brocade-console', 'brocade-launcher', 'BROCADE_PLATFORM', 'install.sh']))
    (inputs / 'DISPOSABLE_UPGRADE_DRILL').write_text('KVM-only\n')
    migrations = {str(int(path.name.split('_', 1)[0])): hashlib.sha384(path.read_bytes()).hexdigest()
                  for path in sorted((repo / 'crates/brocade-store/migrations').glob('*.sql'))}
    (inputs / 'candidate-migrations.json').write_text(json.dumps(migrations, sort_keys=True) + '\n')
    if args.adoption_script:
        shutil.copy2(args.adoption_script, inputs / 'adopt-runtime.py')
    (output / 'manifest.json').write_text(json.dumps({
        'console_sha256': digest(args.console), 'launcher_sha256': digest(args.launcher),
        'candidate_version': version,
        'installer_sha256': digest(repo / 'install.sh'),
        'guest_script_sha256': digest(inputs / 'guest.py'),
        'v0.3.1_sha256': digest(args.release_031),
        'os_image_sha256': digest(args.vm_image), 'qemu_image': args.qemu_image,
        'adoption_script_sha256': digest(args.adoption_script) if args.adoption_script else None,
    }, indent=2) + '\n')
    startup = '''#!/bin/sh
set -eu
mkdir -p /lab
mountpoint -q /lab || mount -t 9p -o trans=virtio,version=9p2000.L lab /lab
python3 /lab/inputs/guest.py
'''
    (vm / 'user-data').write_text('''#cloud-config
package_update: true
package_upgrade: false
packages: [python3, postgresql-16, postgresql-client-16, ca-certificates, util-linux, liblzma5]
write_files:
  - path: /usr/local/bin/brocade-upgrade-drill
    permissions: '0755'
    content: |
''' + ''.join('      ' + line + '\n' for line in startup.splitlines()) + '''  - path: /etc/systemd/system/brocade-upgrade-drill.service
    content: |
      [Unit]
      After=network-online.target cloud-final.service
      Wants=network-online.target
      [Service]
      Type=oneshot
      ExecStart=/usr/local/bin/brocade-upgrade-drill
      TimeoutStartSec=3600
      [Install]
      WantedBy=cloud-init.target
runcmd:
  - [systemctl, daemon-reload]
  - [systemctl, enable, brocade-upgrade-drill.service]
  - [systemctl, start, --no-block, brocade-upgrade-drill.service]
''')
    (vm / 'meta-data').write_text('instance-id: brocade-upgrade-' + uuid.uuid4().hex + '\n')
    image = str(args.vm_image.resolve())
    name = 'brocade-upgrade-kvm-' + uuid.uuid4().hex[:10]
    created = False
    try:
        run('docker', 'run', '--rm', '--network', 'none', '-v', f'{vm}:/vm', '-v',
            f'{image}:/base.img:ro', args.qemu_image, 'qemu-img', 'create', '-f',
            'qcow2', '-F', 'qcow2', '-b', '/base.img', '/vm/root.qcow2', '24G')
        run('docker', 'run', '--rm', '--network', 'none', '-v', f'{vm}:/vm', args.qemu_image,
            'cloud-localds', '/vm/seed.img', '/vm/user-data', '/vm/meta-data')
        run('docker', 'run', '-d', '--name', name, '--device', '/dev/kvm', '--memory', '7g',
            '--pids-limit', '1024', '-v', f'{output}:/lab', '-v', f'{vm}:/vm',
            '-v', f'{image}:/base.img:ro', args.qemu_image,
            'qemu-system-x86_64', '-accel', 'kvm', '-cpu', 'host', '-smp', '4', '-m', '6144',
            '-display', 'none', '-serial', 'file:/lab/results/serial.log',
            '-drive', 'file=/vm/root.qcow2,if=virtio,format=qcow2',
            '-drive', 'file=/vm/seed.img,if=virtio,format=raw,readonly=on',
            '-netdev', 'user,id=net0', '-device', 'virtio-net-pci,netdev=net0',
            '-fsdev', 'local,id=lab,path=/lab,security_model=none',
            '-device', 'virtio-9p-pci,fsdev=lab,mount_tag=lab')
        created = True
        seen = 0
        deadline = time.monotonic() + 3600
        while time.monotonic() < deadline:
            evidence = results / 'evidence.json'
            if evidence.exists():
                records = json.loads(evidence.read_text())
                for record in records[seen:]:
                    print(json.dumps(record), flush=True)
                seen = len(records)
            if (results / 'failure.txt').exists():
                raise RuntimeError((results / 'failure.txt').read_text())
            if (results / 'result.json').exists():
                print((results / 'result.json').read_text(), flush=True)
                return
            if run('docker', 'inspect', '-f', '{{.State.Running}}', name) != 'true':
                raise RuntimeError('KVM exited prematurely; inspect serial.log')
            time.sleep(2)
        raise RuntimeError('KVM drill exceeded one hour; inspect serial.log')
    finally:
        if created:
            (results / 'qemu.log').write_text(run('docker', 'logs', name))
            run('docker', 'rm', '-f', name)


if __name__ == '__main__':
    main()
