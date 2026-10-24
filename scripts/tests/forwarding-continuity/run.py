#!/usr/bin/env python3
"""Isolated, real Console/PostgreSQL/Agent/Xray forwarding continuity experiment."""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import shutil
import socket
import socketserver
import subprocess
import sys
import threading
import time
import traceback
import uuid

ROOT = Path(__file__).resolve().parents[3] if '--inside' not in sys.argv else Path('/')
BASE = '/users/platform.acme/alice/forwarding'
NODE_TOKEN = 'broc_node_disposable_forwarding_continuity_lab'
BOOT_TOKEN = 'disposable-forwarding-continuity-bootstrap-token'


def require(value, message):
    if not value:
        raise RuntimeError(message)


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def command(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=30, **kwargs)


def api(method, path, body=None, cookie=None, bootstrap=False):
    client = http.client.HTTPConnection('127.0.0.1', 18080, timeout=3)
    headers = {'Content-Type': 'application/json'}
    if cookie:
        headers['Cookie'] = cookie
    if bootstrap:
        headers['Authorization'] = 'Bearer ' + BOOT_TOKEN
    try:
        client.request(method, path, json.dumps(body) if body is not None else None, headers)
        response = client.getresponse()
        data = response.read()
        require(200 <= response.status < 300, f'{method} {path}: HTTP {response.status} {data[:800]!r}')
        return json.loads(data) if data else None, response.getheader('set-cookie')
    finally:
        client.close()


class TCPServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(300)
        try:
            while data := self.request.recv(65536):
                self.request.sendall(data)
        except (TimeoutError, ConnectionError, OSError):
            pass


def exchange(connection, payload):
    connection.sendall(payload)
    data = bytearray()
    while len(data) < len(payload):
        block = connection.recv(len(payload) - len(data))
        require(block, 'unexpected EOF')
        data.extend(block)
    require(data == payload, 'payload mismatch')


class Probes:
    def __init__(self):
        self.stop = threading.Event()
        self.workers = []
        self.rows = []
        for port in [20001, 20003]:
            for index in range(3):
                self.start('long-tcp', port, index)
            self.start('short-tcp', port, 0)
        for port in [20002, 20003]:
            self.start('persistent-udp', port, 0)
            self.start('fresh-udp', port, 0)

    def start(self, kind, port, index):
        row = {'kind': kind, 'port': port, 'index': index, 'samples': 0, 'errors': [], 'max_gap_ms': 0}
        self.rows.append(row)
        thread = threading.Thread(target=self.run, args=(row,), daemon=True)
        self.workers.append(thread)
        thread.start()

    def run(self, row):
        connection = None
        previous = time.monotonic()
        try:
            while not self.stop.is_set():
                now = time.monotonic()
                payload = row['samples'].to_bytes(8, 'big') + bytes(range(256)) * 4
                try:
                    if row['kind'].endswith('udp'):
                        if connection is None:
                            connection = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                            connection.settimeout(.5)
                            connection.connect(('127.0.0.1', row['port']))
                        connection.send(payload)
                        require(connection.recv(4096) == payload, 'UDP payload mismatch')
                    else:
                        if connection is None:
                            connection = socket.create_connection(('127.0.0.1', row['port']), .5)
                            connection.settimeout(.5)
                        exchange(connection, payload)
                    completed = time.monotonic()
                    row['samples'] += 1
                    row['max_gap_ms'] = max(row['max_gap_ms'], (completed - previous) * 1000)
                    previous = completed
                except Exception as error:
                    row['errors'].append({'at': now, 'error': str(error)})
                    if row['kind'] == 'long-tcp':
                        return  # Never reconnect: continuity must use the original socket.
                if row['kind'] in ('short-tcp', 'fresh-udp') and connection:
                    connection.close()
                    connection = None
                self.stop.wait(.02)
        finally:
            if connection:
                connection.close()

    def snapshot(self):
        return [{'kind': r['kind'], 'port': r['port'], 'index': r['index'],
                 'samples': r['samples'], 'errors': len(r['errors'])} for r in self.rows]

    def finish(self):
        self.stop.set()
        for thread in self.workers:
            thread.join(2)
        return self.rows


def xray_identity():
    found = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            args = (entry / 'cmdline').read_bytes().split(b'\0')
            if b'run' in args and b'/tmp/continuity/xray.json' in args:
                ticks = (entry / 'stat').read_text().rsplit(') ', 1)[1].split()[19]
                found.append({'pid': int(entry.name), 'start_ticks': ticks})
        except (FileNotFoundError, ProcessLookupError):
            pass
    return found


def inside(output):
    state = Path('/tmp/continuity')
    state.mkdir(mode=0o700)
    command(['ip', 'addr', 'add', '11.0.0.2/32', 'dev', 'lo'])
    servers = []
    for port in [31000, 31001]:
        server = TCPServer(('11.0.0.2', port), Echo)
        servers.append(server)
        threading.Thread(target=server.serve_forever, daemon=True).start()
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(('11.0.0.2', 31000))
    def udp_echo():
        while True:
            try:
                data, peer = udp.recvfrom(65536)
                udp.sendto(data, peer)
            except OSError:
                return
    threading.Thread(target=udp_echo, daemon=True).start()
    config = {
        'log': {'loglevel': 'info'}, 'stats': {},
        'api': {'tag': 'api', 'services': ['HandlerService', 'StatsService']},
        'policy': {'system': {'statsInboundUplink': True, 'statsInboundDownlink': True}},
        'inbounds': [{'tag': 'api', 'listen': '127.0.0.1', 'port': 10085,
                      'protocol': 'dokodemo-door', 'settings': {'address': '127.0.0.1'}}],
        'outbounds': [{'tag': 'out:forward', 'protocol': 'freedom'}],
        'routing': {'rules': [{'type': 'field', 'inboundTag': ['api'], 'outboundTag': 'api'}]},
    }
    save(state / 'xray.json', config)
    cookie = api('POST', '/auth/init', {
        'operator_id': 'root', 'display_name': 'Docker lab',
        'password': 'disposable-lab-password-42!', 'root_tenant': 'platform'}, bootstrap=True)[1].split(';')[0]
    for user in ['alice', 'bob']:
        api('PUT', f'/users/platform.acme/{user}/forwarding-nodes/n1', {
            'enabled': True, 'tcp_allowed': True, 'udp_allowed': True,
            'listen_port_start': 20000, 'listen_port_end': 20999,
            'max_rules': 64, 'allow_private_targets': False}, cookie)

    writes = {}
    def create(name, protocol, port, user='alice'):
        body = {'node_id': 'n1', 'name': name, 'protocol': protocol, 'listen_port': port,
                'target_host': '11.0.0.2', 'target_port': 31000, 'enabled': True}
        view = api('POST', f'/users/platform.acme/{user}/forwarding', body, cookie)[0]
        rule = next(r for r in view['rules'] if r['listen_port'] == port)
        writes[rule['id']] = body
        return rule['id'], view

    def overview(user='alice'):
        return api('GET', f'/users/platform.acme/{user}/forwarding', cookie=cookie)[0]

    events = []
    def wait_status(status, timeout=23, user='alice'):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            view = overview(user)
            if view['entitlements'][0]['reconcile_status'] == status:
                if status != 'converged' or all(r['runtime_status'] in ['active', 'disabled'] for r in view['rules']):
                    return view
            time.sleep(.1)
        raise RuntimeError(f'timed out waiting for {status}: {overview(user)}')

    original = [create(name, protocol, port)[0] for name, protocol, port in [
        ('old-tcp', 'tcp', 20001), ('old-udp', 'udp', 20002),
        ('old-dual', 'tcp_udp', 20003), ('edit-control', 'tcp', 20004)]]
    environment = {**os.environ, 'BROCADE_XRAY_BIN': '/inputs/xray'}
    repair = command(['/inputs/agent', 'repair', '--state-dir', str(state)], env=environment)
    (output / 'repair.log').write_text(repair.stdout + repair.stderr)
    agent_log = (output / 'agent.log').open('wb')
    resident = subprocess.Popen(['/inputs/agent', 'run', '--server', 'http://127.0.0.1:18080',
        '--token', NODE_TOKEN, '--state-dir', str(state), '--apply', 'linux'],
        env=environment, stdout=agent_log, stderr=subprocess.STDOUT)
    probes = None
    result = {'passed': False, 'events': events}
    try:
        baseline = wait_status('converged')
        save(output / 'baseline.json', baseline)
        identity = xray_identity()
        require(len(identity) == 1, f'expected one Xray: {identity}')
        probes = Probes()
        time.sleep(1)
        require(all(r['samples'] > 5 and not r['errors'] for r in probes.rows), 'invalid traffic baseline')

        def scenario(name, mutate, status='converged', user='alice'):
            before = probes.snapshot()
            began = time.monotonic()
            returned = mutate()
            pending = overview()
            save(output / f'{name}-pending.json', pending)
            for old_rule in pending['rules']:
                if old_rule['id'] in original and not (name == 'change-one-target' and old_rule['id'] == original[3]):
                    require(old_rule['runtime_status'] == 'active', f'{name}: old rule status was {old_rule["runtime_status"]}')
            terminal = wait_status(status, user=user)
            save(output / f'{name}-terminal.json', terminal)
            time.sleep(.25)
            after = probes.snapshot()
            require(all(b['samples'] < a['samples'] and a['errors'] == b['errors']
                        for b, a in zip(before, after)), f'{name}: an unchanged forwarding flow stopped')
            require(xray_identity() == identity, f'{name}: Xray was restarted')
            event = {'name': name, 'elapsed_ms': (time.monotonic() - began) * 1000,
                     'old_rule_status_during_sync': {r['name']: r['runtime_status'] for r in pending['rules'] if r['id'] in original},
                     'node_status_after': terminal['entitlements'][0]['reconcile_status'],
                     'xray_identity': xray_identity(), 'before': before, 'after': after}
            events.append(event)
            print(json.dumps({'event': name, 'elapsed_ms': event['elapsed_ms'],
                'old_status': event['old_rule_status_during_sync'], 'old_flows_healthy': True}), flush=True)
            return returned

        for name, protocol, port in [('add-tcp', 'tcp', 20010), ('add-udp', 'udp', 20011),
                                     ('add-dual', 'tcp_udp', 20012)]:
            scenario(name, lambda n=name, p=protocol, l=port: create(n, p, l))
            if protocol != 'udp':
                with socket.create_connection(('127.0.0.1', port), .5) as connection:
                    connection.settimeout(.5)
                    exchange(connection, b'new-rule-really-works')
            if protocol != 'tcp':
                with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as connection:
                    connection.settimeout(.5)
                    connection.sendto(b'new-udp-really-works', ('127.0.0.1', port))
                    require(connection.recv(4096) == b'new-udp-really-works', 'new UDP rule failed')
        scenario('other-user-add', lambda: create('bob-new-tcp', 'tcp', 20013, 'bob'), user='bob')

        collision = socket.socket()
        collision.bind(('0.0.0.0', 20020))
        collision.listen()
        scenario('add-bind-conflict', lambda: create('occupied-port', 'tcp', 20020), status='error')
        failed = overview()
        require(all(rule['runtime_status'] == ('error' if rule['listen_port'] == 20020 else 'active')
                    for rule in failed['rules']), 'one failure contaminated another rule status')
        require(next(rule for rule in failed['rules'] if rule['listen_port'] == 20020)['runtime_error'],
                'failed rule has no scoped error')
        time.sleep(2.5)  # Exercise several actual retry rounds with old streams still active.
        scenario('bind-conflict-recovery', collision.close)
        with socket.create_connection(('127.0.0.1', 20020), .5) as connection:
            connection.settimeout(.5)
            exchange(connection, b'released-port-automatically-restored')
        result['bind_conflict_recovery'] = {'restored': True}
        generation_before_rename = overview()['entitlements'][0]['desired_generation']

        renamed = dict(writes[original[0]], name='old-tcp-renamed')
        scenario('rename-only', lambda: api('PUT', f'{BASE}/{original[0]}', renamed, cookie))
        require(overview()['entitlements'][0]['desired_generation'] == generation_before_rename,
                'metadata-only edit changed the runtime generation')

        control = socket.create_connection(('127.0.0.1', 20004), .5)
        control.settimeout(.5)
        exchange(control, b'control-before-edit')
        changed = dict(writes[original[3]], target_port=31001)
        scenario('change-one-target', lambda: api('PUT', f'{BASE}/{original[3]}', changed, cookie))
        closed = False
        try:
            exchange(control, b'control-after-edit')
        except (RuntimeError, ConnectionError, OSError):
            closed = True
        control.close()
        require(closed, 'positive control: editing a forwarding target did not close its old TCP socket')
        with socket.create_connection(('127.0.0.1', 20004), .5) as connection:
            connection.settimeout(.5)
            exchange(connection, b'new-target-verified')

        result.update(passed=True, old_xray_identity=identity, final_xray_identity=xray_identity(),
                      positive_control_edited_rule_closed=closed,
                      final_overview=overview(), duration_secs=sum(e['elapsed_ms'] for e in events) / 1000)
    except Exception as error:
        result.update(error=str(error), traceback=traceback.format_exc())
    finally:
        if probes:
            result['traffic'] = probes.finish()
            result['unchanged_flow_errors'] = sum(len(r['errors']) for r in result['traffic'])
            require(result['unchanged_flow_errors'] == 0, 'unchanged flows had failures')
        resident.terminate()
        try:
            resident.wait(5)
        except subprocess.TimeoutExpired:
            resident.kill()
            resident.wait(2)
        for row in xray_identity():
            os.kill(row['pid'], 9)
        shutil.copytree(state, output / 'state', dirs_exist_ok=True,
                        ignore=shutil.ignore_patterns('*.sock', 'xray', 'control.sock'))
        save(output / 'result.json', result)
        print(json.dumps({k: result.get(k) for k in ['passed', 'error', 'duration_secs', 'unchanged_flow_errors']}), flush=True)
    return 0 if result['passed'] else 1


def outside(output, args):
    output.mkdir(parents=True, exist_ok=False)
    name = 'brocade-forwarding-check-' + uuid.uuid4().hex[:10]
    network, database, node = name + '-net', name + '-db', name + '-node'
    image = args.image
    inputs = output / 'inputs'
    inputs.mkdir()
    binaries = {'agent': Path(args.agent).resolve(),
                'console': Path(args.console).resolve(),
                'xray': Path(args.xray).resolve()}
    manifest = {'timestamp': time.strftime('%Y-%m-%dT%H:%M:%S%z'), 'binaries': {},
                'image': command(['docker', 'image', 'inspect', image, '--format', '{{.Id}}']).stdout.strip()}
    for label, source in binaries.items():
        destination = inputs / label
        try:
            os.link(source, destination)
        except OSError as error:
            if error.errno != 18:  # EXDEV: an explicit binary may live on another filesystem.
                raise
            shutil.copy2(source, destination)
        manifest['binaries'][label] = {'source': str(source), 'sha256': hashlib.file_digest(destination.open('rb'), 'sha256').hexdigest()}
    manifest['source_sha256'] = {str(path): hashlib.sha256((ROOT / path).read_bytes()).hexdigest()
        for path in [Path('crates/brocade-agent/src/forwarding.rs'),
                     Path('crates/brocade-store/src/forwarding.rs'),
                     Path('frontend/src/panes/user-forwarding.tsx')]}
    sidecar = binaries['xray'].with_suffix('.build.json')
    if sidecar.exists():
        shutil.copy2(sidecar, inputs / 'xray.build.json')
    shutil.copy2(__file__, inputs / 'run.py')
    manifest['runner_sha256'] = hashlib.sha256((inputs / 'run.py').read_bytes()).hexdigest()
    manifest['git_head'] = command(['git', 'rev-parse', 'HEAD'], cwd=ROOT).stdout.strip()
    save(output / 'manifest.json', manifest)
    try:
        command(['docker', 'network', 'create', '--internal', network])
        command(['docker', 'run', '-d', '--name', database, '--network', network,
                 '--network-alias', 'db', '--memory', '512m', '--pids-limit', '128',
                 '-e', 'POSTGRES_USER=brocade', '-e', 'POSTGRES_DB=brocade',
                 '-e', 'POSTGRES_HOST_AUTH_METHOD=trust', 'postgres:18-alpine'])
        for _ in range(100):
            ready = subprocess.run(['docker', 'exec', database, 'pg_isready', '-h', '127.0.0.1', '-U', 'brocade'], capture_output=True)
            if ready.returncode == 0:
                break
            time.sleep(.1)
        else:
            raise RuntimeError('PostgreSQL did not start')
        command(['docker', 'run', '-d', '--name', node, '--network', network,
                 '--cap-add', 'NET_ADMIN', '--memory', '1500m', '--cpus', '2', '--pids-limit', '512',
                 '--mount', f'type=bind,src={inputs},dst=/inputs,readonly',
                 '--mount', f'type=bind,src={output},dst=/results', image, 'sleep', 'infinity'])
        command(['docker', 'exec', '-d', '-e', 'DATABASE_URL=postgres://brocade@db:5432/brocade',
                 '-e', 'BROCADE_ADMIN_BIND=127.0.0.1:18080', '-e', f'BROCADE_BOOTSTRAP_TOKEN={BOOT_TOKEN}',
                 '-e', 'BROCADE_SECRET_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=', node, 'sh', '-c',
                 'exec /inputs/console >/results/console.log 2>&1'])
        for _ in range(150):
            ready = subprocess.run(['docker', 'exec', node, 'curl', '-fsS', 'http://127.0.0.1:18080/healthz'], capture_output=True)
            if ready.returncode == 0:
                break
            time.sleep(.2)
        else:
            raise RuntimeError('Console did not start: ' + (output / 'console.log').read_text()[-3000:])
        token_hash = hashlib.sha256(NODE_TOKEN.encode()).hexdigest()
        sql = f"""
INSERT INTO tenants(id,name) VALUES('platform.acme','Docker tenant');
INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,api_port,overlay,egress_allowed,dns_kind,dns_servers)
VALUES('n1','platform.acme','Docker forwarding node','11.0.0.2','10.66.0.1','fixture-private','fixture-public',51820,10085,false,true,'system','[]');
INSERT INTO users(tenant_id,id,uuid) VALUES('platform.acme','alice','2d2304da-f114-4574-8d44-625afdb1db5c'),('platform.acme','bob','4f84aaf8-49ea-4cce-8a93-683ee21ba3c6');
INSERT INTO node_lifecycle_state(node_id) VALUES('n1') ON CONFLICT DO NOTHING;
INSERT INTO node_agent_state(node_id,token_hash,token_prefix,token_created_at) VALUES('n1','{token_hash}','broc_node_disposable',now())
ON CONFLICT(node_id) DO UPDATE SET token_hash=excluded.token_hash,token_prefix=excluded.token_prefix,token_created_at=now();
"""
        command(['docker', 'exec', '-i', database, 'psql', '-U', 'brocade', '-d', 'brocade', '-v', 'ON_ERROR_STOP=1'], input=sql)
        completed = subprocess.run(['docker', 'exec', node, 'timeout', '210', 'python3', '/inputs/run.py',
                                    '--inside', '--output', '/results'], text=True, capture_output=True, timeout=220)
        (output / 'docker.log').write_text(completed.stdout + completed.stderr)
        print(completed.stdout, end='', flush=True)
        print(completed.stderr, end='', file=sys.stderr, flush=True)
        if (output / 'result.json').exists():
            query = "SELECT json_build_object('node', row_to_json(s), 'rules', (SELECT json_agg(r ORDER BY id) FROM forwarding_rules r)) FROM forwarding_node_state s WHERE node_id='n1';"
            evidence = command(['docker', 'exec', database, 'psql', '-U', 'brocade', '-d', 'brocade', '-At', '-c', query]).stdout
            (output / 'postgres-final.json').write_text(evidence)
        return completed.returncode
    finally:
        for container in [node, database]:
            subprocess.run(['docker', 'rm', '-f', container], capture_output=True, timeout=20)
        subprocess.run(['docker', 'network', 'rm', network], capture_output=True, timeout=20)
        if output.exists():
            subprocess.run(['docker', 'run', '--rm', '--network', 'none', '--mount',
                f'type=bind,src={output},dst=/results', image, 'chown', '-R', f'{os.getuid()}:{os.getgid()}', '/results'], capture_output=True, timeout=20)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--inside', action='store_true')
    parser.add_argument('--output', required=True)
    parser.add_argument('--agent', default=str(ROOT / 'target/debug/brocade-agent'))
    parser.add_argument('--console', default=str(ROOT / 'target/debug/brocade-console'))
    parser.add_argument('--xray', help='Explicit Xray binary to exercise, including older releases')
    parser.add_argument('--image', default='brocade-forwarding-continuity:local')
    args = parser.parse_args()
    if not args.inside and not args.xray:
        parser.error('--xray is required outside the container')
    path = Path(args.output).resolve()
    sys.exit(inside(path) if args.inside else outside(path, args))
