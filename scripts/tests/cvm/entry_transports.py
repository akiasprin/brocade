#!/usr/bin/env python3
"""Run Store-generated entry assignments on three explicitly owned disposable KVM guests.

The lab manifest supplies localhost SSH ports for b (entry), h1 (native target),
and h2 (external client). This runner never changes the physical host network.
"""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import time

REMOTE = '/opt/brocade-entry-transports'
STATE = REMOTE + '/state'
ADDRESSES = {'b': '172.30.80.21', 'h1': '172.30.80.11', 'h2': '172.30.80.12'}
SERVER = r"""
import socket,threading,sys
label=sys.argv[1]
def tcp():
 s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('0.0.0.0',22222));s.listen(16)
 while True:
  c,p=s.accept();c.sendall((label+' '+p[0]).encode());c.close()
def udp(port=22223):
 s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(('0.0.0.0',port))
 while True:
  data,p=s.recvfrom(8192);s.sendto((label+' '+p[0]).encode(),p)
threading.Thread(target=tcp,daemon=True).start();threading.Thread(target=udp,daemon=True).start();threading.Thread(target=udp,args=(59999,),daemon=True).start();threading.Event().wait()
"""


class Lab:
    def __init__(self, args):
        self.args = args
        self.manifest = json.loads(args.lab.read_text())
        self.started = time.monotonic()
        self.report = {
            'started_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
            'cases': [],
            'timeout_injection': not args.skip_timeout_test,
        }

    def ssh(self, node, command, data=None, check=True):
        result = subprocess.run(
            [
                'ssh',
                '-i',
                str(self.args.ssh_key),
                '-o',
                'IdentitiesOnly=yes',
                '-o',
                'StrictHostKeyChecking=accept-new',
                '-o',
                'ConnectTimeout=5',
                '-o',
                'UserKnownHostsFile=' + str(self.args.output / 'known_hosts'),
                '-p',
                str(self.manifest['nodes'][node]['ssh']),
                'root@127.0.0.1',
                command,
            ],
            input=data,
            capture_output=True,
            timeout=180,
        )
        if check and result.returncode:
            raise RuntimeError(f'{node} command failed ({result.returncode}): {result.stderr.decode()[-1500:]}')
        return result

    def text(self, node, command, check=True):
        return self.ssh(node, command, check=check).stdout.decode().strip()

    def put(self, node, path, data):
        self.ssh(
            node,
            'umask 077; cat >'
            + shlex.quote(path + '.next')
            + ' && mv '
            + shlex.quote(path + '.next')
            + ' '
            + shlex.quote(path),
            data,
        )

    def case(self, name):
        self.report['cases'].append(name)
        print('PASS ' + name, flush=True)

    def empty(self, node):
        return {
            'node_id': {'b': 'n2', 'h1': 'n1', 'h2': 'client'}[node],
            'generation': 100,
            'rules': [],
            'deliveries': [],
            'tunnels': [],
        }

    def desired(self, node, value):
        self.put(node, REMOTE + '/desired.json', json.dumps(value).encode())

    def prepare(self):
        for node in ADDRESSES:
            self.text(node, f'umask 077; mkdir -p {REMOTE} {STATE}; rm -f {STATE}/stop')
            code = f"from pathlib import Path;import os,signal;p=Path('{REMOTE}/server.pid');pid=int(p.read_text()) if p.exists() else 0;cmd=Path('/proc/'+str(pid)+'/cmdline');a=cmd.read_bytes().split(bytes([0])) if cmd.exists() else [];os.kill(pid,signal.SIGTERM) if len(a)>1 and a[1]==b'{REMOTE}/server.py' else None"
            self.text(node, 'python3 -c ' + shlex.quote(code))
            for source, name in [
                (self.args.agent_tests, 'test-agent'),
                (self.args.xray, 'xray'),
                (self.args.phantun_client, 'phantun-client'),
                (self.args.phantun_server, 'phantun-server'),
            ]:
                data = source.read_bytes()
                self.put(node, REMOTE + '/' + name, data)
                digest = self.text(node, 'sha256sum ' + REMOTE + '/' + name).split()[0]
                assert digest == hashlib.sha256(data).hexdigest()
                self.text(node, f'chmod 700 {REMOTE}/{name}')
            self.desired(node, self.empty(node))
            self.put(node, REMOTE + '/server.py', SERVER.encode())
            self.text(
                node,
                f'nohup python3 {REMOTE}/server.py {shlex.quote(node)} >{REMOTE}/server.log 2>&1 </dev/null & echo $! >{REMOTE}/server.pid',
            )
            self.text(
                node,
                f'ip -6 address replace fd30:80::{ {"b": 21, "h1": 11, "h2": 12}[node] }/64 dev lan0; ip -4 route replace 198.51.100.0/24 via 172.30.80.21 dev lan0'
                if node != 'b'
                else 'ip -6 address replace fd30:80::21/64 dev lan0; ip address replace 198.51.100.20/32 dev lo; ip address replace 198.51.100.30/32 dev lo',
            )
            command = f'PATH={REMOTE}:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin BROCADE_TEST_LOG_SINK="cat >/dev/null" BROCADE_RUN_CVM_KVM=1 BROCADE_ENTRY_TEST_NOTRACK=1 BROCADE_ENTRY_FIXTURE={REMOTE}/desired.json BROCADE_ENTRY_ROOT={STATE} BROCADE_XRAY_BIN={REMOTE}/xray {REMOTE}/test-agent --ignored --exact cvm::entries::tests::serve_fixture --nocapture --test-threads 1'
            self.text(node, f'nohup env {command} >{REMOTE}/daemon.log 2>&1 </dev/null & echo $! >{REMOTE}/daemon.pid')
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                certificate = self.text(node, f'cat {STATE}/certificate.public.pem', check=False)
                if certificate.startswith('-----BEGIN CERTIFICATE-----'):
                    (self.args.output / (node + '.public.pem')).write_text(certificate + '\n')
                    break
                time.sleep(0.2)
            else:
                raise RuntimeError(node + ' daemon failed: ' + self.text(node, f'tail -n 20 {REMOTE}/daemon.log'))
            self.case(node + ' exact binaries and persistent test worker prepared')

    def connected(self, assignments, timeout=50):
        deadline = time.monotonic() + timeout
        current = {}
        while time.monotonic() < deadline:
            for node, side in [('b', 'provider'), ('h1', 'consumer')]:
                raw = self.text(node, f'cat {STATE}/report.json', check=False)
                try:
                    current[node] = json.loads(raw)
                except json.JSONDecodeError:
                    current[node] = {}
            if all(
                current[node].get('error') is None
                and any(
                    t['id'] == assignments[side]['tunnels'][0]['id'] and t['connected'] and t['error'] is None
                    for t in current[node].get('tunnels', [])
                )
                for node, side in [('b', 'provider'), ('h1', 'consumer')]
            ):
                return
            time.sleep(0.2)
        raise RuntimeError('entry connection did not converge: ' + json.dumps(current))

    def request(self, address, protocol='tcp', port=None, node='h2', fragmented=True):
        port = port or (22222 if protocol == 'tcp' else 22223)
        kind = 'SOCK_STREAM' if protocol == 'tcp' else 'SOCK_DGRAM'
        code = f'import socket;s=socket.socket(socket.AF_INET,socket.{kind});s.settimeout(3);s.connect(({address!r},{port}));'
        if protocol == 'udp':
            code += "s.send(b'fragmented-'*400);" if fragmented else "s.send(b'probe');"
        code += 'print(s.recv(200).decode())'
        return self.text(node, 'python3 -c ' + shlex.quote(code))

    def check(self, assignments):
        public = assignments['public']
        slot = assignments['provider']['tunnels'][0]['slot']
        transit = '.'.join(map(str, (240, (slot * 4 + 1) >> 16, ((slot * 4 + 1) >> 8) & 255, (slot * 4 + 1) & 255)))
        for address, protocol, port in [
            (public, 'tcp', 22222),
            (public, 'udp', 22223),
            (assignments['shared'], 'tcp', 30022),
            (assignments['shared'], 'udp', 30023),
        ]:
            assert self.request(address, protocol, port) == 'h1 ' + transit
        self.text('h2', 'ping -c 1 -W 2 -s 2000 ' + shlex.quote(public))
        assert self.request(ADDRESSES['h2'], node='b') == 'h2 ' + ADDRESSES['b']
        assert self.request(ADDRESSES['h2'], 'udp', node='b') == 'h2 ' + ADDRESSES['b']
        direct = self.text('h2', 'ssh-keyscan -T 3 -t ed25519 ' + ADDRESSES['h1']).splitlines()[-1].split()[1:]
        forwarded = self.text('h2', 'ssh-keyscan -T 3 -t ed25519 ' + shlex.quote(public)).splitlines()[-1].split()[1:]
        assert direct == forwarded, 'whole-IP forwarding must include the target SSH service'

    def pids(self, node, executable):
        code = f"""from pathlib import Path
import json
out=[]
for p in Path('/proc').iterdir():
 try:
  a=(p/'cmdline').read_bytes().split(b'\\0')
  owned=a and (a[0]=={(REMOTE + '/' + executable).encode()!r} or (Path(a[0].decode()).name=={executable!r} and any(a[i]==b'--tun' and a[i+1].startswith(b'bep') for i in range(len(a)-1))))
  if owned: out.append(int(p.name))
 except OSError: pass
print(json.dumps(sorted(out)))
"""
        return json.loads(self.text(node, 'python3 -c ' + shlex.quote(code)))

    def run(self):
        paths = sorted(
            self.args.assignments.glob('*.json'),
            key=lambda p: (
                {'udp': 0, 'xray': 1, 'phantun': 2}[p.stem.split('-')[0]],
                p.stem.split('-')[2],
                p.stem.split('-')[1],
            ),
        )
        assert len(paths) == 12, f'expected 12 actual Store assignment cases, got {len(paths)}'
        for path in paths:
            assignment = json.loads(path.read_text())
            self.desired('h1', assignment['consumer'])
            time.sleep(0.4)  # Dial before the entry is updated, exercising stale L3 DNAT cleanup.
            self.desired('b', assignment['provider'])
            self.connected(assignment)
            self.check(assignment)
            self.case(assignment['name'] + ' L3/L4 TCP/fragmented UDP/ICMP and provider replies')
            if assignment['name'] == 'phantun-provider-v4':
                before = {
                    node: self.pids(node, 'phantun-client') + self.pids(node, 'phantun-server') for node in ['b', 'h1']
                }
                assert all(before.values()), 'entry Phantun process identity must be observed'
                for node in ['b', 'h1']:
                    if node == 'b':
                        value = {
                            'servers': [
                                {
                                    'tcp_port': 44550,
                                    'forward_to_udp_port': 59999,
                                    'tun': {'name': 'btentrylab', 'local': '192.168.250.1', 'peer': '192.168.250.2'},
                                }
                            ],
                            'clients': [],
                        }
                    else:
                        value = {
                            'servers': [],
                            'clients': [
                                {
                                    'peer': 'b',
                                    'listen_udp_port': 44551,
                                    'remote_tcp_endpoint': '172.30.80.21:44550',
                                    'tun': {'name': 'btentrylab', 'local': '192.168.251.1', 'peer': '192.168.251.2'},
                                }
                            ],
                        }
                    for revision in [1, 2]:
                        value['revision'] = revision
                        data = json.dumps(value).encode()
                        self.put(node, STATE + '/backbone.json', data)
                        deadline = time.monotonic() + 10
                        while (
                            self.text(node, f'cat {STATE}/backbone.applied', check=False)
                            != hashlib.sha256(data).hexdigest()
                        ):
                            assert time.monotonic() < deadline
                            time.sleep(0.2)
                    assert set(before[node]).issubset(
                        set(self.pids(node, 'phantun-client') + self.pids(node, 'phantun-server'))
                    )
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    try:
                        if self.request('127.0.0.1', 'udp', 44551, 'h1', fragmented=False) == 'b 127.0.0.1':
                            break
                    except RuntimeError:
                        pass
                    time.sleep(0.2)
                else:
                    raise RuntimeError('legacy Phantun UDP stopped working beside entry Phantun')
                self.check(assignment)
                self.case('backbone Phantun reload preserves entry workers and both UDP paths')
            if assignment['name'] in ['xray-provider-v4', 'phantun-provider-v4']:
                executable = 'xray' if assignment['name'].startswith('xray') else 'phantun-client'
                old = self.pids('h1', executable)
                assert old
                self.text('h1', 'kill -KILL ' + ' '.join(map(str, old)))
                time.sleep(1)
                self.connected(assignment)
                self.check(assignment)
                assert not set(old).intersection(self.pids('h1', executable))
                self.case(executable + ' process failure recovers without changing entry identity')
            time.sleep(1)
            self.check(assignment)
        if not self.args.skip_timeout_test:
            # Exercise a real blocked FakeTCP path long enough for the production timeout.
            # The test-owned firewall is inside the disposable VM and is always removed.
            blocked = json.loads(paths[-1].read_text())
            assert blocked['name'] == 'phantun-provider-v6'
            tunnel = blocked['consumer']['tunnels'][0]
            dev = 'bep' + hashlib.sha256(tunnel['id'].encode()).hexdigest()[:12]
            for node in ['b', 'h1']:
                report = json.loads(self.text(node, f'cat {STATE}/report.json'))
                diagnostics = report['diagnostics']
                assert any(event['source'] == 'phantun' for event in diagnostics['events'])
                assert any(event['source'] == 'agent' for event in diagnostics['events'])
                raw = json.dumps(diagnostics)
                for item in blocked['consumer']['tunnels'] + blocked['provider']['tunnels']:
                    assert item['credential'] not in raw and item['peer_credential'] not in raw
            self.case('both endpoint Agent and Phantun process logs are retained without credentials')
            try:
                self.text(
                    'h1',
                    f'nft add table inet brocade_log_test; nft add chain inet brocade_log_test forward "{{ type filter hook forward priority -300; policy accept; }}"; nft add rule inet brocade_log_test forward iifname "bep*" tcp dport {tunnel["port"]} drop',
                )
                owned = [
                    pid
                    for pid in self.pids('h1', 'phantun-client')
                    if self.text('h1', f'grep -a -q -- {dev} /proc/{pid}/cmdline && echo owned', check=False) == 'owned'
                ]
                assert owned, 'current entry Phantun client must be identified'
                self.text('h1', 'kill -KILL ' + ' '.join(map(str, owned)))
                deadline = time.monotonic() + 150
                update = time.monotonic()
                while time.monotonic() < deadline:
                    report = json.loads(self.text('h1', f'cat {STATE}/report.json'))
                    if any(item.get('error') and '120' in item['error'] for item in report['tunnels']):
                        assert any(p['phase'] == 'failed' for p in report['diagnostics']['resources'])
                        assert any(
                            e['level'] == 'error' and '120' in e['message'] for e in report['diagnostics']['events']
                        )
                        break
                    if time.monotonic() >= update:
                        print('Waiting for real 120-second blocked-path verdict', flush=True)
                        update = time.monotonic() + 20
                    time.sleep(1)
                else:
                    raise RuntimeError('blocked Phantun path stayed pending instead of explaining its timeout')
                assert self.request(ADDRESSES['h2'], node='b') == 'h2 ' + ADDRESSES['b']
                self.case('blocked Phantun explains its timeout while unrelated host traffic stays healthy')
            finally:
                self.text('h1', 'nft delete table inet brocade_log_test', check=False)
            self.connected(blocked)
            self.check(blocked)
            report = json.loads(self.text('h1', f'cat {STATE}/report.json'))
            assert any(p['phase'] == 'connected' for p in report['diagnostics']['resources'])
            self.case('clearing the blocked path recovers the same entry and records the recovery')
        for node in ['b', 'h1']:
            self.desired(node, self.empty(node))
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            reports = [json.loads(self.text(node, f'cat {STATE}/report.json')) for node in ['b', 'h1']]
            if all(not r.get('tunnels') and r.get('error') is None for r in reports):
                break
            time.sleep(0.2)
        else:
            raise RuntimeError('entry withdrawal did not finish')
        assert self.request('172.30.80.21') == 'b 172.30.80.12'
        for node in ['b', 'h1']:
            raw = json.loads(self.text(node, 'ip -j link show'))
            assert not any(
                link.get('ifalias', '').startswith(('brocade-entry:', 'brocade-entry-phantun:')) for link in raw
            )
        self.case('withdrawal releases all owned entry devices and restores provider services')
        self.report['status'] = 'passed'

    def stop(self):
        for node in ADDRESSES:
            self.text(node, f'touch {STATE}/stop', check=False)
        for node in ADDRESSES:
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                code = f"from pathlib import Path;p=Path('{REMOTE}/daemon.pid');pid=int(p.read_text()) if p.exists() else 0;a=Path('/proc/'+str(pid)+'/cmdline');print('running' if a.exists() and a.read_bytes().split(bytes([0]))[0]==b'{REMOTE}/test-agent' else 'stopped')"
                if self.text(node, 'python3 -c ' + shlex.quote(code)) == 'stopped':
                    break
                time.sleep(0.2)
            else:
                raise RuntimeError(node + ' test daemon did not stop')
            self.case(node + ' test worker stopped')

    def save(self):
        self.report['ended_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        self.report['elapsed_seconds'] = round(time.monotonic() - self.started, 3)
        (self.args.output / (self.args.action + '.json')).write_text(json.dumps(self.report, indent=2) + '\n')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('action', choices=['prepare', 'run', 'stop'])
    p.add_argument(
        '--skip-timeout-test',
        action='store_true',
        help='Run the data-plane matrix without the two-minute blocked-path injection',
    )
    for flag in ['lab', 'ssh-key', 'agent-tests', 'xray', 'phantun-client', 'phantun-server', 'output', 'assignments']:
        p.add_argument('--' + flag, type=Path, required=True)
    args = p.parse_args()
    os.umask(0o077)
    args.output.mkdir(parents=True, exist_ok=True)
    lab = Lab(args)
    try:
        getattr(lab, args.action)()
    except BaseException as error:
        lab.report['status'] = 'failed'
        lab.report['error'] = str(error)
        for node in ['b', 'h1']:
            diagnostic = lab.text(
                node,
                f'cat {STATE}/report.json; tail -n 30 {REMOTE}/daemon.log; wg show all listen-port; wg show all latest-handshakes; nft list table ip brocade_cvm_entries; nft list table inet brocade_entry_phantun; ip -4 route show table all',
                check=False,
            )
            (args.output / (node + '-diagnostic.log')).write_text(diagnostic)
        raise
    finally:
        lab.save()


if __name__ == '__main__':
    main()
