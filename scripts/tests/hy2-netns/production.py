#!/usr/bin/env python3
"""Drive the real Agent restart path and real Xray CLI in a disposable lab."""

import concurrent.futures
import json
import os
import pathlib
import re
import ipaddress
import struct
import select
import socket
import subprocess
import sys
import threading
import time
import traceback
import uuid

from workload import CLIENT, PUBLIC, at, check, namespace, run
from hy2 import ct, stop

SELF = str(pathlib.Path(__file__).resolve())
INPUTS = pathlib.Path(__file__).resolve().parent


def tuples(rows):
    return {(re.search(r'src=(\S+) dst=(\S+) sport=(\d+) dport=(\d+)', row).groups()) for row in rows}


def wait_socks(pid, process):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        check(process.poll() is None, 'Xray client exited during startup')
        if ':1080 ' in at(pid, 'ss', '-H', '-lnt'):
            return
        time.sleep(0.05)
    raise TimeoutError('client SOCKS listener did not start')


def exercise(client_pid, output, version, port, hopping=False, before_stop=None):
    label = f'ipv{version}-' + ('hopping' if hopping else str(port))
    auth = 'hy2-netns-disposable-test'
    config = {
        'log': {'loglevel': 'info'},
        'inbounds': [{'listen': '127.0.0.1', 'port': 1080, 'protocol': 'socks', 'settings': {'udp': True}}],
        'outbounds': [
            {
                'protocol': 'hysteria',
                'settings': {'version': 2, 'address': PUBLIC[version], 'port': port},
                'streamSettings': {
                    'network': 'hysteria',
                    'security': 'tls',
                    'hysteriaSettings': {'version': 2, 'auth': auth},
                    'tlsSettings': {
                        'serverName': 'cover.test',
                        'certificates': [{'usage': 'verify', 'certificateFile': str(INPUTS / 'cover.pem')}],
                    },
                },
            }
        ],
    }
    if hopping:
        start = 30000 if version == 4 else 30100
        config['outbounds'][0]['streamSettings']['finalMask'] = {
            'quicParams': {'udpHop': {'ports': f'{start}-{start + 99}', 'interval': 5}}
        }
    path = output / (label + '.json')
    path.write_text(json.dumps(config))
    with (output / (label + '.log')).open('wb') as log:
        process = subprocess.Popen(
            ['nsenter', f'--net=/proc/{client_pid}/ns/net', '--', str(INPUTS / 'xray'), 'run', '-config', str(path)],
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        try:
            wait_socks(client_pid, process)
            record = json.loads(
                at(client_pid, sys.executable, str(INPUTS / 'hy2.py'), '--request', version, int(hopping), timeout=45)
            )
            if before_stop:
                before_stop()
                # The existing QUIC connection must still carry data after PMTU changes.
                at(client_pid, sys.executable, str(INPUTS / 'hy2.py'), '--request', version, 0, timeout=25)
            return record
        finally:
            stop(process)


def inside(output):
    started = time.monotonic()
    ancestor = int(os.environ['BROCADE_HY2_LAB_ANCESTOR_FD'])
    check(
        os.readlink(f'/proc/self/fd/{ancestor}') != os.readlink('/proc/self/ns/net'),
        'caller network namespace is not isolated',
    )
    check([name for _, name in socket.if_nameindex()] == ['lo'], 'lab requires a fresh network namespace')
    output.mkdir(parents=True, exist_ok=True)
    # Docker mounts /proc/sys read-only independently of NET_ADMIN. Only this
    # disposable mount namespace makes namespaced sysctls writable for the test.
    if any(
        parts[4] == '/proc/sys' and 'ro' in parts[5].split(',')
        for parts in (line.split() for line in pathlib.Path('/proc/self/mountinfo').read_text().splitlines())
    ):
        run('mount', '-o', 'remount,rw', '/proc/sys')
    cases, baseline, children = [], set(), []
    result = {'passed': False, 'cases': cases, 'kernel': run('uname', '-r')}
    state_dir = pathlib.Path('/var/tmp') / ('bh2-lab-' + uuid.uuid4().hex[:8])
    stop_control = threading.Event()
    server_socket = None
    native = None
    trace = None
    trace_log = None
    try:
        client = namespace()
        children.append(client)
        run('ip', 'link', 'set', 'lo', 'up')
        run('ip', 'link', 'add', 'wan', 'type', 'veth', 'peer', 'name', 'client')
        run('ip', 'link', 'set', 'client', 'netns', client.pid)
        for prefix, interface, v4, v6 in (
            (None, 'wan', PUBLIC[4], PUBLIC[6]),
            (client.pid, 'client', CLIENT[4], CLIENT[6]),
        ):
            execute = (lambda *args: at(prefix, *args)) if prefix else run
            execute('ip', 'link', 'set', 'lo', 'up')
            execute('ip', 'addr', 'add', v4 + '/24', 'dev', interface)
            execute('ip', '-6', 'addr', 'add', v6 + '/64', 'dev', interface, 'nodad')
            execute('ip', 'link', 'set', interface, 'up')
        # Exercise the existing ingress->IFB path; the production feature must not
        # install competing TC filters or bypass its queue.
        run('ip', 'link', 'add', 'labifb', 'type', 'ifb')
        run('ip', 'link', 'set', 'labifb', 'up')
        run('tc', 'qdisc', 'add', 'dev', 'labifb', 'root', 'tbf', 'rate', '20mbit', 'burst', '64k', 'latency', '500ms')
        run('tc', 'qdisc', 'add', 'dev', 'wan', 'clsact')
        run(
            'tc',
            'filter',
            'add',
            'dev',
            'wan',
            'ingress',
            'pref',
            '49152',
            'handle',
            '0xbcd0',
            'matchall',
            'action',
            'mirred',
            'egress',
            'redirect',
            'dev',
            'labifb',
        )
        # Reproduce the real public egress path, including fq's default horizon
        # guard. Capturing ingress assigns RX wall-clock timestamps; a raw dup
        # must not carry those into fq as monotonic transmit deadlines.
        run(
            'tc',
            'qdisc',
            'add',
            'dev',
            'wan',
            'root',
            'handle',
            '1:',
            'tbf',
            'rate',
            '1gbit',
            'burst',
            '1m',
            'latency',
            '50ms',
        )
        run('tc', 'qdisc', 'add', 'dev', 'wan', 'parent', '1:1', 'handle', '2:', 'fq')
        public_qdisc = json.loads(run('tc', '-j', 'qdisc', 'show', 'dev', 'wan'))
        target = subprocess.Popen(
            ['nsenter', f'--net=/proc/{client.pid}/ns/net', '--', sys.executable, str(INPUTS / 'hy2.py'), '--target'],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        children.append(target)
        check(
            bool(select.select([target.stdout], [], [], 5)[0]) and target.stdout.readline().strip() == 'ready',
            'echo target did not start',
        )
        server_socket = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
        server_socket.bind(('::', 18082))
        server_socket.listen(16)
        server_socket.settimeout(0.2)

        def control_server():
            while not stop_control.is_set():
                try:
                    connection, _ = server_socket.accept()
                    with connection:
                        connection.settimeout(2)
                        connection.sendall(connection.recv(32))
                except socket.timeout:
                    pass

        worker = threading.Thread(target=control_server)
        worker.start()
        env = os.environ.copy()
        env.update(
            BROCADE_HY2_LAB_RESULTS=str(output),
            BROCADE_HY2_LAB_INPUTS=str(INPUTS),
            BROCADE_TEST_XRAY=str(INPUTS / 'xray'),
            BROCADE_HY2_LAB_STATE=str(state_dir),
        )
        if (INPUTS / 'agent').is_file():
            env['BROCADE_HY2_RELEASE_AGENT'] = str(INPUTS / 'agent')
        with (output / 'vpn-snat-tests.log').open('wb') as snat_log:
            snat = subprocess.run(
                [
                    str(INPUTS / 'agent-tests'),
                    '--exact',
                    'vpngate::tests::namespace_snat_is_peer_scoped_and_converges_prior_rules',
                    '--ignored',
                    '--nocapture',
                    '--test-threads=1',
                ],
                env=env,
                pass_fds=(ancestor,),
                stdout=snat_log,
                stderr=subprocess.STDOUT,
                timeout=45,
            )
        check(
            snat.returncode == 0 and 'test result: ok. 1 passed;' in (output / 'vpn-snat-tests.log').read_text(),
            'VPN peer-only source NAT or migration failed or matched no test; see vpn-snat-tests.log',
        )
        cases.append({'case': 'vpn-peer-source-nat-and-existing-rule-migration', 'passed': True})
        # The standalone VPN test owns these otherwise empty iptables base
        # chains. Keeping a NAT base chain enables host conntrack hooks even
        # without rules; remove only its verified-empty fixture tables.
        for table in ('nat', 'filter'):
            objects = json.loads(run('nft', '-j', 'list', 'table', 'ip', table))['nftables']
            check(not any('rule' in item for item in objects), 'VPN fixture left firewall rules')
            run('nft', 'delete', 'table', 'ip', table)
        log = (output / 'native-tests.log').open('wb')
        native = subprocess.Popen(
            [
                str(INPUTS / 'agent-tests'),
                '--exact',
                'hy2_network::tests::native_restart_migration_recovery_and_reconcile',
                '--ignored',
                '--nocapture',
                '--test-threads=1',
            ],
            env=env,
            pass_fds=(ancestor,),
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        deadline, seen = time.monotonic() + (600 if os.environ.get('BROCADE_HY2_TCG') else 300), set()
        phases = [
            'legacy-ready',
            'isolated-ready',
            'reconciled-ready',
            'link-recovered-ready',
            'restarted-ready',
            'failed-change-recovered',
            'single-port-ready',
            're-enabled-ready',
            'rollback-ready',
            'ipv4-only-ready',
        ]
        if (INPUTS / 'legacy-xray').is_file():
            phases += ['downgrade-ready']
            if (INPUTS / 'legacy-agent').is_file():
                phases += ['old-agent-ready']
            phases += ['re-upgraded-ready']
        while native.poll() is None:
            check(time.monotonic() < deadline, 'native Agent restart test exceeded deadline')
            phase = phases[len(seen)] if len(seen) < len(phases) else None
            phase_file = output / f'phase-{phase}.json'
            if not phase_file.exists():
                time.sleep(0.05)
                continue
            check(json.loads(phase_file.read_text())['phase'] == phase, 'Agent phases arrived out of order')
            phase_dir = output / phase
            phase_dir.mkdir()
            isolated = phase in (
                'isolated-ready',
                'reconciled-ready',
                'link-recovered-ready',
                'restarted-ready',
                're-enabled-ready',
                'ipv4-only-ready',
                're-upgraded-ready',
            )
            state = json.loads((state_dir / 'hy2-network.json').read_text()) if isolated else None
            if state:
                (phase_dir / 'namespace-state.json').write_text(json.dumps(state, indent=2) + '\n')
            namespace_prefix = ['ip', 'netns', 'exec', state['namespace']] if state else []
            if phase == 'isolated-ready':
                trace_log = (output / 'ipv6-packets.log').open('wb')
                trace = subprocess.Popen(
                    [
                        'tcpdump',
                        '-i',
                        'any',
                        '-nn',
                        '-vv',
                        '-l',
                        '-s',
                        '128',
                        '-c',
                        '5000',
                        'ip6 and (icmp6 or udp portrange 30100-30199)',
                    ],
                    stdout=trace_log,
                    stderr=subprocess.STDOUT,
                )
                time.sleep(0.1)

            def scoped_rows(version, namespace=False):
                args = [
                    'conntrack',
                    '-L',
                    '-o',
                    'extended',
                    '-f',
                    'ipv4' if version == 4 else 'ipv6',
                    '-p',
                    'udp',
                    '--orig-src',
                    CLIENT[version],
                    '--orig-dst',
                    PUBLIC[version],
                ]
                completed = subprocess.run(
                    (namespace_prefix if namespace else []) + args,
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=5,
                )
                check(completed.returncode == 0, completed.stderr)
                return {row for row in tuples(completed.stdout.splitlines()) if 30000 <= int(row[3]) <= 30199}

            for version in (
                (4,)
                if phase in ('ipv4-only-ready', 'downgrade-ready', 'old-agent-ready', 're-upgraded-ready')
                else (6, 4)
            ):
                ports = (30000, 30037, 30099) if version == 4 else (30100, 30137, 30199)
                if phase != 'isolated-ready':
                    ports = (
                        (ports[0], ports[-1])
                        if phase == 'legacy-ready'
                        else ((ports[0],) if phase == 'single-port-ready' else (ports[1],))
                    )
                for port in (*ports, None) if phase == 'isolated-ready' else ports:
                    previous = scoped_rows(version, True) if isolated else set()
                    record = exercise(client.pid, phase_dir, version, port or ports[0], port is None)
                    host_rows = scoped_rows(version)
                    if isolated:
                        new_rows = scoped_rows(version, True) - previous
                        check(bool(new_rows), 'real HY2 flow was absent in the production namespace')
                        check(not (host_rows - baseline), 'new HY2 flow was tracked in the main namespace')
                        if port is None:
                            hops = sorted({int(row[3]) for row in new_rows})
                            check(len(hops) >= 2, 'live port hopping was not observed')
                            record['observed_hop_ports'] = hops
                    else:
                        if phase != 'single-port-ready':
                            check(bool(host_rows), 'legacy/rollback conntrack positive control missing')
                        baseline |= host_rows
                    cases.append(
                        {
                            'case': phase + f'-ipv{version}-' + ('hopping' if port is None else str(port)),
                            'passed': True,
                            'main_new_hy2_entries': 0 if isolated else None,
                            **record,
                        }
                    )
            if isolated:
                (phase_dir / 'namespace-sockets.txt').write_text(run(*namespace_prefix, 'ss', '-ulnp') + '\n')
                (phase_dir / 'namespace-conntrack.txt').write_text(
                    run(*namespace_prefix, 'conntrack', '-L', '-o', 'extended') + '\n'
                )
                (phase_dir / 'host-conntrack.txt').write_text(run('conntrack', '-L', '-o', 'extended') + '\n')
            if phase == 'isolated-ready':
                previous = scoped_rows(6, True)

                def inject_pmtu():
                    fresh = scoped_rows(6, True) - previous
                    check(len(fresh) == 1, 'PMTU test did not identify one fresh QUIC flow')
                    _, _, client_port, server_port = next(iter(fresh))
                    at(client.pid, sys.executable, SELF, '--icmp6-pmtu', client_port, server_port)
                    deadline = time.monotonic() + 3
                    observed = ''
                    while time.monotonic() < deadline:
                        observed = run(*namespace_prefix, 'ip', '-6', 'route', 'get', CLIENT[6])
                        if 'mtu 1280' in observed:
                            break
                        time.sleep(0.05)
                    check('mtu 1280' in observed, 'ICMPv6 PTB did not update the HY2 namespace PMTU')
                    (phase_dir / 'pmtu-route.txt').write_text(observed + '\n')

                exercise(client.pid, phase_dir, 6, 30137, before_stop=inject_pmtu)
                cases.append({'case': 'icmpv6-pmtu-delivery', 'passed': True})
                # Test the original no-other-NAT condition separately. The
                # unrelated source NAT coexistence path was exercised above.
                run('nft', 'delete', 'table', 'ip', 'hy2_lab_scope')
                run('nft', 'delete', 'table', 'ip', 'hy2_lab_accept')
                for version in (4, 6):
                    at(client.pid, sys.executable, SELF, '--tcp-control', version)
                    rows = ct(
                        None,
                        '-f',
                        'ipv4' if version == 4 else 'ipv6',
                        '-p',
                        'tcp',
                        '--orig-src',
                        CLIENT[version],
                        '--orig-dst',
                        PUBLIC[version],
                        '--sport',
                        str(52000 + version),
                        '--dport',
                        '18082',
                    )
                    check(not rows, 'HY2 isolation activated main-namespace TCP tracking')
                    cases.append({'case': f'isolated-host-tcp{version}-untracked', 'passed': True})
                run(
                    'nft',
                    '-f',
                    '-',
                    input='add table ip hy2_lab_scope\nadd chain ip hy2_lab_scope post { type nat hook postrouting priority srcnat; policy accept; }\nadd rule ip hy2_lab_scope post ip saddr 169.254.240.2 masquerade\nadd table ip hy2_lab_accept\nadd chain ip hy2_lab_accept forward { type filter hook forward priority filter; policy accept; }\nadd rule ip hy2_lab_accept forward ct state established,related accept\n',
                )
                for address, port, source_port, label in (
                    (PUBLIC[4], 18083, 53004, 'ordinary-local-udp'),
                    ('127.0.0.1', 30037, 53005, 'loopback-hy2-range-udp'),
                ):
                    with (
                        socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as server,
                        socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as caller,
                    ):
                        server.bind((address, port))
                        server.settimeout(2)
                        caller.bind((address, source_port))
                        caller.settimeout(2)
                        caller.sendto(b'tracking-scope-control', (address, port))
                        payload, peer = server.recvfrom(128)
                        server.sendto(payload, peer)
                        check(caller.recv(128) == b'tracking-scope-control', label + ' did not carry data')
                        rows = ct(
                            None,
                            '-f',
                            'ipv4',
                            '-p',
                            'udp',
                            '--orig-src',
                            address,
                            '--orig-dst',
                            address,
                            '--sport',
                            str(source_port),
                            '--dport',
                            str(port),
                        )
                        check(bool(rows), label + ' was incorrectly exempted from host tracking')
                    cases.append({'case': label + '-tracked', 'passed': True})
                local = phase_dir / 'self-probe'
                local.mkdir()
                for version, port in ((4, 30037), (6, 30137)):
                    record = exercise(os.getpid(), local, version, port)
                    rows = ct(
                        None,
                        '-f',
                        'ipv4' if version == 4 else 'ipv6',
                        '-p',
                        'udp',
                        '--orig-src',
                        PUBLIC[version],
                        '--orig-dst',
                        PUBLIC[version],
                        '--dport',
                        str(port),
                    )
                    check(not rows, 'local public-address HY2 probe was tracked in the main namespace')
                    cases.append({'case': f'isolated-local-public-ipv{version}-probe', 'passed': True, **record})
            seen.add(phase)
            (output / f'ack-{phase}').write_text(phase)
            print(
                json.dumps(
                    {
                        'phase': phase,
                        'cases_passed': len(cases),
                        'elapsed_seconds': round(time.monotonic() - started, 2),
                    }
                ),
                flush=True,
            )
        check(native.returncode == 0, 'native Agent restart path failed; see native-tests.log and xray.log')
        log.close()
        check(seen == set(phases), 'native test did not execute every migration phase')
        native_result = json.loads((output / 'native-result.json').read_text())
        check(native_result['passed'], 'native Agent result was not successful')
        for key in (
            'custom_source_policy_coexists',
            'existing_nat_coexists',
            'foreign_listener_rejected',
            'foreign_range_rejected_before_stop',
            'ipv4_only_host_verified',
            'reserved_ports_preserved',
            'ephemeral_allocator_avoids_hy2',
            'host_udp_timeouts_preserved',
            'readonly_host_udp_recovery_verified',
        ):
            check(native_result.get(key) is True, key + ' was not verified')
        if (INPUTS / 'agent').is_file():
            check(
                native_result.get('release_agent_cli_restart_verified') is True,
                'actual release Agent CLI was not verified',
            )
        for file, key in [
            ('legacy-xray', 'actual_legacy_xray_roundtrip'),
            ('legacy-agent', 'actual_legacy_agent_roundtrip'),
        ]:
            if (INPUTS / file).is_file():
                check(native_result.get(key) is True, 'actual legacy byte roundtrip was not verified')
        if (INPUTS / 'legacy-xray').is_file() and (INPUTS / 'agent').is_file():
            check(
                native_result.get('readonly_host_udp_legacy_cli') is True,
                'legacy CLI with read-only host UDP timeouts was not verified',
            )
        queue = json.loads(run('tc', '-j', '-s', 'qdisc', 'show', 'dev', 'labifb'))
        check(
            any(q.get('kind') == 'tbf' and q.get('bytes', 0) > 1024 * 1024 for q in queue), 'HY2 bypassed the IFB queue'
        )
        cases.append({'case': 'ifb-shaping-preserved', 'passed': True})
        (output / 'ifb-qdisc.json').write_text(json.dumps(queue, indent=2) + '\n')
        check(
            json.loads(run('tc', '-j', 'qdisc', 'show', 'dev', 'wan')) == public_qdisc,
            'public egress queue configuration changed',
        )
        egress_queue = json.loads(run('tc', '-j', '-s', 'qdisc', 'show', 'dev', 'wan'))
        fq = next(q for q in egress_queue if q['kind'] == 'fq')
        check(
            fq.get('bytes', 0) > 1024 * 1024 and fq.get('horizon_drops', 0) == 0,
            'public fq did not carry data or dropped RX timestamps as future deadlines',
        )
        (output / 'public-egress-qdisc.json').write_text(json.dumps(egress_queue, indent=2) + '\n')
        cases.append({'case': 'public-tbf-fq-egress-with-rx-timestamps', 'passed': True})
        result.update(passed=True, native=native_result)
    except Exception:
        result['failure'] = traceback.format_exc()
        try:
            (output / 'failed-host-nft.txt').write_text(run('nft', 'list', 'ruleset'))
            (output / 'failed-public-qdisc.json').write_text(run('tc', '-j', '-s', 'qdisc', 'show', 'dev', 'wan'))
            (output / 'failed-host-neighbors.txt').write_text(run('ip', '-6', 'neigh', 'show'))
            state = json.loads((state_dir / 'hy2-network.json').read_text())
            prefix = ['ip', 'netns', 'exec', state['namespace']]
            for name, args in {
                'neighbors': ['ip', '-6', 'neigh', 'show'],
                'routes': ['ip', '-6', 'route', 'show'],
                'rules': ['nft', 'list', 'ruleset'],
                'sockets': ['ss', '-ulnp'],
                'conntrack': ['conntrack', '-L', '-o', 'extended'],
            }.items():
                (output / ('failed-namespace-' + name + '.txt')).write_text(run(*prefix, *args))
        except Exception as error:
            result['failure_collection_error'] = str(error)
        raise
    finally:
        if trace is not None:
            stop(trace)
        if trace_log is not None:
            trace_log.close()
        if native is not None:
            stop(native)
        stop_control.set()
        if 'worker' in locals():
            worker.join(timeout=3)
        if server_socket is not None:
            server_socket.close()
        for child in reversed(children):
            stop(child)
        result['elapsed_seconds'] = round(time.monotonic() - started, 3)
        (output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')


def main():
    if sys.argv[1:2] == ['--inside']:
        inside(pathlib.Path(sys.argv[2]))
    elif sys.argv[1:2] == ['--hold']:
        print(os.readlink('/proc/self/ns/net'), flush=True)
        sys.stdin.read(1)
    elif sys.argv[1:2] == ['--tcp-control']:
        version = int(sys.argv[2])
        family = socket.AF_INET if version == 4 else socket.AF_INET6
        with socket.socket(family, socket.SOCK_STREAM) as sock:
            sock.settimeout(3)
            sock.bind((CLIENT[version], 52000 + version))
            sock.connect((PUBLIC[version], 18082))
            sock.sendall(b'scope-control')
            check(sock.recv(32) == b'scope-control', 'host TCP control failed')
    elif sys.argv[1:2] == ['--icmp6-pmtu']:
        client_port, server_port = map(int, sys.argv[2:])
        quoted = struct.pack(
            '!IHBB16s16s',
            6 << 28,
            8,
            socket.IPPROTO_UDP,
            64,
            ipaddress.ip_address(PUBLIC[6]).packed,
            ipaddress.ip_address(CLIENT[6]).packed,
        )
        quoted += struct.pack('!HHHH', server_port, client_port, 8, 0)
        packet = struct.pack('!BBHI', 2, 0, 0, 1280) + quoted
        with socket.socket(socket.AF_INET6, socket.SOCK_RAW, socket.IPPROTO_ICMPV6) as sock:
            sock.sendto(packet, (PUBLIC[6], 0))
    elif len(sys.argv) == 2:
        fd = os.open('/proc/self/ns/net', os.O_RDONLY)
        env = dict(os.environ, BROCADE_HY2_LAB_ANCESTOR_FD=str(fd))
        try:
            return subprocess.run(
                ['unshare', '--mount', '--net', '--', sys.executable, SELF, '--inside', sys.argv[1]],
                env=env,
                pass_fds=(fd,),
                timeout=660 if os.environ.get('BROCADE_HY2_TCG') else 340,
            ).returncode
        finally:
            os.close(fd)
    else:
        raise RuntimeError('expected result directory')
    return 0


if __name__ == '__main__':
    sys.exit(main())
