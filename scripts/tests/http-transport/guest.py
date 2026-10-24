#!/usr/bin/env python3
"""Real-process HTTP acceptance. Run only inside the runner's disposable container/VM."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import re
import shutil
import socket
import socketserver
import subprocess
import threading
import time
import traceback
import uuid


def require(value, message):
    if not value:
        raise RuntimeError(message)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def read_json(path, default=None):
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        return default


def save(path, value, mode=0o644):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2))
    temporary.chmod(mode)
    temporary.replace(path)


def command(*args, timeout=15):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT, timeout=timeout).strip()


def receive(connection, size):
    result = bytearray()
    while len(result) < size:
        chunk = connection.recv(size - len(result))
        require(chunk, 'unexpected TCP EOF')
        result.extend(chunk)
    return bytes(result)


def identity(pid):
    stat = Path(f'/proc/{pid}/stat').read_text().split(')', 1)[1].split()
    return [pid, int(stat[19])]


def metrics(pid):
    status = dict(line.split(':', 1) for line in Path(f'/proc/{pid}/status').read_text().splitlines())
    return {'fds': len(list(Path(f'/proc/{pid}/fd').iterdir())),
            'threads': int(status['Threads']), 'rss_kib': int(status['VmRSS'].split()[0])}


class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(3)
        try:
            while data := self.request.recv(65536):
                self.request.sendall(data)
        except OSError:
            pass


class EchoServer(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


class Traffic:
    def __init__(self, lab):
        self.stop = threading.Event()
        self.tcp_samples, self.udp_samples, self.max_gap = 0, 0, 0
        self.errors = []
        # Xray opens its listener before the Agent has restored dynamic grants.
        deadline = time.monotonic() + 15
        while True:
            self.connection = socket.create_connection(('127.0.0.1', 43103), 2,
                                                       source_address=('198.18.0.2', 0))
            self.connection.settimeout(2)
            try:
                self.connection.sendall(b'\0' + uuid.UUID(lab.user).bytes + b'\0\x01'
                                        + lab.echo.server_address[1].to_bytes(2, 'big') + b'\x01\x7f\0\0\x01')
                self.connection.sendall(b'initial')
                require(receive(self.connection, 9) == b'\0\0initial', 'VLESS handshake/payload mismatch')
                break
            except (OSError, RuntimeError):
                self.connection.close()
                if time.monotonic() >= deadline:
                    raise
                time.sleep(.1)
        self.udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.udp.settimeout(2)
        self.threads = [threading.Thread(target=self.tcp_loop, daemon=True),
                        threading.Thread(target=self.udp_loop, daemon=True)]
        for thread in self.threads:
            thread.start()

    def tcp_loop(self):
        previous = time.monotonic()
        while not self.stop.is_set():
            try:
                payload = self.tcp_samples.to_bytes(8, 'big') + bytes(range(256)) * 4
                self.connection.sendall(payload)
                require(receive(self.connection, len(payload)) == payload, 'TCP payload mismatch')
                now = time.monotonic()
                self.max_gap = max(self.max_gap, now - previous)
                previous = now
                self.tcp_samples += 1
                self.stop.wait(.1)
            except Exception as error:
                if not self.stop.is_set():
                    self.errors.append(str(error))
                return

    def udp_loop(self):
        while not self.stop.is_set():
            try:
                payload = self.udp_samples.to_bytes(8, 'big') + b'udp-acceptance'
                self.udp.sendto(payload, ('127.0.0.1', 43104))
                require(self.udp.recv(4096) == payload, 'UDP payload mismatch')
                self.udp_samples += 1
                self.stop.wait(.2)
            except Exception as error:
                if not self.stop.is_set():
                    self.errors.append(str(error))
                return

    def snapshot(self):
        require(not self.errors, f'business traffic failed: {self.errors}')
        return {'tcp_samples': self.tcp_samples, 'udp_samples': self.udp_samples,
                'max_gap_secs': self.max_gap, 'errors': list(self.errors)}

    def close(self):
        self.stop.set()
        self.connection.close()
        self.udp.close()
        for thread in self.threads:
            thread.join(3)


class Lab:
    def __init__(self, output, resumed):
        self.output, self.resumed = output, resumed
        self.records = read_json(output / 'evidence.json', [])
        self.root = Path('/var/lib/brocade-http-acceptance')
        self.state = self.root / 'state'
        self.state.mkdir(parents=True, mode=0o700, exist_ok=True)
        self.installed = self.root / 'bin'
        self.installed.mkdir(mode=0o700, exist_ok=True)
        for name in ['agent', 'xray', 'legacy-xray', 'agent-tests']:
            destination = self.installed / name
            shutil.copyfile(Path('/inputs') / name, destination)
            destination.chmod(0o755)
        self.agent, self.xray = self.installed / 'agent', self.installed / 'xray'
        self.lock = threading.RLock()
        self.offline = resumed
        self.runtime_mode = 'healthy' if resumed else 'stall'
        self.usage_modes = queue.Queue()
        self.faults, self.requests, self.runtime_reports = [], [], []
        self.ledger = read_json(self.root / 'ledger.json', {})
        self.errors = []
        self.user = 'b831381d-6324-4d53-ad4f-000000000001'
        self.label = 'alice@platform.acme#ing-a1b2'
        self.processes, self.traffic = [], None
        self.boot_id = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        self.env = {**os.environ, 'BROCADE_XRAY_BIN': str(self.xray),
                    'HTTP_PROXY': 'http://127.0.0.1:1', 'HTTPS_PROXY': 'http://127.0.0.1:1',
                    'ALL_PROXY': 'http://127.0.0.1:1', 'NO_PROXY': ''}
        if not command('ip', 'route', 'show', 'default'):
            command('ip', 'link', 'add', 'http-meter0', 'type', 'dummy')
            command('ip', 'link', 'set', 'http-meter0', 'up')
            command('ip', 'addr', 'add', '198.18.0.1/32', 'dev', 'http-meter0')
            command('ip', 'route', 'add', 'default', 'dev', 'http-meter0')
        # Xray deliberately excludes 127.0.0.1 from online presence. Keep this
        # synthetic source wholly inside the disposable network namespace.
        command('ip', 'addr', 'add', '198.18.0.2/32', 'dev', 'lo')
        self.echo = EchoServer(('127.0.0.1', 43101), Echo)
        threading.Thread(target=self.echo.serve_forever, daemon=True).start()
        self.udp_echo = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.udp_echo.bind(('127.0.0.1', 43102))
        def udp_echo():
            while True:
                try:
                    data, peer = self.udp_echo.recvfrom(4096)
                    self.udp_echo.sendto(data, peer)
                except OSError:
                    return
        threading.Thread(target=udp_echo, daemon=True).start()
        lab = self
        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def log_message(self, *_args):
                pass
            def handle_request(self):
                self.connection.settimeout(3)
                try:
                    length = int(self.headers.get('content-length', '0'))
                    require(length <= 32 * 1024**2, 'oversize fixture request')
                    raw = self.rfile.read(length)
                    require(len(raw) == length, 'truncated fixture request')
                    body = json.loads(raw) if raw else None
                    with lab.lock:
                        lab.requests.append({'path': self.path, 'at': time.time(), 'offline': lab.offline})
                        if lab.offline:
                            self.reply(503)
                            return
                        mode = 'healthy'
                        if self.path == '/agent/v1/usage':
                            key = f"{body['agent_instance_id']}:{body['sequence']}"
                            hashed = hashlib.sha256(raw).hexdigest()
                            receipt = lab.ledger.get(key)
                            if receipt:
                                require(receipt['sha256'] == hashed, 'idempotency key reused with different bytes')
                                receipt['attempts'] += 1
                            else:
                                lab.ledger[key] = {'sha256': hashed, 'body': body, 'attempts': 1}
                            save(lab.root / 'ledger.json', lab.ledger)
                            if not lab.usage_modes.empty():
                                mode = lab.usage_modes.get_nowait()
                        elif self.path == '/agent/v1/runtime':
                            mode, lab.runtime_mode = lab.runtime_mode, 'healthy'
                            if mode == 'healthy':
                                lab.runtime_reports.append(body)
                    if mode != 'healthy':
                        lab.inject(self, mode, body, raw)
                        return
                    self.reply(204)
                except (BrokenPipeError, ConnectionResetError, TimeoutError):
                    self.close_connection = True
                except Exception as error:
                    lab.errors.append(str(error))
                    self.close_connection = True
            def reply(self, status):
                self.send_response(status)
                self.send_header('Content-Length', '0')
                self.send_header('Connection', 'close')
                self.end_headers()
                self.close_connection = True
            do_GET = do_POST = handle_request
        self.http = http.server.ThreadingHTTPServer(('127.0.0.1', 43110), Handler)
        self.http.daemon_threads = True
        threading.Thread(target=self.http.serve_forever, daemon=True).start()
        if not resumed:
            save(self.state / 'xray.json', {
                'log': {'loglevel': 'warning'}, 'stats': {},
                'api': {'tag': 'api', 'services': ['HandlerService', 'StatsService']},
                'policy': {'levels': {'0': {'statsUserUplink': True, 'statsUserDownlink': True,
                                          'statsUserOnline': True}}},
                'inbounds': [
                    {'tag': 'api', 'listen': '127.0.0.1', 'port': 43100,
                     'protocol': 'dokodemo-door', 'settings': {'address': '127.0.0.1'}},
                    {'tag': 'in:brc/ing-a1b2', 'listen': '127.0.0.1', 'port': 43103,
                     'protocol': 'vless', 'settings': {'clients': [], 'decryption': 'none'}},
                    {'tag': 'udp', 'listen': '127.0.0.1', 'port': 43104, 'protocol': 'dokodemo-door',
                     'settings': {'address': '127.0.0.1', 'port': 43102, 'network': 'udp'}},
                ], 'outbounds': [{'tag': 'direct', 'protocol': 'freedom'}],
                'routing': {'rules': [{'type': 'field', 'inboundTag': ['api'], 'outboundTag': 'api'}]},
            }, mode=0o600)
            save(self.state / 'grants.desired.json', [{'tag': 'in:brc/ing-a1b2', 'clients': [
                {'email': self.label, 'uuid': self.user, 'flow': None, 'level': 0}]}], mode=0o600)
            (self.state / 'usage-generation').write_text('1')
        self.start_agent()
        self.wait(lambda: self.connectable(43103), 20, 'managed Xray startup')
        self.traffic = Traffic(self)

    @staticmethod
    def connectable(port):
        try:
            with socket.create_connection(('127.0.0.1', port), .2):
                return True
        except OSError:
            return False

    def wait(self, predicate, timeout, message):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            require(not self.errors, str(self.errors))
            require(self.agent_process.poll() is None, 'Agent exited unexpectedly')
            if self.traffic:
                self.traffic.snapshot()
            value = predicate()
            if value:
                return value
            time.sleep(.1)
        raise RuntimeError('timeout: ' + message)

    def record(self, case, **details):
        self.records.append({'case': case, 'at': time.time(), **details})
        save(self.output / 'evidence.json', self.records)
        print(json.dumps({'case': case, **details}), flush=True)

    def start_agent(self, retry=True):
        token = self.root / 'node-token'
        token.write_text('broc_node_disposable_http_acceptance')
        token.chmod(0o600)
        log = (self.output / 'agent.log').open('ab')
        self.agent_process = subprocess.Popen([
            str(self.agent), 'run', '--state-dir', str(self.state), '--server',
            'http://127.0.0.1:43110', '--token-file', str(token), '--apply', 'state-dir'],
            env=self.env, stdout=log, stderr=subprocess.STDOUT)
        self.processes.append(self.agent_process)
        log.close()
        time.sleep(.3)
        if self.agent_process.poll() == 0 and retry:
            self.start_agent(retry=False)

    def pids(self):
        found = self.xray_processes()
        require(len(found) == 1, f'expected exactly one managed Xray: {found}')
        return {'agent': identity(self.agent_process.pid),
                'xray': found[0]}

    def xray_processes(self):
        found = []
        for entry in Path('/proc').iterdir():
            if not entry.name.isdigit():
                continue
            try:
                args = (entry / 'cmdline').read_bytes().split(b'\0')
                stat = (entry / 'stat').read_text().rsplit(') ', 1)[1].split()
                if (str(self.xray).encode() in args and b'run' in args and
                        str(self.state / 'xray.json').encode() in args and stat[0] != 'Z'):
                    found.append(identity(int(entry.name)))
            except (FileNotFoundError, ProcessLookupError):
                continue
        return found

    def pending(self):
        path = self.state / 'usage.spool.jsonl'
        try:
            return [json.loads(line) for line in path.read_text().splitlines() if line]
        except FileNotFoundError:
            return []

    def inject(self, handler, mode, body, raw):
        event = {'path': handler.path, 'mode': mode, 'at': time.time(),
                 'body_sha256': hashlib.sha256(raw).hexdigest(), 'body': body}
        with self.lock:
            self.faults.append(event)
        if mode == 'drop':
            handler.connection.shutdown(socket.SHUT_RDWR)
            handler.close_connection = True
            event['closed_at'] = time.time()
            return
        if mode == '503':
            handler.reply(503)
            event['closed_at'] = time.time()
            return
        if mode in ['drip', 'usage-timeout', 'truncate']:
            handler.send_response(200)
            handler.send_header('Content-Length', '4096')
            handler.send_header('Connection', 'close')
            handler.end_headers()
            handler.wfile.write(b'{')
            handler.wfile.flush()
        if mode == 'truncate':
            handler.connection.shutdown(socket.SHUT_RDWR)
            handler.close_connection = True
            event['closed_at'] = time.time()
            return
        handler.connection.settimeout(.2)
        began = time.monotonic()
        while time.monotonic() - began < 38:
            try:
                if mode == 'drip':
                    handler.wfile.write(b'x')
                    handler.wfile.flush()
                if handler.connection.recv(1, socket.MSG_PEEK) == b'':
                    break
            except TimeoutError:
                continue
            except (BrokenPipeError, ConnectionResetError):
                break
        else:
            self.errors.append('client failed to close timed-out request')
        handler.close_connection = True
        event['closed_at'] = time.time()

    def fault(self, path, mode):
        return next((event for event in self.faults if event['path'] == path and event['mode'] == mode), None)

    def online_snapshot(self):
        if not self.runtime_reports:
            return None
        sources = self.runtime_reports[-1]['online_sources']
        if sources and any(row['label'] == self.label and any(
                source['ip'] == '198.18.0.2' and 'vless' in (source.get('protocols') or [])
                for source in row['sources']) for row in sources):
            return sources
        return None

    def daemon_cases(self):
        self.wait(lambda: self.fault('/agent/v1/runtime', 'stall'), 20, 'runtime stall starts')
        baseline = self.pids()
        before = self.traffic.snapshot()
        baseline_resources = metrics(self.agent_process.pid)
        self.wait(lambda: self.fault('/agent/v1/runtime', 'stall').get('closed_at'), 40, 'runtime hard timeout')
        event = self.fault('/agent/v1/runtime', 'stall')
        duration = event['closed_at'] - event['at']
        require(28 <= duration <= 35, f'runtime request deadline: {duration}')
        self.wait(lambda: self.runtime_reports, 8, 'runtime retries without restart')
        require(self.pids() == baseline, 'normal timeout restarted Agent or Xray')
        during = [row for row in self.requests if event['at'] < row['at'] < event['closed_at']]
        require(any(row['path'] == '/agent/v1/usage' for row in during), 'usage blocked behind runtime')
        require(any(row['path'] == '/agent/v1/desired' for row in during), 'desired blocked behind runtime')
        after = self.traffic.snapshot()
        require(after['tcp_samples'] > before['tcp_samples'] + 100, 'existing TCP flow stopped during stall')
        require(after['udp_samples'] > before['udp_samples'] + 50, 'UDP stopped during stall')
        require(self.runtime_reports[-1]['observed_at_unix_secs'] > event['body']['observed_at_unix_secs'],
                'runtime retried an obsolete snapshot after recovery')
        online = self.wait(self.online_snapshot, 10, 'recovered report includes the live VLESS source')
        self.record('runtime-stall-isolated-and-recovers', deadline_secs=duration,
                    pids_before=baseline, pids_after=self.pids(), traffic_before=before, traffic_after=after,
                    online_sources=online,
                    concurrent_usage=sum(row['path'] == '/agent/v1/usage' for row in during),
                    concurrent_desired=sum(row['path'] == '/agent/v1/desired' for row in during))
        with self.lock:
            self.runtime_mode = 'drip'
        self.wait(lambda: self.fault('/agent/v1/runtime', 'drip'), 40, 'drip starts')
        self.wait(lambda: self.fault('/agent/v1/runtime', 'drip').get('closed_at'), 40, 'drip hard timeout')
        event = self.fault('/agent/v1/runtime', 'drip')
        duration = event['closed_at'] - event['at']
        require(28 <= duration <= 35, f'progress extended runtime deadline: {duration}')
        count = len(self.runtime_reports)
        self.wait(lambda: len(self.runtime_reports) > count, 10, 'runtime recovers after dripped response')
        online = self.wait(self.online_snapshot, 10, 'drip recovery includes the live VLESS source')
        require(self.pids() == baseline, 'dripped response restarted a process')
        resources = metrics(self.agent_process.pid)
        require(resources['fds'] <= baseline_resources['fds'] + 12, 'descriptor accumulation after timeouts')
        require(resources['threads'] <= baseline_resources['threads'] + 6, 'thread accumulation after timeouts')
        self.record('runtime-drip-deadline-and-resource-recovery', deadline_secs=duration,
                    baseline=baseline_resources, recovered=resources, pids=self.pids(), online_sources=online)
        for mode in ['drop', 'truncate', '503', 'usage-timeout']:
            self.usage_modes.put(mode)
            event = self.wait(lambda: self.fault('/agent/v1/usage', mode), 40, f'usage {mode} begins')
            key = f"{event['body']['agent_instance_id']}:{event['body']['sequence']}"
            self.wait(lambda: any(f"{row['agent_instance_id']}:{row['sequence']}" == key for row in self.pending()),
                      5, 'unconfirmed usage retained on disk')
            if mode == 'usage-timeout':
                self.wait(lambda: event.get('closed_at'), 40, 'usage complete-body timeout')
                require(28 <= event['closed_at'] - event['at'] <= 35, 'usage body not bounded by total timeout')
            self.wait(lambda: self.ledger[key]['attempts'] >= 2, 12, f'usage {mode} retry')
            self.wait(lambda: not any(f"{row['agent_instance_id']}:{row['sequence']}" == key for row in self.pending()),
                      5, 'fully acknowledged usage removed')
            receipt = self.ledger[key]
            require(receipt['sha256'] == event['body_sha256'], 'retried bytes changed')
            self.record('usage-' + mode + '-ack', key=key, payload_sha256=receipt['sha256'],
                        attempts=receipt['attempts'], unique_commits=1, retained_before_ack=True,
                        removed_after_complete_ack=True, pids=self.pids(),
                        fault_duration_secs=event['closed_at'] - event['at'])
        self.offline = True
        owed = self.wait(self.pending, 40, 'offline durable backlog')
        old_identity = self.pids()
        self.agent_process.kill()
        self.agent_process.wait(5)
        self.start_agent()
        self.wait(lambda: self.connectable(43103), 15, 'Agent restart attaches to live Xray')
        require(self.pids()['agent'] != old_identity['agent'], 'Agent did not actually restart')
        require(self.pids()['xray'] == old_identity['xray'], 'Agent crash restarted serving Xray')
        expected = {f"{row['agent_instance_id']}:{row['sequence']}": hashlib.sha256(json.dumps(row, separators=(',', ':')).encode()).hexdigest() for row in owed}
        retained = self.pending()
        require(all(any(row == candidate for candidate in retained) for row in owed), 'Agent restart lost a queued record')
        self.offline = False
        self.wait(lambda: all(key in self.ledger for key in expected), 20, 'backlog delivers after recovery')
        self.wait(lambda: not self.pending(), 15, 'backlog drains')
        self.record('agent-crash-preserves-backlog-and-live-xray', pids_before=old_identity,
                    pids_after=self.pids(), retained_keys=sorted(expected), traffic=self.traffic.snapshot())
        # Leave a real unacknowledged checkpoint for a container restart or actual VM reboot.
        self.offline = True
        owed = self.wait(self.pending, 40, 'durable backlog before reboot')
        save(self.root / 'resume.json', {'boot_id': self.boot_id, 'owed': owed,
                                       'old_pids': self.pids(), 'ledger_keys': sorted(self.ledger)})
        self.record('ready-for-real-reboot', owed_keys=[f"{row['agent_instance_id']}:{row['sequence']}" for row in owed],
                    boot_id=self.boot_id, traffic=self.traffic.snapshot())
        save(self.output / 'reboot.json', {'boot_id': self.boot_id})

    def resume_case(self):
        before = read_json(self.root / 'resume.json')
        if os.environ['BROCADE_HTTP_ACCEPTANCE_RUNTIME'] == 'kvm':
            require(before['boot_id'] != self.boot_id, 'KVM did not actually reboot')
        self.wait(lambda: all(any(row == candidate for candidate in self.pending()) for row in before['owed']),
                  10, 'offline reboot preserves exact queued checkpoints')
        keys = [f"{row['agent_instance_id']}:{row['sequence']}" for row in before['owed']]
        self.offline = False
        self.wait(lambda: all(key in self.ledger for key in keys), 20, 'reboot backlog delivery')
        self.wait(lambda: not self.pending(), 15, 'reboot backlog drains')
        require(set(before['ledger_keys']).issubset(self.ledger), 'receipt ledger was lost across reboot')
        self.record('offline-reboot-backlog-delivered', old_boot_id=before['boot_id'], new_boot_id=self.boot_id,
                    retained_keys=keys, traffic=self.traffic.snapshot())
        for key, receipt in self.ledger.items():
            body = receipt['body']
            require(key == f"{body['agent_instance_id']}:{body['sequence']}", 'receipt identity mismatch')
        save(self.output / 'ledger.json', self.ledger)
        save(self.output / 'http-requests.json', self.requests)
        save(self.output / 'result.json', {'passed': True, 'cases': len(self.records)})

    def close(self):
        if self.traffic:
            self.traffic.close()
        for process in reversed(self.processes):
            if process.poll() is None:
                process.kill()
                process.wait(5)
        for pid, ticks in self.xray_processes():
            try:
                if identity(pid) == [pid, ticks]:
                    os.kill(pid, 9)
            except (FileNotFoundError, ProcessLookupError):
                pass
        self.http.shutdown()
        self.echo.shutdown()
        self.udp_echo.close()


def run_tests(output):
    tests = '/var/lib/brocade-http-test-bin'
    Path(tests).mkdir(mode=0o700, exist_ok=True)
    env = {**os.environ}
    for name in ['agent-tests', 'xray', 'legacy-xray']:
        destination = Path(tests) / name
        shutil.copyfile(Path('/inputs') / name, destination)
        destination.chmod(0o755)
    env.update({'BROCADE_TEST_XRAY': tests + '/xray', 'BROCADE_XRAY_BIN': tests + '/xray',
                'BROCADE_TEST_LEGACY_XRAY': tests + '/legacy-xray'})
    groups = ['http::', 'desired_delivery_tests::', 'spool::tests::']
    results = []
    for group in groups:
        log = output / ('tests-' + group.replace(':', '_') + '.log')
        with log.open('w') as destination:
            result = subprocess.run([tests + '/agent-tests', group, '--test-threads=4'], env=env,
                                    stdout=destination, stderr=subprocess.STDOUT, timeout=100)
        text = log.read_text()
        match = re.search(r'test result: ok\. (\d+) passed; 0 failed; 0 ignored;', text)
        require(result.returncode == 0 and match and int(match[1]) > 0, f'failed transport group {group}: {text[-2000:]}')
        results.append({'filter': group, 'passed': int(match[1]), 'log': log.name})
        save(output / 'test-progress.json', results)
        print(json.dumps({'case': 'transport-test-group', 'filter': group, 'passed': int(match[1])}), flush=True)
    listing = subprocess.check_output([tests + '/agent-tests', '--list', '--ignored'], text=True, env=env)
    names = [line[:-6] for line in listing.splitlines() if line.endswith(': test')]
    excluded = 'traffic_limit::tests::traffic_limit_system_real_kernel_reconciles_caps_and_preserves_external_qos'
    require(excluded in names and len(names) > 1 and len(set(names)) == len(names),
            'native test inventory is empty, incomplete or duplicated')
    for name in names:
        if name == excluded:
            continue
        log = output / ('native-' + name.replace(':', '_') + '.log')
        with log.open('w') as destination:
            result = subprocess.run([tests + '/agent-tests', name, '--exact', '--ignored', '--test-threads=1'],
                                    env=env, stdout=destination, stderr=subprocess.STDOUT, timeout=150)
        text = log.read_text()
        require(result.returncode == 0 and 'test result: ok. 1 passed; 0 failed; 0 ignored;' in text,
                f'failed native integration {name}: {text[-2500:]}')
        results.append({'filter': name, 'passed': 1, 'log': log.name})
        save(output / 'test-progress.json', results)
        print(json.dumps({'case': 'native-integration', 'name': name, 'passed': True}), flush=True)
    save(output / 'test-results.json', {'groups': results, 'excluded_guest_docker_daemon_test': excluded,
                                       'ignored_inventory': names})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    require(os.environ.get('BROCADE_HTTP_ACCEPTANCE_DISPOSABLE') == '1', 'disposable guest marker required')
    require(Path('/inputs/agent').is_file(), 'frozen guest inputs required; never run on host')
    args.output.mkdir(parents=True, exist_ok=True)
    lab = None
    try:
        resumed = Path('/var/lib/brocade-http-acceptance/resume.json').exists()
        if not resumed:
            run_tests(args.output)
        lab = Lab(args.output, resumed)
        if resumed:
            lab.resume_case()
        else:
            lab.record('environment', kernel=os.uname().release, os=Path('/etc/os-release').read_text(),
                       boot_id=lab.boot_id, agent_sha256=digest(lab.agent), xray_sha256=digest(lab.xray))
            lab.daemon_cases()
    except Exception:
        (args.output / 'failure.txt').write_text(traceback.format_exc())
        raise
    finally:
        if lab:
            lab.close()


if __name__ == '__main__':
    main()
