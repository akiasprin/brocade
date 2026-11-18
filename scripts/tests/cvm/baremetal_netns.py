#!/usr/bin/env python3
"""Exercise native networking in private user, mount and network namespaces only."""

import argparse
import os
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('agent_tests', type=Path)
    args = parser.parse_args()
    binary = args.agent_tests.resolve(strict=True)
    environment = dict(os.environ, BROCADE_RUN_BAREMETAL_NETNS='1')
    for kind in ('user', 'mnt', 'net'):
        environment['BROCADE_TEST_PARENT_NS_' + kind.upper()] = os.readlink('/proc/self/ns/' + kind)
    test = 'cvm::host::network::tests::native_network_namespace_convergence'
    result = subprocess.run(
        [
            'unshare',
            '-Urnm',
            '/bin/sh',
            '-c',
            'mount --make-rprivate / && mount -t tmpfs tmpfs /run && exec "$@"',
            'native-network-test',
            str(binary),
            test,
            '--ignored',
            '--exact',
            '--nocapture',
        ],
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    print(result.stdout, end='')
    if 'running 1 test' not in result.stdout or '1 passed; 0 failed' not in result.stdout:
        return result.returncode or 1
    return result.returncode


if __name__ == '__main__':
    sys.exit(main())
