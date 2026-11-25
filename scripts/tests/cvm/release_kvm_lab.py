#!/usr/bin/env python3
"""Owned, disposable KVMs connected only through a loopback multicast test LAN."""

import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import tempfile
import time
import uuid


def command(words, *, data=None, timeout=180, check=True):
    result = subprocess.run(words, input=data, capture_output=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f'{words[0]} failed: {result.stderr.decode(errors="replace")[-1600:]}')
    return result


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


class Lab:
    def __init__(self, output):
        self.output = Path(output).resolve()
        self.state = json.loads((self.output / 'lab.json').read_text())

    def save(self):
        path = self.output / 'lab.json'
        path.write_text(json.dumps(self.state, indent=2))
        path.chmod(0o600)

    def ssh_words(self, role):
        return [
            'ssh',
            '-i',
            str(self.output / 'host.key'),
            '-p',
            str(self.state['roles'][role]['ssh_port']),
            '-o',
            'BatchMode=yes',
            '-o',
            'IdentitiesOnly=yes',
            '-o',
            'StrictHostKeyChecking=accept-new',
            '-o',
            f'UserKnownHostsFile={self.output / "known-hosts"}',
            '-o',
            'ConnectTimeout=3',
            'cvm-lab@127.0.0.1',
        ]

    def remote(self, role, script, *, data=None, timeout=180, check=True):
        return command([*self.ssh_words(role), script], data=data, timeout=timeout, check=check)

    def python(self, role, code, data=None, timeout=180):
        return self.remote(role, 'sudo python3 -c ' + shlex.quote(code), data=data, timeout=timeout).stdout

    def persist_routes(self, role, routes):
        """Keep owned probe/entry reply routes across the lab's clock and DHCP changes."""
        if role not in self.state['roles']:
            raise ValueError('unknown owned lab role')
        peers = {ipaddress.ip_address(value[key]) for value in self.state['roles'].values() for key in ('ipv4', 'ipv6')}
        configured = dict(self.state.get('static_routes', {}).get(role, {}))
        for destination, gateway in routes:
            network, peer = ipaddress.ip_network(destination), ipaddress.ip_address(gateway)
            if (
                not network.prefixlen
                or peer not in peers
                or str(peer) in (self.state['roles'][role]['ipv4'], self.state['roles'][role]['ipv6'])
                or network.version != peer.version
            ):
                raise ValueError('owned lab routes require an explicit prefix and same-family lab peer')
            configured[str(network)] = str(peer)
        entries = [{'to': destination, 'via': gateway} for destination, gateway in sorted(configured.items())]
        configuration = {'network': {'version': 2, 'ethernets': {'lan': {'routes': entries}}}}
        self.python(
            role,
            "import os,sys;from pathlib import Path;p=Path('/etc/netplan/90-brocade-acceptance-routes.yaml');"
            'p.write_bytes(sys.stdin.buffer.read());os.chmod(p,0o600)',
            json.dumps(configuration).encode(),
        )
        self.remote(role, 'sudo netplan generate && sudo networkctl reload && sudo networkctl reconfigure lab0')
        for destination, gateway in configured.items():
            family = '-4' if ipaddress.ip_network(destination).version == 4 else '-6'
            values = json.loads(
                self.remote(role, shlex.join(['ip', family, '-j', 'route', 'show', destination])).stdout
            )
            if not any(
                r.get('gateway') == gateway and r.get('dev') == 'lab0' and r.get('protocol') == 'static' for r in values
            ):
                raise RuntimeError('owned lab route did not become a persistent static route')
        self.state.setdefault('static_routes', {})[role] = configured
        self.save()

    def upload(self, role, source, target, mode=0o700):
        code = (
            'import os,sys,pathlib,shutil;p=pathlib.Path(' + repr(target) + ');'
            'p.parent.mkdir(parents=True,exist_ok=True);'
            'f=p.open("wb");shutil.copyfileobj(sys.stdin.buffer,f);f.close();'
            f'os.chmod(p,{mode})'
        )
        with Path(source).open('rb') as stream:
            process = subprocess.run(
                [*self.ssh_words(role), 'sudo python3 -c ' + shlex.quote(code)],
                stdin=stream,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=300,
            )
        if process.returncode:
            raise RuntimeError(f'upload to owned {role} VM failed')

    def qmp(self, role, operation):
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(5)
            sock.connect(self.state['roles'][role]['qmp'])
            file = sock.makefile('rwb')
            json.loads(file.readline())
            for request in ['qmp_capabilities', operation]:
                file.write(json.dumps({'execute': request}).encode() + b'\n')
                file.flush()
                while True:
                    response = json.loads(file.readline())
                    if 'error' in response:
                        raise RuntimeError(f'QMP {request} failed')
                    if 'return' in response:
                        break
            return response['return']

    def stop(self):
        for role, state in self.state['roles'].items():
            pid = state.get('pid')
            if pid and Path(f'/proc/{pid}/cmdline').exists():
                words = Path(f'/proc/{pid}/cmdline').read_bytes().split(b'\0')
                if state['name'].encode() not in words or not any(state['qmp'].encode() in w for w in words):
                    raise RuntimeError(f'{role} PID is not this lab; preserved for inspection')
                self.qmp(role, 'quit')
            state['stopped'] = True
        self.save()


def prepare(args):
    image = args.image.resolve()
    digest = hashlib.sha256()
    with image.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    if digest.hexdigest() != args.image_sha256:
        raise RuntimeError('OS image checksum does not match the explicitly selected image')
    output = args.output.resolve()
    output.mkdir(mode=0o700, exist_ok=False)
    task = uuid.uuid4().hex[:10]
    runtime = Path(tempfile.mkdtemp(prefix='brocade-kvm-'))
    command(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(output / 'host.key')])
    public = (output / 'host.key.pub').read_text().strip()
    subnet = 32 + int(task[:2], 16) % 190
    state = {
        'task': task,
        'runtime': str(runtime),
        'os_sha256': args.image_sha256,
        'group': f'239.192.{int(task[2:4], 16)}.{int(task[4:6], 16)}',
        'lan_port': free_port(),
        'roles': {},
        'console_port': free_port(),
    }
    path = output / 'lab.json'
    path.write_text(json.dumps(state, indent=2))
    path.chmod(0o600)
    lab = Lab(output)
    for number, (role, memory, cpus) in enumerate(
        [('compute', 8192, 4), ('provider', 2048, 2), ('client', 1024, 1)], 1
    ):
        directory = output / role
        directory.mkdir(mode=0o700)
        mac = f'52:54:{task[:2]}:{task[2:4]}:{number:02x}'
        wan_mac, lan_mac = mac + ':01', mac + ':02'
        ipv4, ipv6 = f'172.30.{subnet}.{number * 10}', f'fdc0:{task[:4]}:1::{number}'
        userdata = {
            'hostname': 'cvm-lab-' + role,
            'manage_etc_hosts': True,
            'users': [
                {
                    'name': 'cvm-lab',
                    'shell': '/bin/bash',
                    'lock_passwd': True,
                    'sudo': ['ALL=(ALL) NOPASSWD:ALL'],
                    'ssh_authorized_keys': [public],
                }
            ],
            'ssh_pwauth': False,
            'package_update': False,
            'runcmd': [['timedatectl', 'set-ntp', 'false']],
        }
        network = {
            'version': 2,
            'ethernets': {
                'wan': {
                    'match': {'macaddress': wan_mac},
                    'dhcp4': True,
                    'dhcp6': False,
                    'optional': True,
                },
                'lan': {
                    'match': {'macaddress': lan_mac},
                    'set-name': 'lab0',
                    'addresses': [ipv4 + '/24', ipv6 + '/64'],
                    'dhcp6': False,
                    'accept-ra': False,
                    'optional': True,
                },
            },
        }
        (directory / 'user-data').write_text('#cloud-config\n' + json.dumps(userdata))
        (directory / 'meta-data').write_text(
            json.dumps({'instance-id': task + role, 'local-hostname': 'cvm-lab-' + role})
        )
        (directory / 'network.json').write_text(json.dumps(network))
        command(
            [
                'cloud-localds',
                '--network-config=' + str(directory / 'network.json'),
                str(directory / 'seed.img'),
                str(directory / 'user-data'),
                str(directory / 'meta-data'),
            ]
        )
        command(
            ['qemu-img', 'create', '-f', 'qcow2', '-F', 'qcow2', '-b', str(image), str(directory / 'root.qcow2'), '48G']
        )
        ssh_port = free_port()
        name = f'brocade-release-{task}-{role}'
        qmp = runtime / (role + '.qmp')
        forwards = f'user,id=wan,hostfwd=tcp:127.0.0.1:{ssh_port}-:22'
        if role == 'compute':
            forwards += f',hostfwd=tcp:127.0.0.1:{state["console_port"]}-:8080'
        words = [
            'qemu-system-x86_64',
            '-name',
            name,
            '-machine',
            'q35,accel=kvm',
            '-cpu',
            'host,kvm-asyncpf=off,kvm-asyncpf-int=off',
            '-m',
            str(memory),
            '-smp',
            str(cpus),
            '-display',
            'none',
            '-daemonize',
            '-qmp',
            f'unix:{qmp},server=on,wait=off',
            '-pidfile',
            str(directory / 'pid'),
            '-serial',
            'file:' + str(directory / 'serial.log'),
            '-drive',
            f'file={directory / "root.qcow2"},if=virtio,format=qcow2',
            '-drive',
            f'file={directory / "seed.img"},media=cdrom,readonly=on',
            '-netdev',
            forwards,
            '-device',
            f'virtio-net-pci,netdev=wan,mac={wan_mac}',
            '-netdev',
            f'socket,id=lan,mcast={state["group"]}:{state["lan_port"]},localaddr=127.0.0.1',
            '-device',
            f'virtio-net-pci,netdev=lan,mac={lan_mac}',
        ]
        command(words)
        lab.state['roles'][role] = {
            'name': name,
            'pid': int((directory / 'pid').read_text()),
            'ssh_port': ssh_port,
            'ipv4': ipv4,
            'ipv6': ipv6,
            'qmp': str(qmp),
            'memory_mib': memory,
            'cpus': cpus,
            'command': words,
        }
        lab.save()
        assert lab.qmp(role, 'query-kvm')['enabled'] is True
        print(f'STARTED {role}: real KVM, {cpus} vCPU, {memory} MiB', flush=True)
    for role in lab.state['roles']:
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            result = lab.remote(role, 'true', timeout=8, check=False)
            if result.returncode == 0:
                break
            time.sleep(1)
        else:
            raise RuntimeError(f'{role} SSH did not become ready; owned VM retained')
        lab.remote(role, 'sudo cloud-init status --wait', timeout=240)
        release = lab.remote(role, 'cat /etc/os-release; uname -r; cat /proc/sys/kernel/random/boot_id').stdout.decode()
        if 'VERSION_ID="26.04"' not in release:
            raise RuntimeError('VM did not boot the selected Ubuntu 26.04 image')
        (output / role / 'os.txt').write_text(release)
        lab.state['roles'][role]['verified_os'] = release
        print(f'READY {role}: Ubuntu 26.04, SSH and cloud-init', flush=True)
    target = lab.state['roles']['provider']['ipv4']
    lab.remote('compute', 'ping -c 2 -W 2 ' + target)
    lab.save()
    print('PASS: actual KVMs communicate over their private virtual LAN', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['prepare', 'stop'])
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--image', type=Path)
    parser.add_argument('--image-sha256')
    args = parser.parse_args()
    if args.action == 'prepare':
        if not args.image or not args.image_sha256:
            parser.error('prepare requires an explicitly pinned OS image')
        prepare(args)
    else:
        Lab(args.output).stop()


if __name__ == '__main__':
    main()
