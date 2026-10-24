#!/usr/bin/env python3
"""Run the identical real-process acceptance suite in Docker and an accelerated KVM guest."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import time
import uuid


def run(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT, timeout=30).strip()


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def monitor(output, timeout, container, reboot=False, after=0):
    deadline = time.monotonic() + timeout
    last = after
    while time.monotonic() < deadline:
        evidence = output / 'evidence.json'
        if evidence.exists():
            try:
                records = json.loads(evidence.read_text())
                for record in records[last:]:
                    print(json.dumps({'runtime': output.name, **record}), flush=True)
                last = len(records)
            except json.JSONDecodeError:
                pass
        if (output / 'failure.txt').exists():
            raise RuntimeError((output / 'failure.txt').read_text())
        if (output / 'result.json').exists():
            return
        if reboot and (output / 'reboot.json').exists():
            return
        status = run('docker', 'inspect', '-f', '{{.State.Running}}', container)
        if status != 'true':
            raise RuntimeError(f'guest stopped prematurely: {run("docker", "logs", "--tail", "25", container)}')
        time.sleep(1)
    raise RuntimeError(f'guest timeout: {output}')


def docker(args, inputs):
    output = args.output / 'docker'
    output.mkdir()
    name = 'brocade-traffic-limit-' + uuid.uuid4().hex[:10]
    try:
        run('docker', 'run', '-d', '--name', name, '--network', 'none', '--memory', '2g',
            '--pids-limit', '1024', '--cap-add', 'NET_ADMIN', '--cap-add', 'SYS_ADMIN',
            '--security-opt', 'apparmor=unconfined', '--security-opt', 'seccomp=unconfined',
            '-e', 'BROCADE_TRAFFIC_LIMIT_DISPOSABLE=1', '-e', 'BROCADE_TRAFFIC_LIMIT_RUNTIME=docker',
            '-e', f'BROCADE_TRAFFIC_LIMIT_FAULT_INJECTION={int(args.fault_injection)}',
            '-v', f'{inputs}:/inputs:ro', '-v', f'{output}:/results',
            '--entrypoint', 'python3', args.docker_image, '/inputs/guest.py', '--output', '/results')
        monitor(output, 1200 if args.fault_injection else 720, name, reboot=True)
        # The first suite may have finished before Docker reports PID 1 exiting.
        for _ in range(20):
            if run('docker', 'inspect', '-f', '{{.State.Running}}', name) == 'false':
                break
            time.sleep(.2)
        first = run('docker', 'inspect', '-f', '{{.State.StartedAt}}', name)
        records = len(json.loads((output / 'evidence.json').read_text()))
        run('docker', 'start', name)
        second = run('docker', 'inspect', '-f', '{{.State.StartedAt}}', name)
        if first == second:
            raise RuntimeError('Docker did not actually restart the container')
        monitor(output, 180, name, after=records)
        (output / 'container-restart.json').write_text(json.dumps({'before': first, 'after': second}))
    finally:
        (output / 'container.log').write_text(run('docker', 'logs', name))
        run('docker', 'rm', '-f', name)


def kvm(args, inputs):
    output = args.output / 'kvm'
    output.mkdir()
    vm = args.output / 'vm'
    vm.mkdir()
    if not args.vm_image or not args.vm_image.is_file():
        raise RuntimeError('--vm-image must select a verified amd64 Ubuntu 24.04 cloud image')
    (vm / 'image.json').write_text(json.dumps({'source': str(args.vm_image), 'sha256': digest(args.vm_image)}))
    # Cloud-init mounts the isolated laboratory share; all Agent state remains on the guest disk.
    startup = '''#!/bin/sh
set -eu
mkdir -p /lab
mountpoint -q /lab || mount -t 9p -o trans=virtio,version=9p2000.L lab /lab
mkdir -p /inputs
mountpoint -q /inputs || mount --bind /lab/inputs /inputs
export BROCADE_TRAFFIC_LIMIT_DISPOSABLE=1
export BROCADE_TRAFFIC_LIMIT_RUNTIME=kvm
export BROCADE_TRAFFIC_LIMIT_FAULT_INJECTION=FAULT_INJECTION_VALUE
for tool in python3 ip tc modprobe; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'missing KVM guest dependency: %s\n' "$tool" > /lab/kvm/failure.txt
    exit 1
  fi
done
python3 /inputs/guest.py --output /lab/kvm
if [ ! -f /lab/kvm/result.json ]; then
  systemctl reboot
fi
'''.replace('FAULT_INJECTION_VALUE', str(int(args.fault_injection)))
    packages = 'package_update: true\npackages: [python3, iproute2, kmod]\n' if args.provision_packages else 'package_update: false\n'
    (vm / 'user-data').write_text('''#cloud-config
''' + packages + '''write_files:
  - path: /usr/local/bin/brocade-traffic-acceptance
    permissions: '0755'
    content: |
''' + ''.join('      ' + line + '\n' for line in startup.splitlines()) + '''  - path: /etc/systemd/system/brocade-traffic-acceptance.service
    content: |
      [Unit]
      After=network-online.target cloud-final.service
      Wants=network-online.target
      [Service]
      Type=oneshot
      ExecStart=/usr/local/bin/brocade-traffic-acceptance
      TimeoutStartSec=1200
      [Install]
      WantedBy=cloud-init.target
runcmd:
  - [systemctl, daemon-reload]
  - [systemctl, enable, brocade-traffic-acceptance.service]
  - [systemctl, start, --no-block, brocade-traffic-acceptance.service]
''')
    (vm / 'meta-data').write_text('instance-id: brocade-traffic-' + uuid.uuid4().hex + '\n')
    name = 'brocade-traffic-kvm-' + uuid.uuid4().hex[:10]
    try:
        run('docker', 'run', '--rm', '--network', 'none', '-v', f'{vm}:/vm', '-v',
            f'{args.vm_image}:/base.img:ro', args.qemu_image, 'qemu-img', 'create', '-f',
            'qcow2', '-F', 'qcow2', '-b', '/base.img', '/vm/root.qcow2', '6G')
        run('docker', 'run', '--rm', '--network', 'none', '-v', f'{vm}:/vm', args.qemu_image,
            'cloud-localds', '/vm/seed.img', '/vm/user-data', '/vm/meta-data')
        run('docker', 'run', '-d', '--name', name, '--device', '/dev/kvm', '--memory', '4g',
            '--pids-limit', '1024', '-v', f'{args.output}:/lab', '-v', f'{vm}:/vm',
            '-v', f'{args.vm_image}:/base.img:ro', args.qemu_image,
            'qemu-system-x86_64', '-accel', 'kvm', '-cpu', 'host', '-smp', '4', '-m', '3072',
            '-display', 'none', '-serial', 'file:/lab/kvm/serial.log',
            '-drive', 'file=/vm/root.qcow2,if=virtio,format=qcow2',
            '-drive', 'file=/vm/seed.img,if=virtio,format=raw,readonly=on',
            '-netdev', 'user,id=net0', '-device', 'virtio-net-pci,netdev=net0',
            '-fsdev', 'local,id=lab,path=/lab,security_model=none',
            '-device', 'virtio-9p-pci,fsdev=lab,mount_tag=lab')
        monitor(output, 1800 if args.fault_injection else 1200, name)
    finally:
        (output / 'qemu.log').write_text(run('docker', 'logs', name))
        run('docker', 'rm', '-f', name)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--runtime', choices=['docker', 'kvm', 'both'], default='both')
    parser.add_argument('--agent', type=Path, required=True)
    parser.add_argument('--xray', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--vm-image', type=Path)
    parser.add_argument('--fault-injection', action='store_true')
    parser.add_argument('--provision-packages', action='store_true')
    parser.add_argument('--docker-image', default='brocade-forwarding-continuity:local')
    parser.add_argument('--qemu-image', default='cube-qemu-lab:24.04')
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    args.vm_image = args.vm_image.resolve() if args.vm_image else None
    inputs = args.output / 'inputs'
    inputs.mkdir()
    for source, target in [(args.agent, 'agent'), (args.xray, 'xray'),
                           (Path(__file__).with_name('guest.py'), 'guest.py')]:
        shutil.copy2(source, inputs / target)
    (args.output / 'manifest.json').write_text(json.dumps({
        'agent_sha256': digest(inputs / 'agent'), 'xray_sha256': digest(inputs / 'xray'),
        'fixture_sha256': digest(inputs / 'guest.py'), 'docker_image': args.docker_image,
        'qemu_image': args.qemu_image,
        'fault_injection': args.fault_injection,
        'provision_packages': args.provision_packages,
    }, indent=2))
    if args.runtime in ['docker', 'both']:
        docker(args, inputs)
    if args.runtime in ['kvm', 'both']:
        kvm(args, inputs)


if __name__ == '__main__':
    main()
