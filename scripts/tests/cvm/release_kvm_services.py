#!/usr/bin/env python3
"""Install prerequisites only inside the VMs owned by release_kvm_lab.py."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import json
from pathlib import Path
import time

from release_kvm_lab import Lab


def prepare(lab, output):
    output.mkdir(mode=0o700, exist_ok=True)
    started = datetime.now(timezone.utc).isoformat()
    mono = time.monotonic()
    results = []

    def install(role):
        before = time.monotonic()
        packages = 'python3 curl ca-certificates iproute2 iputils-ping'
        if role != 'client':
            packages += ' nftables wireguard-tools conntrack socat libcap-ng0 libseccomp2 e2fsprogs xfsprogs bpftool'
        else:
            packages += ' tcpdump'
        if role == 'compute':
            packages += ' postgresql'
        script = (
            'sudo env DEBIAN_FRONTEND=noninteractive apt-get update && '
            'sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ' + packages
        )
        result = lab.remote(role, script, timeout=600, check=False)
        path = output / (role + '-packages.log')
        path.write_bytes(result.stdout + result.stderr)
        path.chmod(0o600)
        if result.returncode:
            raise RuntimeError(f'{role} packages failed; see private package log')
        if role != 'client':
            lab.remote(
                role,
                'sudo modprobe kvm_intel && sudo modprobe tun && '
                'sudo sysctl -w net.ipv4.ip_forward=1 net.ipv6.conf.all.forwarding=1',
            )
            proof = json.loads(
                lab.python(
                    role,
                    "import os,fcntl,json;f=os.open('/dev/kvm',os.O_RDWR|os.O_CLOEXEC);v=fcntl.ioctl(f,0xAE01,0);print(json.dumps({'nested_kvm':True,'api':fcntl.ioctl(f,0xAE00,0)}));os.close(v);os.close(f)",
                )
            )
        else:
            proof = {}
        item = {
            'role': role,
            'seconds': time.monotonic() - before,
            'packages': packages,
            'completed_at': datetime.now(timezone.utc).isoformat(),
            **proof,
        }
        print('PASS: ' + role + ' OS prerequisites' + (' and nested KVM' if proof else ''), flush=True)
        return item

    with ThreadPoolExecutor(max_workers=3) as pool:
        for item in pool.map(install, lab.state['roles']):
            results.append(item)
    value = {
        'start': started,
        'end': datetime.now(timezone.utc).isoformat(),
        'seconds': time.monotonic() - mono,
        'roles': results,
    }
    (output / 'prerequisites.json').write_text(json.dumps(value, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--lab', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    prepare(Lab(args.lab), args.output.resolve())


if __name__ == '__main__':
    main()
