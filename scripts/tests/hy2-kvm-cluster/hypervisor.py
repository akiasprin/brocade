#!/usr/bin/env python3
"""Four QEMU processes; TAP/vhost bridges stay inside a network-none container."""
import json
from pathlib import Path
import signal
import socket
import subprocess
import time
import traceback

ROOT = Path('/lab')
CHILDREN = []
NODES = {
    'client': (4, 1536, [('1', '2', 'edge-0')]),
    'edge': (2, 768, [('1', '1', 'client-0'), ('2', '1', 'node-0')]),
    'node': (4, 2048, [('2', '2', 'edge-1'), ('3', '1', 'target-0')]),
    'target': (2, 768, [('3', '2', 'node-1')]),
}


def run(*args):
    subprocess.run([str(x) for x in args], check=True)


def cleanup(*_):
    for p in CHILDREN:
        if p.poll() is None: p.terminate()
    for p in CHILDREN:
        try: p.wait(timeout=10)
        except subprocess.TimeoutExpired: p.kill(); p.wait(timeout=5)
    raise SystemExit()


def qmp(path):
    s = socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect(str(path))
    with s, s.makefile('rwb', buffering=0) as f:
        json.loads(f.readline())
        for command in ('qmp_capabilities', 'query-kvm'):
            f.write((json.dumps({'execute':command})+'\n').encode())
            while True:
                result = json.loads(f.readline())
                if 'error' in result: raise RuntimeError(result)
                if 'return' in result: break
        assert result['return'] == {'enabled': True, 'present': True}
        return result['return']


def main():
    signal.signal(signal.SIGTERM, cleanup)
    for segment in ('1','2','3'):
        run('ip','link','add','lan'+segment,'type','bridge')
        run('ip','link','set','lan'+segment,'up')
    for role, (cpus, memory, links) in NODES.items():
        vm = ROOT / ('vm-' + role); vm.mkdir()
        (ROOT / 'results' / role).mkdir()
        startup = f'''#!/bin/sh
set -eu
mkdir -p /lab
mount -t 9p -o trans=virtio,version=9p2000.L lab /lab
printf '#!/bin/sh\\nexit 101\\n' > /usr/sbin/policy-rc.d
chmod 755 /usr/sbin/policy-rc.d
DEBIAN_FRONTEND=noninteractive dpkg -i /lab/inputs/packages/*.deb > /lab/results/{role}/packages.log 2>&1
python3 -u /lab/inputs/guest.py {role} > /lab/results/{role}/guest.log 2>&1
'''
        (vm / 'user-data').write_text('#cloud-config\nwrite_files:\n  - path: /usr/local/bin/cluster-lab\n    permissions: "0755"\n    content: |\n' + ''.join('      '+line+'\n' for line in startup.splitlines()) + "runcmd:\n  - [sh, -c, '/usr/local/bin/cluster-lab > /var/log/cluster-lab.log 2>&1 &']\n")
        (vm / 'meta-data').write_text('instance-id: cluster-' + role + '\n')
        net = 'version: 2\nethernets:\n'
        args = []
        for index, (segment, host, peer) in enumerate(links):
            mac = f'52:54:00:bc:{segment.zfill(2)}:{host.zfill(2)}'
            # Kernel TAP/vhost avoids saturating QEMU's userspace packet copier.
            tap=role+str(index)
            run('ip','tuntap','add','dev',tap,'mode','tap')
            run('ip','link','set',tap,'master','lan'+segment)
            run('ip','link','set',tap,'up')
            args += ['-netdev', f'tap,id=n{index},ifname={tap},script=no,downscript=no,vhost=on',
                     '-device', f'virtio-net-pci,netdev=n{index},mac={mac}']
            net += f'''  eth{index}:
    match:
      macaddress: '{mac}'
    set-name: eth{index}
    dhcp4: false
    dhcp6: false
    accept-ra: false
    addresses: ['198.18.{segment}.{host}/24', 'fd42:bc:{segment}::{host}/64']
'''
            if role in ('client', 'node', 'target') and index == 0:
                gateway = {'client':'1', 'node':'1', 'target':'1'}[role]
                net += f'''    routes:
      - to: default
        via: 198.18.{segment}.{gateway}
      - to: '::/0'
        via: 'fd42:bc:{segment}::{gateway}'
'''
        (vm / 'network-config').write_text(net)
        run('qemu-img', 'create', '-f', 'qcow2', '-F', 'qcow2', '-b', '/base.img', vm / 'root.qcow2', '8G')
        run('cloud-localds', '--network-config='+str(vm/'network-config'), vm/'seed.img', vm/'user-data', vm/'meta-data')
        command = ['qemu-system-x86_64', '-accel', 'kvm', '-cpu', 'host', '-smp', str(cpus), '-m', str(memory), '-display', 'none',
                   '-serial', f'file:{ROOT}/results/{role}/serial.log', '-qmp', f'unix:{vm}/qmp.sock,server=on,wait=off',
                   '-drive', f'file={vm}/root.qcow2,if=virtio,format=qcow2', '-drive', f'file={vm}/seed.img,if=virtio,format=raw,readonly=on',
                   '-fsdev', 'local,id=lab,path=/lab,security_model=none', '-device', 'virtio-9p-pci,fsdev=lab,mount_tag=lab', *args]
        (vm / 'command.json').write_text(json.dumps(command, indent=2))
        log = (ROOT/'results'/role/'qemu.log').open('wb')
        CHILDREN.append(subprocess.Popen(command, stdout=log, stderr=log)); log.close()
    time.sleep(2)
    proof = {role: qmp(ROOT/('vm-'+role)/'qmp.sock') for role in NODES}
    (ROOT/'results'/'kvm.json').write_text(json.dumps(proof, indent=2))
    deadline=time.monotonic()+14400
    while time.monotonic()<deadline:
        if any(p.poll() is not None for p in CHILDREN): raise RuntimeError('QEMU exited early')
        time.sleep(2)
    raise TimeoutError('offline cluster lifetime reached four hours')


if __name__ == '__main__':
    try: main()
    except Exception:
        (ROOT/'results'/'hypervisor-failure.txt').write_text(traceback.format_exc())
        cleanup()
