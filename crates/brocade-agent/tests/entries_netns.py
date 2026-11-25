#!/usr/bin/env python3
"""End-to-end entry data path in private user/mount/network namespaces.

Usage: python3 crates/brocade-agent/tests/entries_netns.py TEST_BINARY

Topology (all private): client -- entry machine == WireGuard underlay == container
host -- "cube-dev" -- guest. The entry machine and the host each run one Agent
convergence of a fixture desired state. Nothing on the real host is modified.
"""

import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time

BINARY = None
WORK = None


def sh(*args, ns=None, check=True, timeout=20, capture=True):
    command = (['ip', 'netns', 'exec', ns] if ns else []) + list(args)
    result = subprocess.run(command, check=False, timeout=timeout, capture_output=capture, text=True)
    if check and result.returncode != 0:
        raise RuntimeError(f'{" ".join(command)} failed: {result.stderr.strip()}')
    return result


def converge(ns, desired, root):
    path = os.path.join(WORK, f'{ns}-desired.json')
    with open(path, 'w') as handle:
        json.dump(desired, handle)
    env = dict(os.environ, BROCADE_ENTRY_FIXTURE=path, BROCADE_ENTRY_ROOT=root)
    result = subprocess.run(
        [
            'ip',
            'netns',
            'exec',
            ns,
            BINARY,
            '--ignored',
            '--exact',
            'cvm::entries::tests::converge_fixture',
            '--nocapture',
            '--test-threads',
            '1',
        ],
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    for line in result.stdout.splitlines():
        if 'ENTRY-REPORT ' in line:
            return json.loads(line.split('ENTRY-REPORT ', 1)[1])
    raise RuntimeError(f'no report from {ns}: {result.stdout}\n{result.stderr}')


def keypair():
    private = sh('wg', 'genkey').stdout.strip()
    public = subprocess.run(['wg', 'pubkey'], input=private, capture_output=True, text=True, check=True).stdout.strip()
    return private, public


GUEST = r"""
import socket, threading, sys
def tcp(address, port):
    server = socket.socket(); server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind((address, port)); server.listen(16)
    while True:
        connection, peer = server.accept()
        connection.sendall(f"{peer[0]} {address}:{port}".encode()); connection.close()
def udp(address, port):
    server = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); server.bind((address, port))
    while True:
        data, peer = server.recvfrom(2048); server.sendto(data + f" {peer[0]}".encode(), peer)
def echo(address):
    def connection(client):
        try:
            while data := client.recv(2048): client.sendall(data)
        finally: client.close()
    server = socket.socket(); server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind((address, 8030)); server.listen(16)
    while True: threading.Thread(target=connection, args=(server.accept()[0],), daemon=True).start()
for target in (lambda: tcp("192.168.0.7", 8022), lambda: udp("192.168.0.7", 8022),
               lambda: tcp("192.168.0.9", 80), lambda: tcp("192.168.0.8", 8022),
               lambda: echo("192.168.0.8"), lambda: udp("192.168.0.8", 8030)):
    threading.Thread(target=target, daemon=True).start()
print("ready", flush=True)
threading.Event().wait()
"""


def tcp(ns, address, port, timeout=3):
    code = (
        f"import socket;s=socket.create_connection(('{address}',{port}),timeout={timeout});print(s.recv(200).decode())"
    )
    result = sh('python3', '-c', code, ns=ns, check=False, timeout=timeout + 5)
    return result.stdout.strip() if result.returncode == 0 else None


def udp(ns, address, port):
    code = (
        f'import socket;s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.settimeout(3);'
        f"s.sendto(b'probe',('{address}',{port}));print(s.recv(200).decode())"
    )
    result = sh('python3', '-c', code, ns=ns, check=False, timeout=10)
    return result.stdout.strip() if result.returncode == 0 else None


def tunnel(slot, side, entry, target, key, peer, endpoint):
    return {
        'id': f'entry-tunnel-{slot}',
        'side': side,
        'entry_node': entry,
        'target_node': target,
        'wireguard_transport': 'udp',
        'offer': {
            'listen_side': 'provider',
            'kind': 'wireguard',
            'endpoint': endpoint,
            'port_start': 40000,
            'port_end': 40010,
            'mtu': 1420,
        },
        'port': 40000 + slot,
        'slot': slot,
        'credential': key,
        'peer_credential': peer,
    }


PRIMARY_SERVICES = r"""
import socket, threading, sys
address, label = sys.argv[1:]
def tcp(port):
    s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind((address,port));s.listen(8)
    while True:
        c,peer=s.accept();c.sendall(label.encode());c.close()
def udp(port):
    s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind((address,port))
    while True:
        data,peer=s.recvfrom(2048);s.sendto(label.encode(),peer)
for fn,port in [(tcp,p) for p in (22,9090,9091,9095,9099,30104,30106,30107)]+[(udp,9090),(udp,22)]: threading.Thread(target=fn,args=(port,),daemon=True).start()
print('ready',flush=True);threading.Event().wait()
"""


def primary_l3(edge, host, roots):
    servers = []
    try:
        for ns, addr, label in [
            ('edge', '198.51.100.1', 'EDGE'),
            ('host', '198.51.100.2', 'NATIVE'),
            ('client', '203.0.113.100', 'CONTROL'),
        ]:
            p = subprocess.Popen(
                ['ip', 'netns', 'exec', ns, 'python3', '-u', '-c', PRIMARY_SERVICES, addr, label],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            servers.append(p)
            assert p.stdout.readline().strip() == 'ready'
        sh('ip', 'route', 'replace', '198.51.100.1/32', 'via', '203.0.113.1', ns='client')
        host = json.loads(json.dumps(host))
        host['generation'] = 5
        host['deliveries'].append(
            {
                'id': 'native',
                'instance_id': 'baremetal',
                'mode': {'kind': 'l3'},
                'target': '198.51.100.2',
                'tunnel': 'entry-tunnel-3',
                'host_local': True,
            }
        )
        assert converge('host', host, roots['host'])['error'] is None
        edge = json.loads(json.dumps(edge))
        edge['generation'] = 5
        primary = {
            'id': 'native',
            'address': '198.51.100.1',
            'mode': {'kind': 'l3'},
            'target': '198.51.100.2',
            'tunnel': 'entry-tunnel-3',
            'host_local': False,
        }
        edge['rules'].append(primary)
        assert converge('edge', edge, roots['edge'])['error'] is None
        assert tcp('client', '198.51.100.1', 22) == 'NATIVE'
        assert udp('client', '198.51.100.1', 9090) == 'NATIVE'
        sh('ping', '-c', '1', '-W', '3', '198.51.100.1', ns='client')

        def control():
            for kind in ['tcp', 'udp']:
                code = (
                    "import socket;s=socket.socket(socket.AF_INET,socket.SOCK_STREAM if %r=='tcp' else socket.SOCK_DGRAM);s.settimeout(3);s.bind(('198.51.100.1',0));s.connect(('203.0.113.100',9090));"
                    % kind
                )
                code += "s.send(b'probe');" if kind == 'udp' else ''
                code += 'print(s.recv(100).decode())'
                assert sh('python3', '-c', code, ns='edge').stdout.strip() == 'CONTROL'
            sh('ping', '-c', '1', '-W', '3', '-I', '198.51.100.1', '203.0.113.100', ns='edge')

        control()
        # Re-convergence must preserve the handshake on the very IP being forwarded.
        for _ in range(2):
            report = converge('edge', edge, roots['edge'])
            assert report['error'] is None and all(t['connected'] for t in report['tunnels']), report
            assert tcp('client', '198.51.100.1', 22) == 'NATIVE'
        primary['target'] = None
        edge['generation'] += 1
        assert converge('edge', edge, roots['edge'])['error'] is None
        assert tcp('client', '198.51.100.1', 22, timeout=2) is None
        control()
        edge['rules'].remove(primary)
        edge['generation'] += 1
        assert converge('edge', edge, roots['edge'])['error'] is None
        assert tcp('client', '198.51.100.1', 22) == 'EDGE'
        # Protocol policies preserve host SSH while mapping a separate public port to guest SSH.
        all_ports = {'kind': 'forward', 'ports': {'kind': 'all'}, 'mappings': []}
        mode = {
            'kind': 'protocols',
            'tcp': dict(
                all_ports,
                mappings=[
                    {'listen': {'start': 30022, 'end': 30022}, 'target': {'start': 22, 'end': 22}},
                    {'listen': {'start': 30100, 'end': 30109}, 'target': {'start': 9090, 'end': 9099}},
                ],
            ),
            'udp': all_ports,
            'icmp': True,
            'other_ip': True,
        }
        reserved = {
            'tcp': [{'start': 22, 'end': 22}, {'start': 30104, 'end': 30104}, {'start': 30106, 'end': 30107}],
            'udp': [],
        }
        primary.update(mode=mode, reserved_ports=reserved, target='198.51.100.2')
        edge['rules'].append(primary)
        edge['generation'] += 1
        native = next(d for d in host['deliveries'] if d['id'] == 'native')
        native.update(mode=mode, reserved_ports=reserved)
        host['generation'] += 1
        assert converge('host', host, roots['host'])['error'] is None
        assert converge('edge', edge, roots['edge'])['error'] is None
        for port in (22, 30104, 30106, 30107):
            assert tcp('client', '198.51.100.1', port) == 'EDGE', port
        for port in (30022, 30100, 30101, 30105, 30109):
            assert tcp('client', '198.51.100.1', port) == 'NATIVE', port
        assert udp('client', '198.51.100.1', 22) == 'NATIVE'
        control()
        sh('ping', '-c', '1', '-W', '3', '198.51.100.1', ns='client')
        # Reservations are scoped to an address: the same port of another entry still forwards.
        sh('ip', 'address', 'add', '203.0.113.31/32', 'dev', 'lo', ns='edge')
        another = dict(primary, id='another', address='203.0.113.31', reserved_ports={'tcp': [], 'udp': []})
        edge['rules'].append(another)
        edge['generation'] += 1
        assert converge('edge', edge, roots['edge'])['error'] is None
        assert tcp('client', '203.0.113.31', 22) == 'NATIVE'
        primary['target'] = None
        edge['generation'] += 1
        assert converge('edge', edge, roots['edge'])['error'] is None
        assert tcp('client', '198.51.100.1', 22) == 'EDGE'
        assert tcp('client', '198.51.100.1', 30022, timeout=2) is None
        assert tcp('client', '198.51.100.1', 9090, timeout=2) is None
        control()
        edge['rules'].remove(primary)
        edge['rules'].remove(another)
        edge['generation'] += 1
        assert converge('edge', edge, roots['edge'])['error'] is None
        assert tcp('client', '198.51.100.1', 22) == 'EDGE'
        primary['target'] = '198.51.100.2'
        edge['rules'].append(primary)
        edge['generation'] += 1
        assert converge('edge', edge, roots['edge'])['error'] is None
        assert tcp('client', '198.51.100.1', 22) == 'EDGE'
        assert tcp('client', '198.51.100.1', 30022) == 'NATIVE'
        print(
            'protocol policies: reserved SSH, TCP/UDP separation, address isolation, mapped range offsets, target fencing and withdrawal/recreation passed'
        )
        print(
            'primary L3: native TCP/SSH-port, UDP, ICMP, shared underlay endpoint, host TCP/UDP/ICMP replies, repeated convergence, stopped-target fence and withdrawal passed'
        )
    finally:
        for p in servers:
            p.kill()
            p.wait()


def withdrawal_recovery(local, root):
    """Replay separate Agent processes across a bounded, interrupted cleanup."""
    desired = json.loads(json.dumps(local))

    def rule(name, start, end):
        return {
            'id': name,
            'address': '203.0.114.20',
            'mode': {
                'kind': 'l4_range',
                'protocol': 'tcp_udp',
                'listen_start': start,
                'listen_end': end,
                'target_start': 8030,
                'target_end': 8030 + end - start,
            },
            'target': '192.168.0.8',
            'tunnel': None,
            'host_local': False,
        }

    withdrawn = rule('withdrawn-connections', 31000, 31064)
    desired['rules'].extend([withdrawn, rule('healthy-connections', 31070, 31070)])

    def settle():
        for _ in range(4):
            report = converge('host2', desired, root)
            if report['error'] is None:
                return
            assert 'continues next round' in report['error'], report
        raise AssertionError('bounded cleanup did not converge')

    settle()
    code = r"""
import json,socket,sys
port=int(sys.argv[1]); peers=[]
for kind in [socket.SOCK_STREAM,socket.SOCK_DGRAM]:
    s=socket.socket(socket.AF_INET,kind);s.settimeout(1);s.connect(('203.0.114.20',port));peers.append(s)
print('ready',flush=True)
for line in sys.stdin:
    result=[]
    for s in peers:
        try:
            s.send(b'probe');result.append(s.recv(200).startswith(b'probe'))
        except OSError: result.append(False)
    print(json.dumps(result),flush=True)
"""
    clients = []
    try:
        for port in (31000, 31070):
            client = subprocess.Popen(
                ['ip', 'netns', 'exec', 'client', 'python3', '-u', '-c', code, str(port)],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            clients.append(client)
            assert client.stdout.readline().strip() == 'ready'

        def probe(client):
            client.stdin.write('probe\n')
            client.stdin.flush()
            return json.loads(client.stdout.readline())

        assert probe(clients[0]) == [True, True]
        assert probe(clients[1]) == [True, True]
        settle()
        assert probe(clients[0]) == [True, True]
        assert probe(clients[1]) == [True, True]
        desired['rules'].remove(withdrawn)
        desired['generation'] += 1
        report = converge('host2', desired, root)
        assert 'continues next round' in report['error'], report
        sh('nft', 'list', 'table', 'ip', 'brocade_cvm_entry_withdraw', ns='host2')
        assert probe(clients[0]) == [False, False]
        assert probe(clients[1]) == [True, True]
        # Each converge starts a new Agent process with the durable cursor.
        settle()
        settle()
        assert probe(clients[0]) == [False, False]
        assert probe(clients[1]) == [True, True]
        remaining = sh(
            'conntrack',
            '-L',
            '-f',
            'ipv4',
            '-d',
            '203.0.114.20',
            '--dst-nat',
            '192.168.0.8',
            '-p',
            'udp',
            '--dport',
            '31000',
            ns='host2',
        )
        assert not remaining.stdout.strip(), remaining.stdout
        assert sh('nft', 'list', 'table', 'ip', 'brocade_cvm_entry_withdraw', ns='host2', check=False).returncode
        print(
            'withdrawal recovery: established TCP/UDP revoked, durable fence/cursor replayed, healthy TCP/UDP preserved'
        )
    finally:
        for client in clients:
            client.kill()
            client.wait()


def main():
    global BINARY, WORK
    BINARY = os.path.abspath(sys.argv[1])
    WORK = tempfile.mkdtemp(prefix='entries-netns-')
    sh('mount', '-t', 'tmpfs', 'tmpfs', '/run')
    os.makedirs('/run/netns', exist_ok=True)
    for ns in ('client', 'edge', 'host', 'host2', 'guest', 'guest2'):
        sh('ip', 'netns', 'add', ns)
        sh('ip', 'link', 'set', 'lo', 'up', ns=ns)

    def veth(a, a_name, a_addr, b, b_name, b_addr):
        sh('ip', 'link', 'add', a_name, 'netns', a, 'type', 'veth', 'peer', 'name', b_name, 'netns', b)
        for ns, name, address in ((a, a_name, a_addr), (b, b_name, b_addr)):
            sh('ip', 'address', 'add', address, 'dev', name, ns=ns)
            sh('ip', 'link', 'set', name, 'up', ns=ns)

    veth('client', 'c0', '203.0.113.100/24', 'edge', 'up0', '203.0.113.1/24')
    for address in ('203.0.113.12/32', '203.0.113.30/32'):
        sh('ip', 'address', 'add', address, 'dev', 'lo', ns='edge')
    veth('edge', 'u1', '198.51.100.1/24', 'host', 'u1', '198.51.100.2/24')
    # The second host is also an entry machine for its own container.
    veth('client', 'c1', '203.0.114.100/24', 'host2', 'up0', '203.0.114.1/24')
    sh('ip', 'address', 'add', '203.0.114.20/32', 'dev', 'lo', ns='host2')
    veth('edge', 'u2', '198.51.101.1/24', 'host2', 'u2', '198.51.101.2/24')
    # Both hosts use the same sandbox network; the guests even share an address.
    veth('host', 'cube-dev', '192.168.0.1/24', 'guest', 'eth0', '192.168.0.7/24')
    sh('ip', 'address', 'add', '192.168.0.9/24', 'dev', 'eth0', ns='guest')
    veth('host2', 'cube-dev', '192.168.0.1/24', 'guest2', 'eth0', '192.168.0.7/24')
    sh('ip', 'address', 'add', '192.168.0.8/24', 'dev', 'eth0', ns='guest2')
    for ns in ('guest', 'guest2'):
        sh('ip', 'route', 'add', 'default', 'via', '192.168.0.1', ns=ns)
    for ns in ('edge', 'host', 'host2'):
        sh('sysctl', '-qw', 'net.ipv4.ip_forward=1', ns=ns)
    guests = [
        subprocess.Popen(
            ['ip', 'netns', 'exec', ns, 'python3', '-u', '-c', GUEST],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        for ns in ('guest', 'guest2')
    ]
    try:
        for guest in guests:
            assert guest.stdout.readline().strip() == 'ready'
        scenario()
    except BaseException:
        diagnose()
        raise
    finally:
        for guest in guests:
            guest.kill()
            guest.wait()


def diagnose():
    for ns in ('edge', 'host', 'host2'):
        for command in (
            ('nft', 'list', 'ruleset'),
            ('wg', 'show'),
            ('ip', '-4', 'rule', 'show'),
            ('ip', '-4', 'route', 'show', 'table', 'all'),
            ('ip', '-4', 'address', 'show'),
            ('conntrack', '-L'),
        ):
            result = sh(*command, ns=ns, check=False)
            print(f'--- {ns}: {" ".join(command)}\n{result.stdout}{result.stderr}', file=sys.stderr)


def scenario():
    edge_key, edge_public = keypair()
    host_key, host_public = keypair()
    host2_key, host2_public = keypair()
    l4 = {
        'kind': 'l4_range',
        'protocol': 'tcp_udp',
        'listen_start': 30022,
        'listen_end': 30022,
        'target_start': 8022,
        'target_end': 8022,
    }
    edge = {
        'node_id': 'edge',
        'generation': 1,
        'rules': [
            {
                'id': 'ssh',
                'address': '203.0.113.12',
                'mode': l4,
                'target': '192.168.0.7',
                'tunnel': 'entry-tunnel-3',
                'host_local': False,
            },
            {
                'id': 'web',
                'address': '203.0.113.30',
                'mode': {'kind': 'l3'},
                'target': '192.168.0.9',
                'tunnel': 'entry-tunnel-3',
                'host_local': False,
            },
            # Same container address, other host: only the tunnel tells them apart.
            {
                'id': 'other',
                'address': '203.0.113.12',
                'mode': {
                    'kind': 'l4_range',
                    'protocol': 'tcp',
                    'listen_start': 30023,
                    'listen_end': 30023,
                    'target_start': 8022,
                    'target_end': 8022,
                },
                'target': '192.168.0.7',
                'tunnel': 'entry-tunnel-4',
                'host_local': False,
            },
        ],
        'deliveries': [],
        'tunnels': [
            tunnel(3, 'entry', 'edge', 'host', edge_key, host_public, '198.51.100.1'),
            tunnel(4, 'entry', 'edge', 'host2', edge_key, host2_public, '198.51.101.1'),
        ],
    }
    host = {
        'node_id': 'host',
        'generation': 1,
        'rules': [],
        'deliveries': [
            {
                'id': 'ssh',
                'instance_id': 'cvm-web',
                'mode': l4,
                'target': '192.168.0.7',
                'tunnel': 'entry-tunnel-3',
                'host_local': False,
            },
            {
                'id': 'web',
                'instance_id': 'cvm-web',
                'mode': {'kind': 'l3'},
                'target': '192.168.0.9',
                'tunnel': 'entry-tunnel-3',
                'host_local': False,
            },
        ],
        'tunnels': [tunnel(3, 'target', 'edge', 'host', host_key, edge_public, '198.51.100.1')],
    }
    host2 = {
        'node_id': 'host2',
        'generation': 1,
        'rules': [],
        'deliveries': [
            {
                'id': 'other',
                'instance_id': 'cvm-api',
                'mode': {
                    'kind': 'l4_range',
                    'protocol': 'tcp',
                    'listen_start': 30023,
                    'listen_end': 30023,
                    'target_start': 8022,
                    'target_end': 8022,
                },
                'target': '192.168.0.7',
                'tunnel': 'entry-tunnel-4',
                'host_local': False,
            },
        ],
        'tunnels': [tunnel(4, 'target', 'edge', 'host2', host2_key, edge_public, '198.51.101.1')],
    }
    roots = {ns: tempfile.mkdtemp(prefix=f'{ns}-', dir=WORK) for ns in ('edge', 'host', 'host2')}
    for ns, desired in (('edge', edge), ('host', host), ('host2', host2)):
        report = converge(ns, desired, roots[ns])
        assert report['error'] is None, (ns, report)
        assert all(t['error'] is None for t in report['tunnels']), (ns, report)
        assert all(r['configured'] for r in report['rules']), (ns, report)
    # The host side dials; give WireGuard a moment to complete its handshake.
    deadline = time.monotonic() + 20
    reply = None
    while time.monotonic() < deadline and reply is None:
        reply = tcp('client', '203.0.113.12', 30022, timeout=2)
    assert reply == '192.168.0.1 192.168.0.7:8022', reply
    assert udp('client', '203.0.113.12', 30022) == 'probe 192.168.0.1'
    assert tcp('client', '203.0.113.30', 80) == '192.168.0.1 192.168.0.9:80'
    sh('ping', '-c', '1', '-W', '3', '203.0.113.30', ns='client')
    # The other host's guest has the same address but answers with its own listener set.
    reply = None
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline and reply is None:
        reply = tcp('client', '203.0.113.12', 30023, timeout=2)
    assert reply == '192.168.0.1 192.168.0.7:8022', reply
    assert tcp('client', '203.0.113.12', 30024, timeout=2) is None
    # Simulate interruption after atomic creation but before the alias update.
    links = json.loads(sh('ip', '-d', '-j', 'link', 'show', ns='edge').stdout)
    owned = next(link for link in links if link.get('ifalias') == 'brocade-entry:entry-tunnel-3')
    sh('ip', 'link', 'set', 'dev', owned['ifname'], 'alias', '', ns='edge')
    # Reports prove connected tunnels and configured items.
    report = converge('edge', edge, roots['edge'])
    assert all(t['connected'] for t in report['tunnels']), report
    assert all(r['configured'] for r in report['rules']), report
    links = json.loads(sh('ip', '-j', 'link', 'show', ns='edge').stdout)
    assert any(link.get('ifalias') == 'brocade-entry:entry-tunnel-3' for link in links)
    # A rule without a target keeps the port fenced instead of reaching the machine.
    stopped = json.loads(json.dumps(edge))
    stopped['generation'] = 2
    stopped['rules'][0]['target'] = None
    assert converge('edge', stopped, roots['edge'])['error'] is None
    assert tcp('client', '203.0.113.12', 30022, timeout=2) is None
    # Re-pointing keeps the address: the same entry now reaches the second host.
    moved = json.loads(json.dumps(edge))
    moved['generation'] = 3
    moved['rules'][0]['tunnel'] = 'entry-tunnel-4'
    moved['rules'][0]['mode'] = {
        'kind': 'l4_range',
        'protocol': 'tcp',
        'listen_start': 30022,
        'listen_end': 30022,
        'target_start': 8022,
        'target_end': 8022,
    }
    moved['rules'][0]['target'] = '192.168.0.8'
    host2_moved = json.loads(json.dumps(host2))
    host2_moved['generation'] = 2
    host2_moved['deliveries'].append(
        {
            'id': 'ssh',
            'instance_id': 'cvm-api',
            'mode': moved['rules'][0]['mode'],
            'target': '192.168.0.8',
            'tunnel': 'entry-tunnel-4',
            'host_local': False,
        }
    )
    assert converge('host2', host2_moved, roots['host2'])['error'] is None
    assert converge('edge', moved, roots['edge'])['error'] is None
    assert tcp('client', '203.0.113.12', 30022) == '192.168.0.1 192.168.0.8:8022'
    # Same-machine delivery: no tunnel, straight to the primary address via cube-dev.
    local = json.loads(json.dumps(host2_moved))
    local['generation'] = 3
    local['rules'] = [
        {
            'id': 'local',
            'address': '203.0.114.20',
            'mode': {
                'kind': 'l4_range',
                'protocol': 'tcp',
                'listen_start': 30080,
                'listen_end': 30080,
                'target_start': 8022,
                'target_end': 8022,
            },
            'target': '192.168.0.8',
            'tunnel': None,
            'host_local': False,
        }
    ]
    assert converge('host2', local, roots['host2'])['error'] is None
    assert tcp('client', '203.0.114.20', 30080) == '192.168.0.1 192.168.0.8:8022'
    assert tcp('client', '203.0.113.12', 30022) == '192.168.0.1 192.168.0.8:8022'
    withdrawal_recovery(local, roots['host2'])
    primary_l3(moved, host, roots)
    # Withdrawal removes the table contents, tunnel devices and policy rules.
    for ns in ('edge', 'host', 'host2'):
        empty = {'node_id': ns, 'generation': 99, 'rules': [], 'deliveries': [], 'tunnels': []}
        assert converge(ns, empty, roots[ns])['error'] is None
        links = json.loads(sh('ip', '-j', 'link', 'show', ns=ns).stdout)
        assert not any(str(l.get('ifalias', '')).startswith('brocade-entry:') for l in links), ns
        rules = json.loads(sh('ip', '-4', '-j', 'rule', 'show', ns=ns).stdout)
        assert not any(r.get('priority') == 31700 for r in rules), ns
    assert tcp('client', '203.0.113.12', 30023, timeout=2) is None
    print(
        'entries netns: L4 TCP/UDP, L3 TCP/ICMP, repeated container addresses across hosts, '
        'fence without target, re-point across hosts, same-machine delivery and full withdrawal passed'
    )


if __name__ == '__main__':
    if os.environ.get('BROCADE_ENTRY_NETNS_CHILD') != '1':
        os.execvpe(
            'unshare',
            [
                'unshare',
                '--user',
                '--map-root-user',
                '--net',
                '--mount',
                '--fork',
                sys.executable,
                os.path.abspath(__file__),
            ]
            + sys.argv[1:],
            dict(os.environ, BROCADE_ENTRY_NETNS_CHILD='1'),
        )
    main()
