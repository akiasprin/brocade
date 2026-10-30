#!/usr/bin/env python3
"""Verify default WebSocket compression with real Console/Agent in isolated Docker and KVM."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import uuid


def command(*args, timeout=60):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT, timeout=timeout).strip()


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def save(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def monitor(args, output, container, database, dbname):
    deadline, delivered, last_sql, last_progress = time.monotonic() + 900, 0, None, 0
    while time.monotonic() < deadline:
        request = output / 'sql-request.json'
        if request.exists():
            value = json.loads(request.read_text())
            if value['id'] != last_sql:
                query = value['query']
                if value['data'] is not None:
                    data = json.dumps(value['data']).replace('\\', '\\\\')
                    query = 'CREATE TEMP TABLE lab_seed(data jsonb);\nCOPY lab_seed FROM STDIN;\n' + data + '\n\\.\n' + query
                result = subprocess.run(['docker', 'exec', '-i', database, 'psql', '-XqAt', '-U', 'brocade',
                    '-d', dbname, '-v', 'ON_ERROR_STOP=1'], input=query, text=True, capture_output=True, timeout=45)
                if result.returncode:
                    (output / 'sql-error-private.txt').write_text(result.stderr)
                save(output / 'sql-response.json', {'id': value['id'], 'ok': result.returncode == 0, 'result': result.stdout})
                last_sql = value['id']
        evidence = output / 'evidence.json'
        if evidence.exists():
            records = json.loads(evidence.read_text())
            for item in records[delivered:]:
                print(json.dumps({'runtime': output.name, **item}), flush=True)
            delivered = len(records)
        progress = output / 'load-progress.json'
        if progress.exists():
            records = json.loads(progress.read_text())
            if len(records) > last_progress:
                latest = dict(records[-1])
                if output.name == 'kvm':
                    latest['qemu_open_fds'] = int(command('docker', 'exec', container, 'python3', '-c',
                        "from pathlib import Path;print(len(list(Path('/proc/1/fd').iterdir())))"))
                print(json.dumps({'runtime': output.name, 'load_progress': latest}), flush=True)
                last_progress = len(records)
        if (output / 'result.json').exists():
            result = json.loads((output / 'result.json').read_text())
            if not result['passed']:
                raise RuntimeError(f'{output.name} failed; inspect {output}/failure.txt')
            return
        if (output / 'guest-failed').exists():
            raise RuntimeError(f'{output.name} guest exited unsuccessfully; inspect guest.log')
        if command('docker', 'inspect', '-f', '{{.State.Running}}', container) != 'true':
            raise RuntimeError(f'{output.name} exited; inspect its container or serial log')
        time.sleep(.2)
    raise RuntimeError(f'{output.name} exceeded the 900-second acceptance budget')


def clean(container, output):
    (output / 'container.log').write_text(command('docker', 'logs', '--tail', '200', container))
    command('docker', 'rm', '-f', container)


def docker(args, inputs, network, database):
    output = args.output / 'docker'
    name = 'brocade-realtime-docker-' + uuid.uuid4().hex[:10]
    created = False
    try:
        command('docker', 'run', '-d', '--name', name, '--network', network, '--memory', '6g', '--cpus', '4',
                '--pids-limit', '4096', '--ulimit', 'nofile=16384:16384', '--cap-add', 'NET_ADMIN',
                '-e', 'BROCADE_REALTIME_DISPOSABLE=1', '-v', f'{inputs}:/inputs:ro', '-v', f'{output}:/results',
                '--entrypoint', 'python3', args.docker_image, '/inputs/guest.py', '--runtime', 'docker',
                '--output', '/results', '--nodes', str(args.nodes), '--real-agents', str(args.real_agents), '--seconds', str(args.seconds))
        created = True
        monitor(args, output, name, database, 'realtime_docker')
    finally:
        if created:
            clean(name, output)


def kvm(args, inputs, network, database):
    output = args.output / 'kvm'
    vm = args.output / 'vm'
    vm.mkdir()
    if not args.vm_image or not args.vm_sha256 or digest(args.vm_image) != args.vm_sha256:
        raise RuntimeError('KVM requires a verified image and matching SHA-256')
    if not Path('/dev/kvm').exists():
        raise RuntimeError('KVM hardware acceleration is required')
    startup = f'''#!/bin/sh
set -eu
mkdir -p /lab /inputs
mountpoint -q /lab || mount -t 9p -o trans=virtio,version=9p2000.L lab /lab
mountpoint -q /inputs || mount --bind /lab/inputs /inputs
export BROCADE_REALTIME_DISPOSABLE=1
if ! python3 /inputs/guest.py --runtime kvm --output /lab/kvm --nodes {args.nodes} --real-agents {args.real_agents} --seconds {args.seconds} > /lab/kvm/guest.log 2>&1; then\n  echo failed > /lab/kvm/guest-failed\n  exit 1\nfi
'''
    (vm / 'user-data').write_text('''#cloud-config
package_update: false
package_upgrade: false
write_files:
  - path: /usr/local/bin/brocade-realtime-acceptance
    permissions: '0755'
    content: |
''' + ''.join('      ' + line + '\n' for line in startup.splitlines()) + '''  - path: /etc/systemd/system/brocade-realtime-acceptance.service
    content: |
      [Unit]
      After=network-online.target cloud-final.service
      Wants=network-online.target
      [Service]
      Type=oneshot
      ExecStart=/usr/local/bin/brocade-realtime-acceptance
      TimeoutStartSec=900
      LimitNOFILE=65536
      [Install]
      WantedBy=cloud-init.target
runcmd:
  - [systemctl, daemon-reload]
  - [systemctl, enable, brocade-realtime-acceptance.service]
  - [systemctl, start, --no-block, brocade-realtime-acceptance.service]
''')
    (vm / 'meta-data').write_text('instance-id: brocade-realtime-' + uuid.uuid4().hex + '\n')
    name = 'brocade-realtime-kvm-' + uuid.uuid4().hex[:10]
    created = False
    try:
        common = ['docker', 'run', '--rm', '--network', 'none', '-v', f'{vm}:/vm']
        command(*common, '-v', f'{args.vm_image}:/base.img:ro', args.qemu_image,
                'qemu-img', 'create', '-f', 'qcow2', '-F', 'qcow2', '-b', '/base.img', '/vm/root.qcow2', '16G')
        command(*common, args.qemu_image, 'cloud-localds', '/vm/seed.img', '/vm/user-data', '/vm/meta-data')
        command('docker', 'run', '-d', '--name', name, '--network', network, '--device', '/dev/kvm',
                '--memory', '7g', '--pids-limit', '1024', '--ulimit', 'nofile=65536:65536', '-v', f'{args.output}:/lab', '-v', f'{vm}:/vm',
                '-v', f'{args.vm_image}:/base.img:ro', args.qemu_image,
                'qemu-system-x86_64', '-accel', 'kvm', '-cpu', 'host', '-smp', '4', '-m', '6144',
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
        acceleration = json.loads(command('docker', 'exec', name, 'python3', '-c', query))
        if not acceleration.get('enabled') or not acceleration.get('present'):
            raise RuntimeError('QMP did not confirm real KVM acceleration')
        save(output / 'acceleration.json', acceleration)
        limits = command('docker', 'exec', name, 'python3', '-c',
            "from pathlib import Path;print(next(line for line in Path('/proc/1/limits').read_text().splitlines() if 'open files' in line))")
        save(output / 'qemu-limits.json', {'open_files': limits, 'configured_soft_and_hard': 65536})
        monitor(args, output, name, database, 'realtime_kvm')
    finally:
        if created:
            clean(name, output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--console', type=Path, required=True)
    parser.add_argument('--agent', type=Path, required=True)
    parser.add_argument('--runtime', choices=['docker', 'kvm', 'both'], default='both')
    parser.add_argument('--nodes', type=int, default=1000)
    parser.add_argument('--real-agents', type=int, default=32)
    parser.add_argument('--seconds', type=int, default=90)
    parser.add_argument('--vm-image', type=Path)
    parser.add_argument('--vm-sha256')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--docker-image', default='brocade-forwarding-continuity:local')
    parser.add_argument('--qemu-image', default='cube-qemu-lab:24.04')
    parser.add_argument('--database-image', default='postgres:17-alpine')
    args = parser.parse_args()
    os.umask(0o077)
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    inputs = args.output / 'inputs'
    inputs.mkdir()
    for source, name in [(args.console, 'console'), (args.agent, 'agent'), (Path(__file__).with_name('guest.py'), 'guest.py')]:
        shutil.copy2(source.resolve(), inputs / name)
    runtimes = ['docker', 'kvm'] if args.runtime == 'both' else [args.runtime]
    for runtime in runtimes:
        (args.output / runtime).mkdir()
    if args.vm_image:
        args.vm_image = args.vm_image.resolve()
    manifest = {'inputs': {name: digest(inputs / name) for name in ['console', 'agent', 'guest.py']},
                'simulated_nodes': args.nodes, 'real_agents': args.real_agents, 'seconds': args.seconds,
                'compression_required': True, 'vm_sha256': args.vm_sha256, 'images': {}}
    required_images = ['database_image'] + (['docker_image'] if 'docker' in runtimes else []) + (['qemu_image'] if 'kvm' in runtimes else [])
    for field in required_images:
        identifier = command('docker', 'image', 'inspect', '-f', '{{.Id}}', getattr(args, field))
        manifest['images'][field] = identifier
        setattr(args, field, identifier)
    save(args.output / 'manifest.json', manifest)
    prefix = 'brocade-realtime-' + uuid.uuid4().hex[:10]
    network, database = prefix + '-net', prefix + '-db'
    network_created = database_created = False
    try:
        command('docker', 'network', 'create', '--internal', network)
        network_created = True
        command('docker', 'run', '-d', '--name', database, '--network', network, '--memory', '1g', '--pids-limit', '256',
                '-e', 'POSTGRES_USER=brocade', '-e', 'POSTGRES_HOST_AUTH_METHOD=trust', args.database_image)
        database_created = True
        for _ in range(100):
            if subprocess.run(['docker', 'exec', database, 'pg_isready', '-h', '127.0.0.1', '-U', 'brocade'], capture_output=True, timeout=5).returncode == 0:
                break
            time.sleep(.2)
        else:
            raise RuntimeError('private PostgreSQL did not start')
        address = json.loads(command('docker', 'inspect', database))[0]['NetworkSettings']['Networks'][network]['IPAddress']
        config = {'uid': os.getuid(), 'gid': os.getgid()}
        for runtime in runtimes:
            command('docker', 'exec', database, 'createdb', '-h', '127.0.0.1', '-U', 'brocade', 'realtime_' + runtime)
            config[runtime] = {'database_url': f'postgres://brocade@{address}:5432/realtime_{runtime}'}
        save(inputs / 'config.json', config)
        tasks = [docker, kvm] if args.runtime == 'both' else [docker if args.runtime == 'docker' else kvm]
        with concurrent.futures.ThreadPoolExecutor(max_workers=len(tasks)) as pool:
            futures = [pool.submit(task, args, inputs, network, database) for task in tasks]
            for future in futures:
                future.result()
        save(args.output / 'result.json', {'passed': True, 'runtimes': runtimes, 'manifest': manifest})
    finally:
        if database_created:
            (args.output / 'postgres.log').write_text(command('docker', 'logs', '--tail', '100', database))
            command('docker', 'rm', '-f', database)
        if network_created:
            command('docker', 'network', 'rm', network)


if __name__ == '__main__':
    main()
