#!/usr/bin/env python3
"""Run the actual Agent HTTP fixture without a route to the Internet or host changes."""

import os
import pathlib
import subprocess
import sys
import tempfile

if len(sys.argv) == 2:
    executable = str(pathlib.Path(sys.argv[1]).resolve(strict=True))
    subprocess.run(
        [
            'unshare',
            '--user',
            '--map-root-user',
            '--net',
            '--mount',
            sys.executable,
            str(pathlib.Path(__file__).resolve()),
            '--inside',
            executable,
        ],
        check=True,
    )
elif len(sys.argv) == 3 and sys.argv[1] == '--inside':
    # Keep deterministic dual-stack DNS in the fixture's mount namespace. These
    # bindings and all socket traffic are invisible to the host network.
    subprocess.run(['mount', '--make-rprivate', '/'], check=True)
    hosts = tempfile.TemporaryDirectory(prefix='brocade-maintenance-hosts-')
    hosts_file = pathlib.Path(hosts.name) / 'hosts'
    hosts_file.write_text(
        '127.0.0.1 localhost mixed.maintenance.test\n'
        '::1 localhost mixed.maintenance.test\n'
        '11.0.0.2 dualstack.maintenance.test mixed.maintenance.test opposite-family.maintenance.test\n'
        '2001:470:ffff:abcd::2 dualstack.maintenance.test mixed.maintenance.test ipv6-only.maintenance.test\n'
        '11.0.0.2 www.youtube.com music.youtube.com www.google.com\n'
        '2001:470:ffff:abcd::2 www.youtube.com music.youtube.com www.google.com\n'
    )
    subprocess.run(['mount', '--bind', str(hosts_file), '/etc/hosts'], check=True)
    for command in [
        ['ip', 'link', 'set', 'lo', 'up'],
        ['ip', 'addr', 'add', '11.0.0.1/32', 'dev', 'lo'],
        ['ip', 'addr', 'add', '11.0.0.2/32', 'dev', 'lo'],
        ['ip', '-6', 'addr', 'add', '2001:470:ffff:abcd::1/128', 'dev', 'lo', 'nodad'],
        ['ip', '-6', 'addr', 'add', '2001:470:ffff:abcd::2/128', 'dev', 'lo', 'nodad'],
    ]:
        subprocess.run(command, check=True)
    for family in [[], ['-6']]:
        if subprocess.check_output(['ip', *family, 'route', 'show', 'default']).strip():
            raise RuntimeError('Fixture must have no Internet default route')
    environment = os.environ.copy()
    environment['BROCADE_MAINTENANCE_NETNS_FIXTURE'] = '1'
    result = subprocess.run(
        [
            sys.argv[2],
            '--ignored',
            '--exact',
            'ip_maintenance::worker::netns_tests::ip_maintenance_real_http_source_cookie_redirect_gzip_and_cancellation',
            '--nocapture',
        ],
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    print(result.stdout, end='')
    hosts.cleanup()
    if result.returncode != 0 or '1 passed; 0 failed' not in result.stdout:
        raise SystemExit(1)
else:
    raise SystemExit('usage: maintenance-http-netns.py /absolute/agent-test-binary')
