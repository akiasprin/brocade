#!/usr/bin/env python3
"""Real Agent/Xray acceptance inside an explicitly disposable Linux guest only."""
import argparse
import concurrent.futures
import datetime
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import socket
import socketserver
import stat
import struct
import subprocess
import threading
import time
import traceback


def require(value, message):
    if not value:
        raise RuntimeError(message)


def command(*args):
    return subprocess.check_output(args, stderr=subprocess.STDOUT, text=True, timeout=15).strip()


def read_json(path):
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
        with os.fdopen(descriptor, 'rb') as source:
            if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
                return None
            data = source.read(128 * 1024 + 1)
            return json.loads(data) if len(data) <= 128 * 1024 else None
    except (OSError, ValueError):
        return None


def write_json(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value))
    temporary.chmod(0o600)
    owner = path.parent.stat()
    os.chown(temporary, owner.st_uid, owner.st_gid)
    temporary.replace(path)


def receive(connection, size):
    chunks = bytearray()
    while len(chunks) < size:
        data = connection.recv(min(65536, size - len(chunks)))
        require(data, 'unexpected TCP EOF')
        chunks.extend(data)
    return chunks


class Peer(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(45)
        try:
            kind, size = struct.unpack('!cQ', receive(self.request, 9))
            block = bytes(range(256)) * 256
            if kind == b'D':
                remaining = size
                while remaining:
                    part = block[:min(remaining, len(block))]
                    self.request.sendall(part)
                    remaining -= len(part)
            elif kind == b'U':
                require(receive(self.request, size) == (block * ((size + 65535) // 65536))[:size],
                        'upload payload corruption')
                self.request.sendall(b'K')
            elif kind == b'E':
                self.request.sendall(receive(self.request, size))
        except (OSError, RuntimeError):
            return


class PeerServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def peer():
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(('0.0.0.0', 45002))
    def echo():
        while True:
            data, address = udp.recvfrom(65536)
            udp.sendto(data, address)
    threading.Thread(target=echo, daemon=True).start()
    PeerServer(('0.0.0.0', 45001), Peer).serve_forever()


class Lab:
    def __init__(self, output):
        self.output = output
        output.mkdir(parents=True, exist_ok=True)
        self.state = Path('/var/lib/brocade-traffic-acceptance')
        self.state.mkdir(mode=0o700, exist_ok=True)
        installed = self.state / 'bin'
        installed.mkdir(mode=0o700, exist_ok=True)
        for name in ['agent', 'xray']:
            target = installed / name
            shutil.copyfile(Path('/inputs') / name, target)
            target.chmod(0o755)
        self.agent_binary = str(installed / 'agent')
        self.xray_binary = str(installed / 'xray')
        self.records = read_json(output / 'evidence.json') or []
        self.processes = []
        self.offline = False
        self.limit_response_mode = 'normal'
        self.limit_get_history = []
        self.desired = read_json(self.state / 'traffic-limit-desired.json')
        self.work = None
        self.receipts, self.runtime, self.observations = [], [], []
        self.revision = self.desired['revision'] if self.desired else 0
        self.boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        # Docker preserves the writable /run directory across a container restart. Remove only
        # this fixture's stale namespace name before recreating its isolated peer.
        if Path('/run/netns/bc-limit-peer').exists():
            command('ip', 'netns', 'del', 'bc-limit-peer')
        if Path('/sys/class/net/limit0').exists():
            command('ip', 'link', 'del', 'limit0')
        command('ip', 'netns', 'add', 'bc-limit-peer')
        command('ip', 'link', 'add', 'limit0', 'type', 'veth', 'peer', 'name', 'peer0')
        command('ip', 'link', 'set', 'peer0', 'netns', 'bc-limit-peer')
        command('ip', 'addr', 'add', '198.18.0.1/30', 'dev', 'limit0')
        command('ip', 'link', 'set', 'limit0', 'up')
        command('ip', 'route', 'replace', 'default', 'via', '198.18.0.2', 'dev', 'limit0')
        for args in [('addr', 'add', '198.18.0.2/30', 'dev', 'peer0'),
                     ('link', 'set', 'peer0', 'up'), ('link', 'set', 'lo', 'up')]:
            command('ip', 'netns', 'exec', 'bc-limit-peer', 'ip', *args)
        self.processes.append(subprocess.Popen(['ip', 'netns', 'exec', 'bc-limit-peer',
                                               'python3', __file__, '--peer']))
        self.wait(lambda: self.connectable('198.18.0.2', 45001), 10, 'peer startup')
        self.record('environment', kernel=os.uname().release,
                    os=Path('/etc/os-release').read_text(), boot_id=self.boot,
                    tc=command('tc', '-V'))
        lab = self
        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def log_message(self, *_args):
                pass
            def handle_request(self):
                size = int(self.headers.get('content-length', '0'))
                body = json.loads(self.rfile.read(size)) if size else None
                reply, status = None, 204
                if lab.offline:
                    status = 503
                elif self.path == '/agent/v1/traffic-limit':
                    if self.command == 'GET':
                        reply = lab.desired
                    else:
                        lab.receipts.append(body)
                elif self.path == '/agent/v1/desired':
                    reply = lab.work
                elif self.path == '/agent/v1/observation':
                    lab.observations.append(body)
                    lab.work = None
                elif self.path == '/agent/v1/runtime':
                    lab.runtime.append(body)
                if reply is not None:
                    status = 200
                elif self.path == '/agent/v1/traffic-limit' and self.command == 'GET' and status == 204:
                    status = 404
                encoded = json.dumps(reply).encode() if reply is not None else b''
                if self.path == '/agent/v1/traffic-limit' and self.command == 'GET':
                    lab.limit_get_history.append({'revision': reply.get('revision') if reply else None,
                                                  'mode': lab.limit_response_mode})
                    if lab.limit_response_mode == 'truncated' and status == 200:
                        self.send_response(status)
                        self.send_header('content-length', str(len(encoded) + 32))
                        self.send_header('connection', 'close')
                        self.end_headers()
                        self.wfile.write(encoded[:len(encoded) // 2])
                        self.close_connection = True
                        return
                self.send_response(status)
                self.send_header('content-length', str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)
            do_GET = do_POST = handle_request
        self.http = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.http.serve_forever, daemon=True).start()
        config = self.state / 'xray.json'
        if not config.exists():
            write_json(config, {
                'log': {'loglevel': 'warning'}, 'stats': {},
                'api': {'tag': 'api', 'services': ['HandlerService', 'StatsService']},
                'inbounds': [
                    {'tag': 'api', 'listen': '127.0.0.1', 'port': 45000,
                     'protocol': 'dokodemo-door', 'settings': {'address': '127.0.0.1'}},
                    {'tag': 'tcp', 'listen': '127.0.0.1', 'port': 45003,
                     'protocol': 'dokodemo-door', 'settings': {
                         'address': '198.18.0.2', 'port': 45001, 'network': 'tcp'}},
                    {'tag': 'udp', 'listen': '127.0.0.1', 'port': 45004,
                     'protocol': 'dokodemo-door', 'settings': {
                         'address': '198.18.0.2', 'port': 45002, 'network': 'udp'}},
                ],
                'outbounds': [{'tag': 'direct', 'protocol': 'freedom'}],
                'routing': {'rules': [{'type': 'field', 'inboundTag': ['api'], 'outboundTag': 'api'}]},
            })
        self.start_agent()
        self.wait(lambda: (self.state / 'traffic-meter.json').exists(), 30, 'durable traffic meter')
        self.wait(lambda: self.connectable('127.0.0.1', 45003), 30, 'real Xray startup')

    @staticmethod
    def connectable(host, port):
        try:
            with socket.create_connection((host, port), .2):
                return True
        except OSError:
            return False

    def record(self, case, **details):
        self.records.append({'case': case, 'at': time.time(), **details})
        write_json(self.output / 'evidence.json', self.records)
        print(json.dumps({'case': case, **details}), flush=True)

    @staticmethod
    def wait(check, timeout, message):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            result = check()
            if result:
                return result
            time.sleep(.1)
        raise RuntimeError('timeout: ' + message)

    def start_agent(self, require_meter=True):
        deadline = time.monotonic() + 35
        previous = (read_json(self.state / 'traffic-meter.json') or {}).get('sequence', 0) if require_meter else 0
        runtime_count = len(self.runtime)
        while time.monotonic() < deadline:
            with (self.output / 'agent.log').open('ab') as log:
                self.agent = subprocess.Popen([
                    self.agent_binary, 'run', '--state-dir', str(self.state), '--server',
                    f'http://127.0.0.1:{self.http.server_port}', '--token', 'broc_node_disposable_limit_lab',
                    '--apply', 'linux'], env={**os.environ, 'BROCADE_XRAY_BIN': self.xray_binary},
                    stdout=log, stderr=subprocess.STDOUT)
            self.processes.append(self.agent)
            while time.monotonic() < deadline:
                code = self.agent.poll()
                if code is not None:
                    require(code == 0, f'Agent failed during startup: {code}')
                    break  # Supervise a clean self-requested restart, as the installed service does.
                meter = read_json(self.state / 'traffic-meter.json') if require_meter else None
                if (meter and meter['sequence'] > previous) or (not require_meter and len(self.runtime) > runtime_count):
                    return
                time.sleep(.1)
        raise RuntimeError('Agent startup did not produce a fresh durable meter')

    def policy(self, layer, rules, enabled=True, reset_anchor=True):
        self.revision += 1
        if reset_anchor or self.desired is None:
            meter = read_json(self.state / 'traffic-meter.json')
            now = datetime.datetime.now(datetime.timezone.utc)
            period = int(now.replace(day=1, hour=0, minute=0, second=0, microsecond=0).timestamp())
            self.desired = {
                'revision': self.revision, 'quota_bytes': 5 * 1024**2,
                'cycle_kind': 'monthly', 'accounting_mode': 'bidirectional_max',
                'reset_month': None, 'reset_day': 1,
                'anchor': {
                    'meter_id': meter['meter_id'], 'sequence': meter['sequence'],
                    'sampled_at_unix_secs': meter['sampled_at_unix_secs'],
                    'period_start_unix_secs': period,
                    'meter_rx_bytes': meter['total_rx_bytes'], 'meter_tx_bytes': meter['total_tx_bytes'],
                    'rx_bytes': 0, 'tx_bytes': 0, 'calibrated_total_bytes': 0,
                },
            }
        self.desired['revision'] = self.revision
        self.desired['policy'] = {'enabled': enabled, 'layer': layer, 'rules': rules}
        self.wait(lambda: (read_json(self.state / 'traffic-limit-desired.json') or {}).get('revision')
                  == self.revision, 35, 'independent policy delivery')

    def status(self, phase, rate=None):
        def check():
            report = read_json(self.state / 'traffic-limit-status.json')
            return report if (report and report['revision'] == self.revision
                              and report['phase'] == phase
                              and (rate is None or report['rate_bps'] == rate)) else None
        return self.wait(check, 40, f'local limit {phase}/{rate}')

    def transfer(self, direction, size, application=False):
        address = ('127.0.0.1', 45003) if application else ('198.18.0.2', 45001)
        block = bytes(range(256)) * 256
        with socket.create_connection(address, 3) as connection:
            connection.settimeout(45)
            connection.sendall(struct.pack('!cQ', direction.encode(), size))
            start = time.monotonic()
            if direction == 'D':
                data = receive(connection, size)
                require(data == (block * ((size + 65535) // 65536))[:size], 'download corruption')
            else:
                connection.sendall((block * ((size + 65535) // 65536))[:size])
                require(receive(connection, 1) == b'K', 'upload missing acknowledgment')
            return size * 8 / (time.monotonic() - start)

    @staticmethod
    def absolute(rate, threshold=1):
        return {'threshold': {'kind': 'absolute', 'bytes': str(threshold)}, 'rate_bps': rate}

    def throughput(self, name, rate, size, application=False):
        values = {direction: self.transfer(direction, size, application) for direction in ['D', 'U']}
        require(all(rate * .45 < value < rate * 1.35 for value in values.values()),
                f'{name}: unexpected directional throughput {values}, expected {rate}')
        self.record(name, target_bps=rate, measured_bps=values)

    def udp(self, size, timeout):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as client:
            client.settimeout(timeout)
            payload = b'\x5a' * size
            start = time.monotonic()
            client.sendto(payload, ('127.0.0.1', 45004))
            require(client.recv(65536) == payload, 'UDP payload/framing corruption')
            return time.monotonic() - start

    def fault_injection(self):
        # Only the disposable guest's files, private NIC, fixture and managed processes change.
        cached = json.loads(json.dumps(self.desired))
        history = len(self.limit_get_history)
        invalid = json.loads(json.dumps(cached))
        invalid['revision'] += 1
        invalid['policy']['rules'][0]['rate_bps'] = 0
        self.desired = invalid
        self.wait(lambda: any(row['revision'] == invalid['revision']
                             for row in self.limit_get_history[history:]), 35, 'invalid policy delivery')
        time.sleep(.5)
        require(read_json(self.state / 'traffic-limit-desired.json') == cached,
                'an invalid policy replaced the last durable valid policy')
        require(self.udp(128, 5) < 5, 'invalid delivery broke the valid active application cap')
        self.record('invalid-delivery-keeps-last-policy', rejected_revision=invalid['revision'])
        self.desired = cached
        history = len(self.limit_get_history)
        self.limit_response_mode = 'truncated'
        self.wait(lambda: any(row['mode'] == 'truncated' for row in self.limit_get_history[history:]),
                  35, 'truncated policy response')
        time.sleep(.5)
        require(read_json(self.state / 'traffic-limit-desired.json') == cached,
                'a truncated response damaged the durable policy')
        require(self.udp(128, 5) < 5, 'truncated delivery broke the application data path')
        self.record('truncated-delivery-keeps-last-policy')
        self.limit_response_mode = 'normal'
        stale = json.loads(json.dumps(cached))
        stale['revision'] -= 1
        stale['policy'] = {'enabled': False, 'layer': 'application', 'rules': []}
        history = len(self.limit_get_history)
        self.desired = stale
        self.wait(lambda: any(row['revision'] == stale['revision']
                             for row in self.limit_get_history[history:]), 35, 'stale policy delivery')
        time.sleep(.5)
        require(read_json(self.state / 'traffic-limit-desired.json') == cached,
                'a stale revision removed the active cap')
        self.throughput('stale-policy-keeps-active-cap', 800_000, 128 * 1024, True)
        self.desired = cached
        self.offline = True
        time.sleep(.5)  # Let an in-flight successful sync finish before replacing its cache.
        policy_cache = self.state / 'traffic-limit-desired.json'
        policy_cache.unlink()
        os.mkfifo(policy_cache, 0o600)
        meter = read_json(self.state / 'traffic-meter.json')
        try:
            self.transfer('D', 64 * 1024, True)
            next_meter = self.wait(lambda: (value if (value := read_json(self.state / 'traffic-meter.json'))
                                           ['sequence'] >= meter['sequence'] + 2 else None),
                                   30, 'meter continues sampling with FIFO policy acknowledgement')
            require(policy_cache.is_fifo(), 'the FIFO fault was removed before sampling exercised it')
            require(next_meter['total_rx_bytes'] > meter['total_rx_bytes'], 'meter lost actual FIFO-window traffic')
            write_json(self.output / 'policy-fifo-progress.json', {
                'sequence_before': meter['sequence'], 'sequence_after': next_meter['sequence'],
                'rx_before': meter['total_rx_bytes'], 'rx_after': next_meter['total_rx_bytes']})
        finally:
            self.offline = False
        self.policy('application', [self.absolute(800_000)], reset_anchor=False)
        self.status('limited', 800_000)
        self.record('policy-fifo-does-not-stall-meter')
        (self.state / 'traffic-limit-desired.json').write_text('{truncated')
        self.policy('application', [], enabled=False, reset_anchor=False)
        self.status('disabled')
        require(self.transfer('D', 4 * 1024**2, True) > 150_000_000,
                'a corrupt local cache prevented an authoritative disable')
        self.record('corrupt-policy-cache-recovers-authoritative-disable')
        self.policy('application', [self.absolute(800_000)], reset_anchor=False)
        self.status('limited', 800_000)
        control = self.state / 'traffic-limit-xray.json'
        expected = read_json(control)
        recovered = {}
        target = self.state / 'control-symlink-target'
        for fault in ['truncated', 'missing', 'oversized', 'fifo', 'symlink']:
            if fault == 'truncated':
                control.write_text('{truncated')
            elif fault == 'oversized':
                control.write_bytes(b' ' * 4097)
            else:
                control.unlink()
                if fault == 'fifo':
                    os.mkfifo(control, 0o600)
                elif fault == 'symlink':
                    target.write_text('untouched')
                    control.symlink_to(target)
            started = time.monotonic()
            if fault in ['fifo', 'symlink']:
                time.sleep(.4)  # Expire Xray's refresh cache and exercise its actual file reader.
                try:
                    self.udp(128, .5)
                except (TimeoutError, ConnectionError):
                    pass  # Temporary fail-closed behavior is expected while the file is invalid.
            self.wait(lambda: control.is_file() and not control.is_symlink()
                      and read_json(control) == expected, 30, 'control self-repair: ' + fault)
            recovered[fault] = time.monotonic() - started
            require(self.udp(128, 5) < 5, 'application control repair did not restore forwarding')
        require(target.read_text() == 'untouched', 'control repair followed a symlink target')
        target.unlink()
        self.record('application-control-self-repair', recovery_seconds=recovered)
        self.agent.kill()
        self.agent.wait(5)
        control.unlink()
        meter = self.state / 'traffic-meter.json'
        backup = self.state / 'traffic-meter.fault-backup'
        meter.rename(backup)
        meter.mkdir()
        try:
            self.start_agent(require_meter=False)
            try:
                self.udp(128, 1)
                raise RuntimeError('missing control on restart bypassed an unknown historical application cap')
            except (TimeoutError, ConnectionError):
                pass
            require(not control.exists(), 'unknown startup published a false uncapped control')
        finally:
            meter.rmdir()
            backup.rename(meter)
        self.wait(lambda: control.is_file() and (read_json(control) or {}).get('rate_bps') == 800_000,
                  35, 'missing startup control recovery after valid meter returns')
        require(self.udp(128, 5) < 5, 'valid meter did not release unknown-startup protection')
        self.record('missing-control-startup-preserves-cap-until-meter-recovers')
        self.agent.kill()
        self.agent.wait(5)
        control.write_text('{truncated')
        started = time.time()
        self.start_agent()
        self.wait(lambda: (state if (state := read_json(control)) and state['enabled']
                           and state['rate_bps'] == 800_000 else None),
                  35, 'cold corrupt control recovery from evaluated policy')
        self.wait(lambda: (self.state / 'traffic-limit-status.json').stat().st_mtime > started,
                  30, 'fresh local round after restart')
        self.status('limited', 800_000)
        require(self.udp(128, 5) < 5, 'cold corrupt control recovery did not restore data')
        self.record('corrupt-application-control-restart-recovers')
        self.policy('both', [self.absolute(800_000)], reset_anchor=False)
        self.status('limited', 800_000)
        network = self.state / 'traffic-limit-network.json'
        owned = read_json(network)
        network.write_text('{truncated')
        self.policy('both', [self.absolute(512_000)], reset_anchor=False)
        report = self.status('error')
        require(report['system']['phase'] == 'error' and report['application']['phase'] == 'limited'
                and report['application']['rate_bps'] == 512_000,
                'corrupt system ownership prevented an independent application update')
        self.throughput('corrupt-network-application-still-updates', 512_000, 128 * 1024, True)
        self.policy('both', [], enabled=False, reset_anchor=False)
        report = self.status('error')
        require(report['application']['phase'] == 'disabled',
                'corrupt system ownership prevented application disable')
        write_json(network, owned)
        self.status('disabled')
        self.record('corrupt-network-application-disable-and-recovery')
        self.policy('both', [self.absolute(800_000)], reset_anchor=False)
        self.status('limited', 800_000)
        command('tc', 'filter', 'add', 'dev', 'limit0', 'egress', 'pref', '1',
                'matchall', 'action', 'pass')
        drift_started = time.time()
        command('tc', 'qdisc', 'del', 'dev', 'limit0', 'root')
        command('tc', 'qdisc', 'add', 'dev', 'limit0', 'root', 'handle', 'bcd0:',
                'htb', 'default', '20')
        def correct_default():
            queues = json.loads(command('tc', '-j', 'qdisc', 'show', 'dev', 'limit0'))
            return any(q.get('handle') == 'bcd0:' and q.get('options', {}).get('default')
                       in [16, '0x10'] for q in queues)
        self.wait(correct_default, 30, 'owned HTB default class drift repair')
        self.wait(lambda: (self.state / 'traffic-limit-status.json').stat().st_mtime > drift_started,
                  30, 'fresh enforcement status after queue repair')
        self.status('limited', 800_000)
        self.throughput('owned-qdisc-default-drift-self-heals', 800_000, 128 * 1024)
        self.policy('both', [], enabled=False, reset_anchor=False)
        self.status('disabled')
        filters = json.loads(command('tc', '-j', 'filter', 'show', 'dev', 'limit0', 'egress'))
        require(any(f.get('pref') == 1 for f in filters), 'foreign egress was deleted')
        self.record('foreign-egress-survives-reconcile-and-disable')
        command('tc', 'filter', 'del', 'dev', 'limit0', 'egress', 'pref', '1')
        command('tc', 'qdisc', 'replace', 'dev', 'limit0', 'root', 'handle', '1234:', 'tbf',
                'rate', '10mbit', 'burst', '32k', 'latency', '50ms')
        self.policy('both', [self.absolute(800_000)], reset_anchor=False)
        report = self.status('error')
        require(report['system']['phase'] == 'error' and report['application']['phase'] == 'limited',
                'custom root was not isolated to the system layer')
        self.policy('both', [], enabled=False, reset_anchor=False)
        self.status('disabled')
        require('1234:' in command('tc', 'qdisc', 'show', 'dev', 'limit0'), 'custom QoS overwritten')
        self.record('foreign-root-preserved-application-still-protects')
        command('tc', 'qdisc', 'del', 'dev', 'limit0', 'root')
        command('tc', 'filter', 'add', 'dev', 'limit0', 'ingress', 'pref', '1',
                'matchall', 'action', 'pass')
        self.policy('both', [self.absolute(800_000)], reset_anchor=False)
        report = self.status('error')
        require(report['system']['phase'] == 'error' and report['application']['phase'] == 'limited',
                'foreign ingress was not isolated to the system layer')
        require(any(f.get('pref') == 1 for f in json.loads(command(
            'tc', '-j', 'filter', 'show', 'dev', 'limit0', 'ingress'))), 'foreign ingress overwritten')
        self.record('foreign-ingress-preserved-application-still-protects')
        self.policy('both', [], enabled=False, reset_anchor=False)
        self.status('disabled')
        command('tc', 'filter', 'del', 'dev', 'limit0', 'ingress', 'pref', '1')
        tc = Path(shutil.which('tc'))
        hidden_tc = tc.with_name(tc.name + '.fault-original')
        tc.rename(hidden_tc)
        try:
            tc.write_text('#!/bin/sh\nsleep 30\n')
            tc.chmod(0o755)
            self.policy('both', [self.absolute(800_000)], reset_anchor=False)
            report = self.status('error')
            require(report['system']['phase'] == 'error' and report['application']['phase'] == 'limited'
                    and 'timed out' in report['system']['error'],
                    'a hung tc command did not time out independently of application enforcement')
            self.record('tc-timeout-does-not-block-application', system_error=report['system']['error'])
        finally:
            tc.unlink()
            hidden_tc.rename(tc)
        self.status('limited', 800_000)
        self.policy('application', [self.absolute(800_000)], reset_anchor=False)
        self.status('limited', 800_000)

    def run(self):
        reboot = read_json(self.output / 'reboot.json')
        if reboot:
            if os.environ['BROCADE_TRAFFIC_LIMIT_RUNTIME'] == 'kvm':
                require(self.boot != reboot['boot_id'], 'KVM guest did not really reboot')
            self.offline = True
            self.status('limited', 800_000)
            self.throughput('offline-reboot-restores-both-layers', 800_000, 128 * 1024, True)
            meter = read_json(self.state / 'traffic-meter.json')
            require(meter['meter_id'] == reboot['meter_id'], 'logical meter identity lost on reboot')
            require(meter['total_rx_bytes'] >= reboot['total_rx_bytes'], 'logical meter regressed')
            self.offline = False
            self.policy('both', [], enabled=False, reset_anchor=False)
            self.status('disabled')
            self.record('reboot-verified', old_boot_id=reboot['boot_id'], new_boot_id=self.boot)
            write_json(self.output / 'result.json', {'passed': True, 'cases': len(self.records)})
            return
        baseline = self.transfer('D', 4 * 1024**2)
        require(baseline > 150_000_000, 'baseline too slow to certify a 100 Mbps cap')
        self.record('uncapped-baseline', measured_bps=baseline)
        rules = [{'threshold': {'kind': 'quota_percent', 'basis_points': 9000}, 'rate_bps': 100_000_000},
                 self.absolute(512_000, 512 * 1024**2)]
        self.policy('system', rules)
        self.status('ready')
        self.transfer('D', 6 * 1024**2)
        self.status('limited', 100_000_000)
        self.throughput('quota-percent-system-rx-tx', 100_000_000, 16 * 1024**2)
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
            start = time.monotonic()
            list(pool.map(lambda _: self.transfer('D', 8 * 1024**2), range(3)))
            aggregate = 24 * 1024**2 * 8 / (time.monotonic() - start)
        require(45_000_000 < aggregate < 135_000_000, 'connections did not share the NIC cap')
        self.record('system-connections-share-cap', aggregate_bps=aggregate)
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            start = time.monotonic()
            values = list(pool.map(lambda direction: self.transfer(direction, 16 * 1024**2), ['D', 'U']))
            combined = 32 * 1024**2 * 8 / (time.monotonic() - start)
        require(all(75_000_000 < rate < 135_000_000 for rate in values)
                and 150_000_000 < combined < 270_000_000,
                'RX and TX incorrectly shared one bidirectional cap')
        self.record('system-duplex-independent-caps', measured_bps=values, combined_bps=combined)
        sequence = read_json(self.state / 'traffic-meter.json')['sequence']
        meter = self.wait(lambda: (value if (value := read_json(self.state / 'traffic-meter.json'))
                                  ['sequence'] > sequence else None), 20, 'post-transfer meter sample')
        anchor = self.desired['anchor']
        current = max(meter['total_rx_bytes'] - anchor['meter_rx_bytes'],
                      meter['total_tx_bytes'] - anchor['meter_tx_bytes'])
        rules[1] = self.absolute(512_000, current + 4 * 1024**2)
        self.policy('system', rules, reset_anchor=False)
        self.status('limited', 100_000_000)
        self.offline = True
        self.transfer('D', 6 * 1024**2)
        self.status('limited', 512_000)
        self.throughput('offline-absolute-strictest-rule', 512_000, 512 * 1024)
        self.offline = False
        self.policy('system', [], enabled=False)
        self.status('disabled')
        require('htb' not in command('tc', 'qdisc', 'show', 'dev', 'limit0'), 'disable retained HTB')
        self.record('disable-cleans-owned-kernel-resources')
        self.policy('application', [self.absolute(800_000)])
        self.transfer('D', 1024)
        self.status('limited', 800_000)
        self.throughput('application-tcp-payload', 800_000, 128 * 1024, True)
        require(self.transfer('D', 4 * 1024**2) > 150_000_000,
                'application limit unexpectedly throttled non-Xray traffic')
        self.record('application-scope-excludes-other-programs')
        if os.environ.get('BROCADE_TRAFFIC_LIMIT_FAULT_INJECTION') == '1':
            self.fault_injection()
        self.policy('application', [self.absolute(1000)], reset_anchor=False)
        self.status('limited', 1000)
        elapsed = self.udp(1200, 35)
        require(elapsed >= 18, '1 Kbps UDP did not share the aggregate payload budget')
        self.record('one-kbps-udp-long-wait', payload_bytes=1200, elapsed_seconds=elapsed)
        self.policy('application', [], enabled=False)
        self.status('disabled')
        tc = Path(shutil.which('tc'))
        hidden_tc = tc.with_name(tc.name + '.acceptance-hidden')
        tc.rename(hidden_tc)
        try:
            self.policy('both', [self.absolute(800_000)])
            self.transfer('D', 1024)
            report = self.status('error')
            require(report['system']['phase'] == 'error' and report['application']['phase'] == 'limited',
                    'a failed system layer suppressed the application layer')
            config = read_json(self.state / 'xray.json')
            config['inbounds'].append({'tag': 'added', 'listen': '127.0.0.1', 'port': 45005,
                                      'protocol': 'dokodemo-door', 'settings': {
                                          'address': '198.18.0.2', 'port': 45001, 'network': 'tcp'}})
            content = json.dumps(config)
            self.work = {'t': 'deployment', 'v': {'deployment': {
                'deployment_id': 9001, 'node_id': 'traffic-limit-lab', 'claim_generation': 1, 'wave': 0,
                'actions': ['apply-xray'], 'xray_config_update_policy': 'handoff', 'usage_generation_id': None,
                'desired': {**{key: {'state': 'unmanaged', 'reason': 'test'}
                              for key in ['phantun', 'wireguard', 'hy2_port_hop']},
                            'xray': {'state': 'present', 'content': content,
                                     'sha256': hashlib.sha256(content.encode()).hexdigest()},
                            'grants': {'state': 'present', 'inbounds': []}},
                'phantun_binary': None}, 'certificates': []}}
            self.wait(lambda: self.observations, 60, 'configuration receipt during tc failure')
            require(self.observations[-1].get('result') == 'applied',
                    f'configuration failed while tc was unavailable: {self.observations[-1]}')
            require(self.connectable('127.0.0.1', 45005), 'configuration did not add real Xray listener')
            with socket.create_connection(('127.0.0.1', 45005), 3) as connection:
                connection.settimeout(5)
                connection.sendall(struct.pack('!cQ', b'E', 32) + bytes(range(32)))
                require(receive(connection, 32) == bytes(range(32)), 'new listener did not forward payload')
            require(self.agent.poll() is None and self.runtime, 'Agent reporting stopped with missing tc')
            self.throughput('tc-failure-application-still-protects', 800_000, 128 * 1024, True)
            self.record('tc-failure-does-not-block-configuration', observation=self.observations[-1],
                        system_error=report['system']['error'])
        finally:
            hidden_tc.rename(tc)
        self.status('limited', 800_000)
        self.throughput('both-layers', 800_000, 128 * 1024, True)
        self.agent.kill()
        self.agent.wait(5)
        require(self.connectable('127.0.0.1', 45003), 'Xray unexpectedly stopped with the Agent')
        try:
            self.udp(32, 12)
            raise RuntimeError('active application cap silently failed open without its budget service')
        except (TimeoutError, ConnectionError):
            pass
        self.start_agent()
        self.status('limited', 800_000)
        require(self.udp(128, 5) < 5, 'budget did not recover after Agent restart')
        self.record('active-budget-outage-and-agent-restart')
        self.policy('both', [], enabled=False)
        self.status('disabled')
        # Establish that the actual Xray has observed the disabled state before corrupting its
        # control file. An Xray that last saw an active cap must remain fail-closed instead.
        stats = json.loads(command(self.xray_binary, 'api', 'statsquery',
                                   '--server=127.0.0.1:45000',
                                   '-pattern=brocade>>>machine_limit>>>rate_bps'))
        require(any(row['name'] == 'brocade>>>machine_limit>>>rate_bps'
                    and int(row.get('value', 0)) == 0 for row in stats.get('stat', [])),
                'Xray did not confirm the uncapped state before optional-budget failure')
        time.sleep(.3)
        self.transfer('D', 1024, True)
        self.agent.kill()
        self.agent.wait(5)
        control = self.state / 'traffic-limit-xray.json'
        control.unlink()
        control.mkdir()
        runtime_count = len(self.runtime)
        self.start_agent()
        self.wait(lambda: self.agent.poll() is None and len(self.runtime) > runtime_count, 40,
                  'Agent remains alive despite budget initialization error')
        # A corrupt checkpoint is unknown state for a newly restored Xray. Availability of its
        # payload path is deliberately not promised until the checkpoint is repaired.
        self.policy('both', [self.absolute(800_000)])
        self.transfer('D', 1024)
        self.status('error')
        control.rmdir()
        self.status('limited', 800_000)
        require(self.udp(128, 5) < 5, 'budget initialization did not recover independently')
        self.record('budget-initialization-failure-retries-without-blocking-agent')
        meter = read_json(self.state / 'traffic-meter.json')
        write_json(self.output / 'reboot.json', {
            'boot_id': self.boot, 'meter_id': meter['meter_id'], 'total_rx_bytes': meter['total_rx_bytes']})
        self.record('ready-for-real-reboot')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--peer', action='store_true')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    require(os.environ.get('BROCADE_TRAFFIC_LIMIT_DISPOSABLE') == '1' and os.geteuid() == 0,
            'this suite requires an explicitly disposable root guest')
    if args.peer:
        peer()
        return
    lab = None
    try:
        lab = Lab(args.output)
        lab.run()
    except Exception:
        args.output.mkdir(parents=True, exist_ok=True)
        (args.output / 'failure.txt').write_text(traceback.format_exc())
        raise
    finally:
        if lab:
            for process in reversed(lab.processes):
                if process.poll() is None:
                    process.kill()
                    process.wait(5)


if __name__ == '__main__':
    main()
