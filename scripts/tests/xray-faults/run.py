#!/usr/bin/env python3
"""Disposable Docker fault injection against the production Agent CLI and Xray.

No production endpoints, test-only Agent hooks, host PID/network access or mocks of Xray.
The local HTTP fixture implements only work delivery and receipt ACKs, not Store policy.
"""
import argparse
import concurrent.futures
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import socketserver
import ssl
import subprocess
import sys
import threading
import time
import traceback
import uuid

CASES = [
    'config-success', 'config-force-drain', 'config-agent-kill-switching',
    'config-agent-kill-draining', 'config-candidate-kill', 'config-both-kill',
    'config-old-kill', 'config-candidate-stop', 'config-old-stop',
    'config-control-unlink', 'config-legacy-process', 'config-bind-conflict',
    'config-invalid', 'config-report-offline', 'config-console-offline',
    'config-grants-1024', 'config-grants-4096',
    'binary-success', 'binary-force-drain', 'binary-agent-kill-draining',
    'binary-candidate-kill', 'binary-both-kill', 'binary-candidate-stop',
    'binary-old-stop', 'binary-control-unlink', 'binary-legacy-process',
    'config-repeat', 'config-spool-full', 'binary-control-backlog',
    'config-old-stop-switching', 'binary-old-stop-switching', 'binary-candidate-stop-ready',
    'config-reality-vision', 'binary-reality-vision',
    'config-ack-lost', 'binary-ack-lost', 'config-grants-changed', 'config-fallback-grants-changed',
    'binary-bad-digest', 'binary-bad-version', 'binary-report-offline',
]


def require(value, message):
    if not value:
        raise RuntimeError(message)


def read_json(path):
    try:
        return json.loads(path.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def private_json(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value))
    temporary.chmod(0o600)
    temporary.replace(path)


def free_port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(2)
        try:
            while payload := self.request.recv(65536):
                self.request.sendall(payload)
        except (ConnectionError, TimeoutError):
            pass


class EchoServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def exchange(connection, payload):
    connection.sendall(payload)
    result = bytearray()
    while len(result) < len(payload):
        received = connection.recv(len(payload) - len(result))
        require(received, 'unexpected EOF')
        result.extend(received)
    require(result == payload, 'payload corruption or sequence mismatch')


class Traffic:
    def __init__(self, lab):
        self.lab = lab
        self.stop = threading.Event()
        self.records = {'tcp': [], 'udp': [], 'grants': []}
        if lab.secure_public:
            self.records['vision'] = []
        self.long = []
        self.threads = []
        self.connections = []
        for index in range(4):
            connection = self.connect('vision' if lab.secure_public and index >= 2 else 'tcp')
            self.connections.append(connection)
            result = {'index': index, 'samples': 0, 'max_gap_ms': 0, 'error': None}
            self.long.append(result)
            self.start(self.stream, connection, result)
        for kind in self.records:
            self.start(self.requests, kind)

    def start(self, target, *args):
        thread = threading.Thread(target=target, args=args, daemon=True)
        self.threads.append(thread)
        thread.start()

    def connect(self, kind):
        port = self.lab.vless if kind == 'grants' else self.lab.secure_public if kind == 'vision' else self.lab.public
        connection = socket.create_connection(('127.0.0.1', port), .5)
        connection.settimeout(2 if kind == 'vision' else .5)
        if kind == 'vision':
            context = ssl.create_default_context(cafile='/tmp/cover.crt')
            connection = context.wrap_socket(connection, server_hostname='local-cover.test')
        if kind == 'grants':
            connection.sendall(b'\0' + uuid.UUID(self.lab.user).bytes + b'\0\x01'
                               + self.lab.upstream.server_address[1].to_bytes(2, 'big')
                               + b'\x01\x7f\0\0\x01')
            # VLESS response is sent with the first payload, not at handshake time.
        return connection

    def stream(self, connection, result):
        previous = time.monotonic()
        while not self.stop.is_set():
            try:
                payload = result['samples'].to_bytes(8, 'big') + bytes(range(256)) * 8
                exchange(connection, payload)
                now = time.monotonic()
                result['max_gap_ms'] = max(result['max_gap_ms'], (now - previous) * 1000)
                previous = now
                result['samples'] += 1
                self.stop.wait(.02)
            except Exception as error:
                if not self.stop.is_set() and not result.get('expected_close'):
                    result['error'] = str(error)
                    result['failed_at'] = time.monotonic()
                return

    def requests(self, kind):
        while not self.stop.is_set():
            began = time.monotonic()
            error = None
            try:
                payload = b'fault-lab-' + time.monotonic_ns().to_bytes(8, 'big')
                if kind == 'udp':
                    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as connection:
                        connection.settimeout(.5)
                        connection.sendto(payload, ('127.0.0.1', self.lab.udp))
                        require(connection.recv(1024) == payload, 'UDP corruption')
                else:
                    with self.connect(kind) as connection:
                        if kind == 'grants':
                            connection.sendall(payload)
                            result = bytearray()
                            while len(result) < len(payload) + 2:
                                received = connection.recv(4096)
                                require(received, 'VLESS unexpected EOF')
                                result.extend(received)
                            require(result[:2] == b'\0\0' and result[2:] == payload, 'VLESS corruption')
                        else:
                            exchange(connection, payload)
            except Exception as failure:
                error = str(failure)
            self.records[kind].append({'at': began, 'duration_ms': (time.monotonic() - began) * 1000,
                                       'ok': error is None, 'error': error})
            self.stop.wait(.05 if kind != 'grants' else .2)

    def finish(self):
        finished_at = time.monotonic()
        self.stop.set()
        for connection in self.connections:
            connection.close()
        for thread in self.threads:
            thread.join(2)
        summary = {}
        for kind, records in self.records.items():
            good = [row for row in records if row['ok']]
            gaps = [(right['at'] - left['at']) * 1000 for left, right in zip(good, good[1:])]
            if records:
                gaps.append((finished_at - (good[-1]['at'] if good else records[0]['at'])) * 1000)
            summary[kind] = {'samples': len(records), 'failed': len(records) - len(good),
                             'max_success_gap_ms': max(gaps, default=0),
                             'max_request_ms': max((row['duration_ms'] for row in records), default=0)}
        return {'requests': summary, 'long_streams': self.long, 'raw': self.records}


class Lab:
    def __init__(self, case, output):
        # A permitted private target stays entirely inside this network namespace.
        # Loopback is intentionally forbidden for managed user forwarding.
        subprocess.run(['ip', 'addr', 'add', '10.77.0.1/32', 'dev', 'lo'], check=True, timeout=5)
        self.case, self.output = case, output
        self.state = Path('/tmp/bf')
        self.state.mkdir(mode=0o700, exist_ok=True)
        self.xray = Path('/tmp/bin/xray')
        self.xray.parent.mkdir()
        shutil.copy2('/inputs/xray', self.xray)
        self.xray.chmod(0o755)
        self.agent = '/inputs/agent'
        self.env = {**os.environ, 'BROCADE_XRAY_BIN': str(self.xray)}
        self.processes, self.events, self.observations, self.receipts, self.usages = [], [], [], [], []
        self.desired, self.offer = None, None
        self.offline, self.report_offline = False, False
        self.api, self.public, self.udp, self.vless, self.added, self.forward = [free_port() for _ in range(6)]
        self.secure_public = None
        self.user = 'b831381d-6324-4d53-ad4f-000000000000'
        self.upstream = EchoServer(('0.0.0.0', 0), Echo)
        threading.Thread(target=self.upstream.serve_forever, daemon=True).start()
        self.udp_upstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.udp_upstream.bind(('127.0.0.1', 0))
        def echo_udp():
            while True:
                try:
                    data, peer = self.udp_upstream.recvfrom(4096)
                    self.udp_upstream.sendto(data, peer)
                except OSError:
                    return
        threading.Thread(target=echo_udp, daemon=True).start()
        lab = self
        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def log_message(self, *_args):
                pass
            def handle_request(self):
                size = int(self.headers.get('content-length', '0'))
                body = json.loads(self.rfile.read(size)) if size else None
                status, reply = 204, None
                if lab.offline:
                    self.connection.close()
                    return
                if self.path == '/agent/v1/desired':
                    reply = lab.desired
                elif self.path == '/agent/v1/usage':
                    lab.usages.append(body)
                elif self.path == '/agent/v1/user-traffic/policies':
                    reply = {'sources': [], 'sha256': hashlib.sha256(b'[]').hexdigest()}
                elif self.path == '/agent/v1/user-traffic/budget':
                    reply = {'mode': 'unlimited', 'valid_ms': 10000, 'managed': False}
                elif self.path == '/agent/v1/forwarding/desired':
                    reply = read_json(lab.state / 'forwarding-desired.json')
                elif self.path == '/agent/v1/xray-release':
                    reply = lab.offer
                elif self.path == '/agent/v1/xray-release/report':
                    lab.receipts.append(body)
                    if lab.case.endswith('ack-lost') and len(lab.receipts) == 1:
                        lab.offer = None
                        self.connection.shutdown(socket.SHUT_RDWR)
                        return
                    if lab.report_offline:
                        status = 503
                    else:
                        reply = {'accepted': True}
                        lab.offer = None
                elif self.path == '/agent/v1/observation':
                    lab.observations.append(body)
                    if lab.case.endswith('ack-lost') and len(lab.observations) == 1:
                        lab.desired = None
                        self.connection.shutdown(socket.SHUT_RDWR)
                        return
                    if lab.report_offline:
                        status = 503
                    else:
                        lab.desired = None
                elif self.path == '/candidate':
                    data = lab.candidate_bytes
                    self.send_response(200)
                    self.send_header('content-length', str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                    return
                if reply is not None:
                    status = 200
                encoded = json.dumps(reply).encode() if reply is not None else b''
                self.send_response(status)
                self.send_header('content-length', str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)
            do_GET = do_POST = handle_request
        self.http = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.http.serve_forever, daemon=True).start()
        self.server = f'http://127.0.0.1:{self.http.server_port}'
        self.base = {
            'log': {'loglevel': 'info'}, 'stats': {},
            'api': {'tag': 'api', 'services': ['HandlerService', 'StatsService']},
            'policy': {'levels': {'0': {'statsUserUplink': True, 'statsUserDownlink': True}},
                       'system': {'statsInboundUplink': True, 'statsInboundDownlink': True}},
            'inbounds': [
                {'tag': 'api', 'listen': '127.0.0.1', 'port': self.api, 'protocol': 'dokodemo-door',
                 'settings': {'address': '127.0.0.1'}},
                {'tag': 'public', 'listen': '127.0.0.1', 'port': self.public, 'protocol': 'dokodemo-door',
                 'settings': {'address': '127.0.0.1', 'port': self.upstream.server_address[1], 'network': 'tcp'}},
                {'tag': 'public-udp', 'listen': '127.0.0.1', 'port': self.udp, 'protocol': 'dokodemo-door',
                 'settings': {'address': '127.0.0.1', 'port': self.udp_upstream.getsockname()[1], 'network': 'udp'}},
                {'tag': 'vless', 'listen': '127.0.0.1', 'port': self.vless, 'protocol': 'vless',
                 'settings': {'clients': [], 'decryption': 'none'}},
            ],
            'outbounds': [{'tag': 'out:forward', 'protocol': 'freedom', 'settings': {'ipsBlocked': []}}],
            'routing': {'rules': [{'type': 'field', 'inboundTag': ['api'], 'outboundTag': 'api'}]},
        }
        if case.endswith('reality-vision'):
            cover = free_port()
            self.secure_public, vision_port = free_port(), free_port()
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256',
                            '-nodes', '-keyout', '/tmp/cover.key', '-out', '/tmp/cover.crt',
                            '-subj', '/CN=local-cover.test', '-days', '1'], check=True, capture_output=True)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = ssl.TLSVersion.TLSv1_3
            context.set_alpn_protocols(['h2', 'http/1.1'])
            context.load_cert_chain('/tmp/cover.crt', '/tmp/cover.key')
            class TLSCover(EchoServer):
                def get_request(self):
                    connection, peer = super().get_request()
                    return context.wrap_socket(connection, server_side=True, do_handshake_on_connect=False), peer
            self.cover = TLSCover(('127.0.0.1', cover), Echo)
            threading.Thread(target=self.cover.serve_forever, daemon=True).start()
            keys = subprocess.check_output([str(self.xray), 'x25519'], text=True)
            fields = dict(line.split(': ', 1) for line in keys.strip().splitlines())
            private_key, public_key = fields['PrivateKey'], fields['Password (PublicKey)']
            self.base['inbounds'].append({
                'tag': 'vision', 'listen': '127.0.0.1', 'port': vision_port, 'protocol': 'vless',
                'settings': {'clients': [], 'decryption': 'none'},
                'streamSettings': {'network': 'tcp', 'security': 'reality', 'realitySettings': {
                    'dest': f'127.0.0.1:{cover}', 'serverNames': ['local-cover.test'],
                    'privateKey': private_key, 'shortIds': ['0123456789abcdef']}}})
            client_config = {'log': {'loglevel': 'warning'}, 'inbounds': [{
                'tag': 'plain', 'listen': '127.0.0.1', 'port': self.secure_public, 'protocol': 'dokodemo-door',
                'settings': {'address': '127.0.0.1', 'port': cover}}], 'outbounds': [{
                'tag': 'proxy', 'protocol': 'vless', 'settings': {'vnext': [{'address': '127.0.0.1', 'port': vision_port,
                    'users': [{'id': self.user, 'encryption': 'none', 'flow': 'xtls-rprx-vision'}]}]},
                'streamSettings': {'network': 'tcp', 'security': 'reality', 'realitySettings': {
                    'serverName': 'local-cover.test', 'fingerprint': 'chrome', 'password': public_key,
                    'shortId': '0123456789abcdef'}}}]}
            private_json(Path('/tmp/client.json'), client_config)
            log = (output / 'client.log').open('wb')
            self.client = subprocess.Popen(['/inputs/xray', 'run', '-config', '/tmp/client.json'], stdout=log, stderr=log)
            self.processes.append(self.client)
            log.close()
        private_json(self.state / 'xray.json', self.base)
        # Provision the authoritative empty source policy with the initial test
        # configuration, as Console enrollment does; missing retained policy is
        # correctly fail-closed on an already provisioned Agent.
        private_json(self.state / 'source-access-policy.json', {'revision': 0, 'node_ips': [], 'users': []})
        (self.state / 'usage-generation').write_text('1')
        count = 4096 if case.endswith('4096') else 1024 if case.endswith('1024') else 64
        self.grants = [{'tag': 'vless', 'clients': [
            {'email': f'lab-{index}', 'uuid': f'b831381d-6324-4d53-ad4f-{index:012x}', 'flow': None, 'level': 0}
            for index in range(count)]}]
        if self.secure_public:
            self.grants.append({'tag': 'vision', 'clients': [{'email': 'vision-user', 'uuid': self.user,
                                                             'flow': 'xtls-rprx-vision', 'level': 0}]})
        private_json(self.state / 'grants.desired.json', self.grants)
        private_json(self.state / 'forwarding-desired.json', {'generation': 72, 'rules': [
            {'id': 42, 'protocol': 'tcp', 'listen_port': self.forward,
             'target_host': '10.77.0.1', 'target_port': self.upstream.server_address[1],
             'allow_private_targets': True}]})
        self.traffic = None
        self.resident = None
        self.collision = None
        self.fault_at = None
        self.recovered_at = None

    def event(self, name, **values):
        self.events.append({'at': time.monotonic(), 'name': name, **values})

    def start_agent(self, command='run'):
        log = (self.output / f'agent-{len(self.processes)}.log').open('wb')
        args = [self.agent, command, '--state-dir', str(self.state)]
        if command != 'repair':
            args += ['--server', self.server, '--token', 'broc_node_fault_lab', '--apply', 'linux']
        process = subprocess.Popen(args, env=self.env, stdout=log, stderr=subprocess.STDOUT)
        self.processes.append(process)
        log.close()
        return process

    def wait(self, check, timeout, message):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            result = check()
            if result:
                return result
            time.sleep(.001)
        raise RuntimeError(message)

    def runtime(self):
        return read_json(self.state / 'xray-upgrade.json')

    def phase(self, phases, candidate=False):
        runtime = self.runtime()
        return runtime if runtime and runtime['phase'] in phases and (not candidate or runtime.get('candidate')) else None

    def pids(self):
        found = []
        for entry in Path('/proc').iterdir():
            if not entry.name.isdigit():
                continue
            try:
                args = (entry / 'cmdline').read_bytes().split(b'\0')
                stat = (entry / 'stat').read_text().rsplit(') ', 1)[1].split()
                if args and b'xray' in args[0] and b'run' in args and b'/tmp/bf/xray.json' in args and stat[0] != 'Z':
                    found.append(int(entry.name))
            except (FileNotFoundError, PermissionError, ProcessLookupError):
                pass
        return found

    def business(self):
        stage = 'starting'
        try:
            for port in [self.public, self.forward]:
                stage = 'tcp-' + ('public' if port == self.public else 'forward')
                with socket.create_connection(('127.0.0.1', port), .3) as connection:
                    connection.settimeout(.3)
                    exchange(connection, b'final-business-check')
            if self.secure_public and self.traffic:
                stage = 'vision'
                with self.traffic.connect('vision') as connection:
                    exchange(connection, b'final-vision-check')
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as connection:
                stage = 'udp'
                connection.settimeout(.3)
                connection.sendto(b'final-udp-check', ('127.0.0.1', self.udp))
                if connection.recv(1024) != b'final-udp-check':
                    return False
            # Real API readback, including every frozen grant (not just TCP port health).
            for grant in self.grants:
                stage = 'grant-readback-' + grant['tag']
                result = subprocess.run([str(self.xray), 'api', 'inbounduser', f'--server=127.0.0.1:{self.api}',
                                         f"-tag={grant['tag']}"], capture_output=True, timeout=2)
                if result.returncode != 0 or any(client['email'].encode() not in result.stdout or client['uuid'].encode() not in result.stdout
                                                  for client in grant['clients']):
                    self.business_error = f'{stage}: exit={result.returncode}, returned={result.stdout[:1024]!r}, error={result.stderr[:256]!r}'
                    return False
                if result.stdout.count(b'"email"') != len(grant['clients']):
                    self.business_error = stage + ': wrong user count'
                    return False
            return True
        except (OSError, TimeoutError, subprocess.TimeoutExpired, RuntimeError) as error:
            self.business_error = stage + ': ' + str(error)
            return False

    def offer_work(self):
        if self.case.startswith('binary-'):
            version = subprocess.check_output([str(self.xray), 'version'], text=True).split()[1]
            data = self.xray.read_bytes()
            self.candidate_bytes = data + b'fault-lab-candidate\0'
            self.offer = {'release_id': 91, 'attempt': 1, 'version': version, 'url': self.server + '/candidate',
                          'sha256': hashlib.sha256(data + b'fault-lab-candidate\0').hexdigest(),
                          'previous_sha256': hashlib.sha256(data).hexdigest(),
                          'strategy': 'handoff-force', 'drain_timeout_secs': 10}
            if self.case.endswith('bad-digest'):
                self.offer['sha256'] = 'f' * 64
            if self.case.endswith('bad-version'):
                self.offer['version'] = '0.0.0-invalid'
        else:
            desired = json.loads(json.dumps(self.base))
            desired['inbounds'].append({'tag': 'added', 'listen': '127.0.0.1', 'port': self.added,
                                       'protocol': 'dokodemo-door', 'settings': {
                                           'address': '127.0.0.1', 'port': self.upstream.server_address[1]}})
            if self.case.endswith('bind-conflict'):
                self.collision = socket.socket()
                self.collision.bind(('127.0.0.1', self.added))
                self.collision.listen()
            if self.case.endswith('invalid'):
                desired['inbounds'][-1]['protocol'] = 'invalid-protocol'
            content = json.dumps(desired)
            self.desired = {'t': 'deployment', 'v': {'deployment': {
                'deployment_id': 9001, 'node_id': 'docker-fault-lab', 'claim_generation': 1, 'wave': 0,
                'actions': ['apply-xray'], 'xray_config_update_policy': 'handoff', 'usage_generation_id': None,
                'desired': {**{key: {'state': 'unmanaged', 'reason': 'test'}
                              for key in ['phantun', 'wireguard', 'hy2_port_hop']},
                            'xray': {'state': 'present', 'content': content,
                                     'sha256': hashlib.sha256(content.encode()).hexdigest()},
                            'grants': {'state': 'present', 'inbounds': self.grants}},
                'phantun_binary': None}, 'certificates': []}}

    def repeat_deployments(self):
        peak = 0
        for iteration in range(1, 8):
            self.resident.kill()
            self.resident.wait(2)
            self.base = read_json(self.state / 'xray.json')
            self.base['inbounds'] = [inbound for inbound in self.base['inbounds'] if inbound['tag'] != 'added']
            self.added = free_port()
            self.offer_work()
            self.desired['v']['deployment']['deployment_id'] = 9001 + iteration
            receipts = len(self.observations)
            self.resident = self.start_agent()
            runtime = self.wait(lambda: self.phase(['draining'], candidate=True), 15,
                                'repeated handoff did not reach admission')
            peak = max(peak, len(list((self.state / 'xray-handoff').iterdir())))
            runtime['drain_deadline_unix_secs'] = int(time.time()) - 1
            private_json(self.state / 'xray-upgrade.json', runtime)
            self.resident.kill()
            self.resident.wait(2)
            self.resident = self.start_agent()
            self.wait(lambda: not self.runtime() and len(self.observations) > receipts and self.business(),
                      20, 'repeated handoff did not terminate with healthy business')
        def directory_count():
            return len(list((self.state / 'xray-handoff').iterdir()))
        # The production guard runs every 30 seconds and removes at most four
        # directories per pass. Eight transitions can require two passes;
        # earlier checks during a live handoff correctly defer pruning.
        self.wait(lambda: directory_count() <= 3, 75, 'retired generations accumulated after 8 handoffs')
        self.event('eight-handoffs-complete', peak_generations=peak, retained_generations=directory_count())

    def inject(self):
        case = self.case
        if case.endswith('spool-full'):
            self.fault_at = time.monotonic()
            self.event('inject-enospc', free=os.statvfs(self.state).f_bavail * os.statvfs(self.state).f_frsize)
            time.sleep(2)
            self.wait(lambda: any('No space left on device' in path.read_text() for path in self.output.glob('agent-*.log')),
                      5, 'ENOSPC was not observed by Agent')
            (self.state / 'fault-fill').unlink()
            self.resident.kill()
            self.resident.wait(2)
            self.resident = self.start_agent()
            return
        if any(case.endswith(suffix) for suffix in ['bind-conflict', 'invalid', 'bad-digest',
                                                   'bad-version', 'legacy-process', 'fallback-grants-changed']):
            return
        if case.endswith(('agent-kill-switching', 'old-stop-switching')):
            runtime = self.wait(lambda: self.phase(['switching']),
                                20, 'never observed switching injection boundary')
        elif case.endswith('candidate-stop-ready'):
            runtime = self.wait(lambda: self.phase(['candidate-ready'], candidate=True),
                                40, 'never observed candidate-ready boundary')
        else:
            runtime = self.wait(lambda: self.phase(['draining', 'blocked'], candidate=True),
                                40, 'never observed admitted candidate')
        if case.endswith(('success', '1024', '4096', 'reality-vision', 'ack-lost', 'grants-changed')):
            before_samples = [row['samples'] for row in self.traffic.long]
            time.sleep(.4)
            require(all(row['error'] is None and row['samples'] > before
                        for row, before in zip(self.traffic.long, before_samples)),
                    'old TCP streams did not progress across candidate admission')
            self.event('old-streams-progressed-across-admission', samples=[row['samples'] for row in self.traffic.long])
        self.fault_at = time.monotonic()
        self.event('inject', phase=runtime['phase'], old=runtime['old']['pid'], candidate=runtime.get('candidate'))
        if case.endswith('agent-kill-switching') or case.endswith('agent-kill-draining'):
            self.resident.kill()
            self.resident.wait(2)
            self.resident = self.start_agent()
        elif case.endswith('candidate-kill') or case.endswith('both-kill'):
            os.kill(runtime['candidate']['pid'], signal.SIGKILL)
            if case.endswith('both-kill'):
                os.kill(runtime['old']['pid'], signal.SIGKILL)
        elif case.endswith('old-kill'):
            os.kill(runtime['old']['pid'], signal.SIGKILL)
        elif case.endswith(('candidate-stop', 'candidate-stop-ready', 'control-backlog')):
            os.kill(runtime['candidate']['pid'], signal.SIGSTOP)
            if case.endswith('control-backlog'):
                env = Path(f"/proc/{runtime['candidate']['pid']}/environ").read_bytes().split(b'\0')
                path = next(value.split(b'=', 1)[1] for value in env if value.startswith(b'XRAY_TCP_HANDOFF_MANIFEST='))
                control = read_json(Path(os.fsdecode(path)))['control_path']
                self.blocked_sockets = []
                for _ in range(512):
                    channel = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
                    channel.setblocking(False)
                    try:
                        channel.connect(control)
                        self.blocked_sockets.append(channel)
                    except BlockingIOError:
                        channel.close()
                        break
                require(self.blocked_sockets, 'control backlog was not filled')
                self.event('control-backlog-filled', connections=len(self.blocked_sockets))

        elif case.endswith(('old-stop', 'old-stop-switching')):
            os.kill(runtime['old']['pid'], signal.SIGSTOP)
        elif case.endswith('control-unlink'):
            pid = runtime['candidate']['pid']
            env = Path(f'/proc/{pid}/environ').read_bytes().split(b'\0')
            manifest = next(value.split(b'=', 1)[1] for value in env if value.startswith(b'XRAY_TCP_HANDOFF_MANIFEST='))
            Path(json.loads(Path(os.fsdecode(manifest)).read_text())['control_path']).unlink()
        elif case.endswith('force-drain'):
            # Exercise the real deadline transition without 120 seconds of test wall time.
            # Binary cases retain the actual offer's minimum 10-second deadline.
            if case.startswith('config-'):
                runtime['drain_deadline_unix_secs'] = int(time.time()) + 2
                private_json(self.state / 'xray-upgrade.json', runtime)
        elif case.endswith('report-offline'):
            self.report_offline = True
        elif case.endswith('console-offline'):
            self.offline = True
        if not case.endswith('force-drain'):
            for result in self.traffic.long:
                result['expected_close'] = True
            for connection in self.traffic.connections:
                try:
                    connection.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

    def run(self):
        initial = self.start_agent('repair')
        require(initial.wait(20) == 0, 'initial repair failed')
        # The resident Agent owns the live admission endpoint used by forwarding.
        self.resident = self.start_agent()
        self.wait(lambda: (self.state / 'user-traffic-control.json').is_file(), 5, 'resident user budget service did not initialize')
        baseline = self.business()
        require(baseline, 'initial forwarding/grants unavailable: ' + getattr(self, 'business_error', 'payload mismatch'))
        old = self.pids()
        require(len(old) == 1, f'expected one old generation, found {old}')
        self.old_pid = old[0]
        if self.case.endswith(('legacy-process', 'fallback-grants-changed')):
            os.kill(self.old_pid, signal.SIGKILL)
            process = subprocess.Popen([str(self.xray), 'run', '-config', str(self.state / 'xray.json')],
                                       env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            self.processes.append(process)
            self.old_pid = process.pid
            # This real Xray has no handoff manifest, representing a legacy launch.
            self.wait(lambda: subprocess.run([str(self.xray), 'api', 'statsquery',
                      f'--server=127.0.0.1:{self.api}'], capture_output=True, timeout=2).returncode == 0,
                      5, 'legacy API not ready')
            # Restore dynamic state over its real API, without relaunching it.
            for grant in self.grants:
                document = {'inbounds': [{'tag': grant['tag'], 'listen': '127.0.0.1', 'port': self.vless,
                                         'protocol': 'vless', 'settings': {
                    'decryption': 'none', 'clients': [{'email': client['email'], 'id': client['uuid'], 'level': 0}
                                for client in grant['clients']]}}]}
                private_json(self.state / 'legacy-grants.json', document)
                added = subprocess.run([str(self.xray), 'api', 'adu', f'--server=127.0.0.1:{self.api}',
                                       str(self.state / 'legacy-grants.json')], check=True, capture_output=True, timeout=5)
                (self.output / 'legacy-grants-cli.log').write_bytes(added.stdout + added.stderr)
                require(f"Added {len(grant['clients'])} user(s)".encode() in added.stdout,
                        'legacy grant setup did not actually add its users; inspect legacy-grants-cli.log')
            # Forwarding is not restored in this synthetic legacy launch; dynamic restoration
            # itself is separately covered in every current-to-current transition.
        self.traffic = Traffic(self)
        self.wait(lambda: all(any(row['ok'] for row in rows) for rows in self.traffic.records.values()),
                  8, 'baseline traffic did not succeed before deployment')
        time.sleep(.5)
        if self.case.endswith('grants-changed'):
            self.user = 'b831381d-6324-4d53-ad4f-100000000000'
            self.grants[0]['clients'][0]['uuid'] = self.user
        self.offer_work()
        self.started = time.monotonic()
        if self.case.endswith('spool-full'):
            free = os.statvfs(self.state).f_bavail * os.statvfs(self.state).f_frsize
            with (self.state / 'fault-fill').open('wb') as filler:
                os.posix_fallocate(filler.fileno(), 0, free - 1024)
        self.report_offline = self.case.endswith('report-offline')
        # Start a fresh work poll as in the original fixture. The temporary
        # baseline resident was needed for admission; this controlled restart
        # leaves the serving Xray and its shared accounting state intact.
        self.resident.kill()
        self.resident.wait(2)
        self.resident = self.start_agent()
        self.inject()
        negative = any(self.case.endswith(suffix) for suffix in ['bind-conflict', 'invalid', 'bad-digest', 'bad-version'])
        def done():
            rows = self.receipts if self.case.startswith('binary-') else self.observations
            if self.runtime():
                return False
            if negative:
                if not rows:
                    return False
                outcome = rows[-1].get('outcome', rows[-1].get('result'))
                require(outcome in ('failed-recovered', 'failed-dirty', 'unsupported'),
                        f'invalid work unexpectedly succeeded: {rows[-1]}')
                return self.business()
            if not rows:
                return False
            outcome = rows[-1].get('outcome', rows[-1].get('result'))
            disruptive = self.case.endswith(('candidate-kill', 'both-kill', 'candidate-stop', 'control-unlink', 'agent-kill-switching', 'candidate-stop-ready', 'old-stop-switching', 'control-backlog', 'old-stop', 'old-kill'))
            if outcome not in ('succeeded', 'applied'):
                if disruptive:
                    if self.case.startswith('binary-'):
                        # A dirty receipt may arrive before the independent recovery receipt.
                        # Require the final audit result, not just restored listeners.
                        return (self.case.endswith('old-stop-switching') and outcome == 'failed-recovered'
                                and self.business())
                    return self.business()
                raise RuntimeError(f'work unexpectedly failed: {outcome}: {rows[-1].get("error")}')
            if self.case.startswith('binary-') and not self.case.endswith('old-stop-switching'):
                require(rows[-1]['performed_update'], 'successful receipt did not perform an update')
            return self.business()
        if not self.case.endswith('force-drain'):
            time.sleep(.3)
            for result in self.traffic.long:
                result['expected_close'] = True
            for connection in self.traffic.connections:
                try:
                    connection.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
        if self.case.endswith('console-offline'):
            self.wait(lambda: not self.runtime() and self.business(), 38, 'offline local recovery did not finish')
            self.offline = False
            self.resident.kill()
            self.resident.wait(2)
            self.resident = self.start_agent()
        self.wait(done, 38, 'no terminal state with restored forwarding/grants within 38 seconds')
        if self.case.endswith('repeat'):
            self.repeat_deployments()
        self.recovered_at = time.monotonic()
        if self.case.endswith('report-offline'):
            prior_pid = self.pids()
            self.report_offline = False
            self.resident.kill()
            self.resident.wait(2)
            self.resident = self.start_agent()
            self.wait(lambda: not (self.state / 'observation.spool.jsonl').exists()
                      or (self.state / 'observation.spool.jsonl').stat().st_size == 0,
                      5, 'receipt spool did not drain after ACK restoration')
            require(self.pids() == prior_pid, 'receipt replay restarted Xray')
        time.sleep(.5)
        require(len(self.pids()) == 1, f'nonterminal generation leak: {self.pids()}')
        if negative:
            require(self.pids() == [self.old_pid], 'invalid candidate restarted the old generation')
        elif not self.case.startswith('binary-') and not self.case.endswith('old-stop-switching'):
            with socket.create_connection(('127.0.0.1', self.added), .5) as connection:
                connection.settimeout(.5)
                exchange(connection, b'new-listener-verified')
        if self.case.endswith('ack-lost'):
            rows = self.receipts if self.case.startswith('binary-') else self.observations
            self.wait(lambda: len(rows) >= 2, 20, 'lost ACK did not replay durable receipt')
            require(rows[0] == rows[1], 'lost ACK changed the idempotent receipt')

        heads = {}
        for report in sorted(self.usages, key=lambda row: row['sequence']):
            for counter in report['counters']:
                key = (report['xray_epoch'], report['usage_generation_id'], counter['label'])
                values = (counter['uplink_bytes'], counter['downlink_bytes'])
                before = heads.get(key, (0, 0))
                require(all(after >= prior for after, prior in zip(values, before)),
                        'cumulative usage regressed within the same epoch')
                heads[key] = values
        return {'usage_reports': self.usages, 'usage_counter_heads': len(heads), 'elapsed_ms': (self.recovered_at - self.started) * 1000,
                'fault_to_terminal_ms': (self.recovered_at - self.fault_at) * 1000 if self.fault_at else None,
                'old_pid': self.old_pid, 'final_pids': self.pids(), 'observations': self.observations,
                'receipts': self.receipts, 'events': self.events, 'business_restored': True,
                'expected_failure': negative}

    def finish(self):
        traffic = self.traffic.finish() if self.traffic else None
        for process in self.processes:
            if process.poll() is None:
                process.kill()
        for pid in self.pids():
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        if self.state.exists():
            shutil.copytree(self.state, self.output / 'state', dirs_exist_ok=True,
                            ignore=shutil.ignore_patterns('xray', 'xray-release.previous', 'xray-release.candidate', 'xray-release.download', '*.sock', 'control.sock', 'fault-fill'))
        self.http.shutdown()
        return traffic


def inside(args):
    output = Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    lab = None
    result = {'case': args.case, 'passed': False}
    try:
        lab = Lab(args.case, output)
        result.update(lab.run())
        result['passed'] = True
    except Exception as error:
        result['error'] = str(error)
        result['traceback'] = traceback.format_exc()
    finally:
        if lab:
            try:
                generations = list((lab.state / 'xray-handoff').iterdir()) if (lab.state / 'xray-handoff').exists() else []
                result['generation_storage'] = {
                    'directories': len(generations),
                    'executables': sum((generation / 'xray').is_file() for generation in generations),
                    'executable_bytes': sum((generation / 'xray').stat().st_size for generation in generations if (generation / 'xray').is_file()),
                }
                result['traffic'] = lab.finish()
                if result['passed']:
                    for kind, sample in result['traffic']['requests'].items():
                        require(sample['samples'] > 0 and sample['failed'] < sample['samples'], f'{kind} had no successful payloads')
                        require(sample['max_success_gap_ms'] < 12000, f'{kind} service gap exceeded 12 seconds: {sample}')
            except Exception as error:
                result['cleanup_error'] = str(error)
                result['passed'] = False
        (output / 'result.json').write_text(json.dumps(result, indent=2))
        if os.environ.get('LAB_UID'):
            for directory, _, files in os.walk(output):
                os.chown(directory, int(os.environ['LAB_UID']), int(os.environ['LAB_GID']))
                for name in files:
                    os.chown(Path(directory) / name, int(os.environ['LAB_UID']), int(os.environ['LAB_GID']))
        print(json.dumps({key: result.get(key) for key in ['case', 'passed', 'elapsed_ms', 'error', 'cleanup_error']}), flush=True)
    return 0 if result['passed'] else 1


def outside(args):
    output = Path(args.output).resolve()
    require(not output.exists(), f'refusing existing results: {output}')
    output.mkdir(parents=True)
    agent, xray = Path(args.agent).resolve(), Path(args.xray).resolve()
    require(agent.is_file() and xray.is_file(), 'explicit Agent and Xray binaries required')
    require(1 <= args.repeat <= 100 and 1 <= args.jobs <= 8, 'repeat must be 1..100 and jobs 1..8')
    image = subprocess.check_output(['docker', 'image', 'inspect', args.image, '--format', '{{.Id}}'], text=True).strip()
    manifest = {'agent': str(agent), 'xray': str(xray), 'image': image,
                'agent_sha256': hashlib.file_digest(agent.open('rb'), 'sha256').hexdigest(),
                'xray_sha256': hashlib.file_digest(xray.open('rb'), 'sha256').hexdigest(),
                'cases': args.cases.split(',') if args.cases else CASES, 'repeat': args.repeat}
    (output / 'inputs.json').write_text(json.dumps(manifest, indent=2))
    frozen_agent = output / 'agent'
    shutil.copy2(agent, frozen_agent)
    agent = frozen_agent
    frozen_runner = output / 'runner.py'
    shutil.copy2(__file__, frozen_runner)
    manifest['runner_sha256'] = hashlib.file_digest(frozen_runner.open('rb'), 'sha256').hexdigest()
    (output / 'inputs.json').write_text(json.dumps(manifest, indent=2))
    results = []
    def run_case(item):
        iteration, case = item
        require(case in CASES, f'unknown case: {case}')
        case_output = output / f'{iteration:02d}-{case}'
        case_output.mkdir()
        name = f'brocade-fault-{uuid.uuid4().hex[:12]}'
        case_timeout = 180 if case == 'config-repeat' else 100
        command = ['docker', 'run', '--rm', '--init', '--name', name, '--network', 'none', '--cap-add', 'NET_ADMIN', '--pids-limit', '512',
                   '--memory', '768m', '--cpus', args.cpus, '--env', f'LAB_UID={os.getuid()}', '--env', f'LAB_GID={os.getgid()}', '--label', 'brocade.test=xray-faults',
                   '--mount', f'type=bind,src={agent},dst=/inputs/agent,readonly',
                   '--mount', f'type=bind,src={xray},dst=/inputs/xray,readonly',
                   '--mount', f'type=bind,src={frozen_runner},dst=/lab/run.py,readonly',
                   '--mount', f'type=bind,src={case_output},dst=/results',
                   image, 'timeout', str(case_timeout), 'python3', '/lab/run.py', '--inside', '--case', case, '--output', '/results']
        if case.endswith('spool-full'):
            command[2:2] = ['--tmpfs', '/tmp/bf:rw,exec,size=64m,mode=0700']
        began = time.monotonic()
        try:
            completed = subprocess.run(command, capture_output=True, text=True, timeout=case_timeout + 10)
            (case_output / 'docker.log').write_text(completed.stdout + completed.stderr)
            row = read_json(case_output / 'result.json') or {'case': case, 'passed': False, 'error': 'missing result'}
            row.update(iteration=iteration, exit_code=completed.returncode, wall_ms=(time.monotonic()-began)*1000)
            row['passed'] = row['passed'] and completed.returncode == 0
        except subprocess.TimeoutExpired:
            row = {'case': case, 'passed': False, 'error': 'Docker case timeout', 'iteration': iteration}
        finally:
            # Exact randomly owned name only, never broad docker prune/pkill.
            subprocess.run(['docker', 'rm', '-f', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return row
    jobs = [(iteration, case) for iteration in range(args.repeat) for case in manifest['cases']]
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for row in pool.map(run_case, jobs):
            results.append(row)
            print(json.dumps({key: row.get(key) for key in ['case', 'iteration', 'passed', 'elapsed_ms', 'error', 'cleanup_error']}), flush=True)
            (output / 'summary.json').write_text(json.dumps(results, indent=2))

    return 0 if all(row['passed'] for row in results) else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--inside', action='store_true')
    parser.add_argument('--case')
    parser.add_argument('--output', required=True)
    parser.add_argument('--agent')
    parser.add_argument('--xray')
    parser.add_argument('--image', default='brocade-xray-faults:local')
    parser.add_argument('--cases')
    parser.add_argument('--repeat', type=int, default=1)
    parser.add_argument('--cpus', default='2')
    parser.add_argument('--jobs', type=int, default=3)
    arguments = parser.parse_args()
    sys.exit(inside(arguments) if arguments.inside else outside(arguments))
