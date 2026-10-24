#!/usr/bin/env python3
"""Run identical frozen HTTP acceptance inputs in disposable Docker/KVM environments."""
import argparse
import concurrent.futures
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import time
import uuid


def run(*args, timeout=30):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT, timeout=timeout).strip()


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def monitor(output, container, timeout, stop_for_reboot=False):
    deadline = time.monotonic() + timeout
    delivered = {}
    while time.monotonic() < deadline:
        for filename in ['test-progress.json', 'evidence.json']:
            path = output / filename
            if path.exists():
                try:
                    records = json.loads(path.read_text())
                    for record in records[delivered.get(filename, 0):]:
                        print(json.dumps({'runtime': output.name, **record}), flush=True)
                    delivered[filename] = len(records)
                except json.JSONDecodeError:
                    pass
        if (output / 'failure.txt').exists():
            raise RuntimeError((output / 'failure.txt').read_text())
        if (output / 'result.json').exists():
            return
        if stop_for_reboot and (output / 'reboot.json').exists():
            return
        if run('docker', 'inspect', '-f', '{{.State.Running}}', container) != 'true':
            raise RuntimeError('environment exited early: ' + run('docker', 'logs', '--tail', '35', container))
        time.sleep(1)
    raise RuntimeError(f'acceptance exceeded {timeout}s: {output}')


def cleanup(container, output, log):
    result = subprocess.run(['docker', 'logs', container], text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, timeout=20)
    (output / log).write_text(result.stdout)
    subprocess.run(['docker', 'rm', '-f', container], check=True, stdout=subprocess.PIPE,
                   stderr=subprocess.STDOUT, timeout=20)


def docker(args, inputs):
    output = args.output / 'docker'
    output.mkdir()
    name = 'brocade-http-' + uuid.uuid4().hex[:10]
    created = False
    try:
        run('docker', 'run', '-d', '--name', name, '--network', 'none', '--memory', '2g',
            '--pids-limit', '1024', '--cap-add', 'NET_ADMIN',
            '--sysctl', 'net.ipv6.conf.all.disable_ipv6=0',
            '--sysctl', 'net.ipv6.conf.default.disable_ipv6=0',
            '-e', 'BROCADE_HTTP_ACCEPTANCE_DISPOSABLE=1', '-e', 'BROCADE_HTTP_ACCEPTANCE_RUNTIME=docker',
            '-v', f'{inputs}:/inputs:ro', '-v', f'{output}:/results', '--entrypoint', 'python3',
            args.docker_image, '/inputs/guest.py', '--output', '/results')
        created = True
        monitor(output, name, 1500, stop_for_reboot=True)
        for _ in range(50):
            if run('docker', 'inspect', '-f', '{{.State.Running}}', name) == 'false':
                break
            time.sleep(.1)
        else:
            raise RuntimeError('Docker guest did not stop after its reboot checkpoint')
        before = run('docker', 'inspect', '-f', '{{.State.StartedAt}}', name)
        run('docker', 'start', name)
        after = run('docker', 'inspect', '-f', '{{.State.StartedAt}}', name)
        if before == after:
            raise RuntimeError('Docker did not really restart')
        (output / 'container-restart.json').write_text(json.dumps({'before': before, 'after': after}))
        monitor(output, name, 180)
    finally:
        if created:
            cleanup(name, output, 'container.log')


def kvm(args, inputs):
    output = args.output / 'kvm'
    output.mkdir()
    vm = args.output / 'vm'
    vm.mkdir()
    if not args.vm_image or not args.vm_sha256 or digest(args.vm_image) != args.vm_sha256:
        raise RuntimeError('KVM requires --vm-image and its verified --vm-sha256')
    (vm / 'image.json').write_text(json.dumps({'source': str(args.vm_image), 'sha256': args.vm_sha256}))
    startup = '''#!/bin/sh
set -eu
mkdir -p /lab /inputs
mountpoint -q /lab || mount -t 9p -o trans=virtio,version=9p2000.L lab /lab
mountpoint -q /inputs || mount --bind /lab/inputs /inputs
export BROCADE_HTTP_ACCEPTANCE_DISPOSABLE=1
export BROCADE_HTTP_ACCEPTANCE_RUNTIME=kvm
python3 /inputs/guest.py --output /lab/kvm
if [ ! -f /lab/kvm/result.json ]; then
  systemctl reboot
fi
'''
    (vm / 'user-data').write_text('''#cloud-config
package_update: true
packages: [python3, iproute2, kmod, curl, procps, ca-certificates]
write_files:
  - path: /usr/local/bin/brocade-http-acceptance
    permissions: '0755'
    content: |
''' + ''.join('      ' + line + '\n' for line in startup.splitlines()) + '''  - path: /etc/systemd/system/brocade-http-acceptance.service
    content: |
      [Unit]
      After=network-online.target cloud-final.service
      Wants=network-online.target
      [Service]
      Type=oneshot
      ExecStart=/usr/local/bin/brocade-http-acceptance
      TimeoutStartSec=1600
      [Install]
      WantedBy=cloud-init.target
runcmd:
  - [systemctl, daemon-reload]
  - [systemctl, enable, brocade-http-acceptance.service]
  - [systemctl, start, --no-block, brocade-http-acceptance.service]
''')
    (vm / 'meta-data').write_text('instance-id: brocade-http-' + uuid.uuid4().hex + '\n')
    name = 'brocade-http-kvm-' + uuid.uuid4().hex[:10]
    created = False
    try:
        run('docker', 'run', '--rm', '--network', 'none', '-v', f'{vm}:/vm',
            '-v', f'{args.vm_image}:/base.img:ro', args.qemu_image,
            'qemu-img', 'create', '-f', 'qcow2', '-F', 'qcow2', '-b', '/base.img', '/vm/root.qcow2', '6G')
        run('docker', 'run', '--rm', '--network', 'none', '-v', f'{vm}:/vm', args.qemu_image,
            'cloud-localds', '/vm/seed.img', '/vm/user-data', '/vm/meta-data')
        run('docker', 'run', '-d', '--name', name, '--device', '/dev/kvm', '--memory', '4g',
            '--pids-limit', '1024', '-v', f'{args.output}:/lab', '-v', f'{vm}:/vm',
            '-v', f'{args.vm_image}:/base.img:ro', args.qemu_image,
            'qemu-system-x86_64', '-accel', 'kvm', '-cpu', 'host', '-smp', '4', '-m', '3072',
            '-display', 'none', '-serial', 'file:/lab/kvm/serial.log',
            '-qmp', 'unix:/vm/qmp.sock,server=on,wait=off',
            '-drive', 'file=/vm/root.qcow2,if=virtio,format=qcow2',
            '-drive', 'file=/vm/seed.img,if=virtio,format=raw,readonly=on',
            '-netdev', 'user,id=net0', '-device', 'virtio-net-pci,netdev=net0',
            '-fsdev', 'local,id=lab,path=/lab,security_model=none',
            '-device', 'virtio-9p-pci,fsdev=lab,mount_tag=lab')
        created = True
        query = '''import json,socket,time
s=socket.socket(socket.AF_UNIX);s.settimeout(5)
for _ in range(50):
 try:s.connect('/vm/qmp.sock');break
 except (FileNotFoundError,ConnectionRefusedError):time.sleep(.1)
f=s.makefile('rwb',buffering=0);json.loads(f.readline())
for command in ['qmp_capabilities','query-kvm']:
 f.write((json.dumps({'execute':command})+'\\n').encode())
 while True:
  reply=json.loads(f.readline())
  if 'return' in reply:break
print(json.dumps(reply['return']))
'''
        acceleration = json.loads(run('docker', 'exec', name, 'python3', '-c', query))
        if not acceleration.get('enabled') or not acceleration.get('present'):
            raise RuntimeError('QEMU did not enable KVM hardware acceleration')
        (output / 'kvm-acceleration.json').write_text(json.dumps(acceleration))
        monitor(output, name, 1500)
    finally:
        if created:
            cleanup(name, output, 'qemu.log')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--runtime', choices=['docker', 'kvm', 'both'], default='both')
    for name in ['agent', 'xray', 'legacy-xray', 'tests', 'output']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--vm-image', type=Path)
    parser.add_argument('--vm-sha256')
    parser.add_argument('--docker-image', default='brocade-forwarding-continuity:local')
    parser.add_argument('--qemu-image', default='cube-qemu-lab:24.04')
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    args.vm_image = args.vm_image.resolve() if args.vm_image else None
    inputs = args.output / 'inputs'
    inputs.mkdir()
    for source, name in [(args.agent, 'agent'), (args.xray, 'xray'), (args.legacy_xray, 'legacy-xray'),
                         (args.tests, 'agent-tests'), (Path(__file__).with_name('guest.py'), 'guest.py')]:
        shutil.copy2(source, inputs / name)
    manifest = {'inputs': {path.name: digest(path) for path in sorted(inputs.iterdir())},
                'host_kernel': run('uname', '-r'),
                'host_boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip()}
    for runtime, field in [('docker', 'docker_image'), ('kvm', 'qemu_image')]:
        if args.runtime in [runtime, 'both']:
            image_id = run('docker', 'image', 'inspect', '-f', '{{.Id}}', getattr(args, field))
            manifest[field] = image_id
            setattr(args, field, image_id)
    (args.output / 'manifest.json').write_text(json.dumps(manifest, indent=2))
    tasks = [docker, kvm] if args.runtime == 'both' else [docker if args.runtime == 'docker' else kvm]
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(tasks)) as pool:
        futures = [pool.submit(task, args, inputs) for task in tasks]
        for future in futures:
            future.result()


if __name__ == '__main__':
    main()
