#!/usr/bin/env python3
"""Real Console/Agent compression and fleet load, only in a disposable container or KVM guest."""
import argparse
import asyncio
import base64
from collections import Counter, deque
import hashlib
import http.client
import json
import os
from pathlib import Path
import resource
import secrets
import shutil
import signal
import socket
import struct
import subprocess
import threading
import time
import traceback
import zlib

EVENTS = []
PROCESSES = []
COOKIE = ''
RESULTS = None
BOOTSTRAP = secrets.token_urlsafe(32)
ROOT = Path('/var/lib/brocade-realtime-acceptance')
OWNER = None


def save(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    if OWNER is not None:
        os.chown(temporary, *OWNER)
    temporary.replace(path)


def event(name, **details):
    EVENTS.append({'name': name, **details})
    save(RESULTS / 'evidence.json', EVENTS)
    print(json.dumps(EVENTS[-1]), flush=True)


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sql(query, data=None):
    identity = secrets.token_hex(16)
    save(RESULTS / 'sql-request.json', {'id': identity, 'query': query, 'data': data})
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        path = RESULTS / 'sql-response.json'
        if path.exists():
            reply = json.loads(path.read_text())
            if reply['id'] == identity:
                require(reply['ok'], 'private sidecar SQL failed; see sql-error-private.txt')
                return reply['result'].strip()
        time.sleep(.05)
    raise RuntimeError('private SQL bridge timed out')


def api(method, path, body=None, bootstrap=False):
    connection = http.client.HTTPConnection('127.0.0.1', 18080, timeout=15)
    headers = {'Content-Type': 'application/json'}
    if COOKIE:
        headers['Cookie'] = COOKIE
    if bootstrap:
        headers['Authorization'] = 'Bearer ' + BOOTSTRAP
    try:
        connection.request(method, path, json.dumps(body) if body is not None else None, headers)
        reply = connection.getresponse()
        payload = reply.read(32 * 1024 * 1024 + 1)
        require(reply.status in (200, 201, 204), f'{method} {path}: HTTP {reply.status}')
        require(len(payload) <= 32 * 1024 * 1024, 'API response exceeds fixture budget')
        return json.loads(payload) if payload else None, dict(reply.getheaders())
    finally:
        connection.close()


def process(name, argv, environment=None):
    with (RESULTS / (name + '.log')).open('ab') as log:
        result = subprocess.Popen(argv, env=environment, stdout=log, stderr=subprocess.STDOUT)
    if OWNER is not None:
        os.chown(RESULTS / (name + '.log'), *OWNER)
    PROCESSES.append(result)
    return result


def rss(pid):
    try:
        for line in Path(f'/proc/{pid}/status').read_text().splitlines():
            if line.startswith('VmRSS:'):
                return int(line.split()[1])
    except FileNotFoundError:
        return 0


class Meter:
    def __init__(self):
        self.frames = self.compressed = self.wire_bytes = 0
        self.buffer = bytearray()
        self.decoder = zlib.decompressobj(-15)
        self.rpc_counts = Counter()
        self.rpc_events = deque(maxlen=256)

    def feed(self, data):
        self.buffer.extend(data)
        while len(self.buffer) >= 2:
            first, second = self.buffer[:2]
            size, offset = second & 127, 2
            if size == 126:
                if len(self.buffer) < 4:
                    return
                size, offset = int.from_bytes(self.buffer[2:4], 'big'), 4
            elif size == 127:
                if len(self.buffer) < 10:
                    return
                size, offset = int.from_bytes(self.buffer[2:10], 'big'), 10
            require(size <= 512 * 1024 + 1024, 'proxy frame exceeds fixture memory budget')
            mask_start = offset
            offset += 4 if second & 128 else 0
            if len(self.buffer) < offset + size:
                return
            if first & 15 in (0, 1, 2):
                self.frames += 1
                self.compressed += bool(first & 64)
                self.wire_bytes += offset + size
            if first & 15 == 1:
                payload = bytes(self.buffer[offset:offset + size])
                if second & 128:
                    mask = self.buffer[mask_start:mask_start + 4]
                    payload = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
                if first & 64:
                    payload = self.decoder.decompress(payload + b'\x00\x00\xff\xff', 512 * 1024 + 1)
                require(len(payload) <= 512 * 1024, 'proxy inflation budget exceeded')
                message = json.loads(payload)
                kind = message.get('type')
                if kind == 'rpc':
                    path = message['path'].split('?', 1)[0]
                    self.rpc_counts[path] += 1
                    self.rpc_counts[message['method'] + ' ' + path] += 1
                    self.rpc_events.append({'at':time.monotonic(), 'path':path})
                elif kind in ('ready', 'wake', 'rpc_result'):
                    self.rpc_counts[kind] += 1
            del self.buffer[:offset + size]


class AgentProxy:
    def __init__(self, identities):
        self.identities = identities
        self.connections = []
        self.tasks = set()

    async def accept(self, reader, writer):
        remote = None
        task = asyncio.current_task()
        self.tasks.add(task)
        try:
            request = await asyncio.wait_for(reader.readuntil(b'\r\n\r\n'), 10)
            target_reader, remote = await asyncio.open_connection('127.0.0.1', 18081)
            remote.write(request)
            await remote.drain()
            observation = None
            if request.split(b'\r\n', 1)[0].startswith(b'GET /agent/v1/realtime '):
                headers = {name.lower(): value.strip() for name, value in
                           (line.split(b':', 1) for line in request.split(b'\r\n')[1:] if b':' in line)}
                token = headers.get(b'authorization', b'').removeprefix(b'Bearer ').decode()
                response = await asyncio.wait_for(target_reader.readuntil(b'\r\n\r\n'), 10)
                writer.write(response)
                await writer.drain()
                observation = {'node': self.identities.get(token), 'up': Meter(), 'down': Meter(),
                               'writers': (writer, remote), 'compression': b'sec-websocket-extensions: permessage-deflate' in response.lower()}
                require(response.startswith(b'HTTP/1.1 101'), 'real Agent upgrade failed')
                require(observation['compression'], 'real Agent failed to negotiate compression')
                self.connections.append(observation)

            async def pump(source, destination, meter):
                try:
                    while data := await source.read(65536):
                        if meter is not None:
                            meter.feed(data)
                        destination.write(data)
                        await destination.drain()
                finally:
                    destination.close()
            await asyncio.gather(pump(reader, remote, observation['up'] if observation else None),
                                 pump(target_reader, writer, observation['down'] if observation else None))
        except (ConnectionError, OSError, asyncio.IncompleteReadError):
            pass
        finally:
            writer.close()
            if remote:
                remote.close()
            self.tasks.discard(task)

    def disconnect(self):
        for connection in self.connections:
            for writer in connection['writers']:
                writer.close()


class Peer:
    def __init__(self, node, token):
        self.node, self.token = node, token
        self.sequence = self.messages = self.raw_bytes = self.wire_bytes = 0
        self.command = None
        self.errors = []
        self.lock = asyncio.Lock()
        self.last_rows = 0

    async def connect(self):
        self.reader, self.writer = await asyncio.open_connection('127.0.0.1', 18081)
        key = base64.b64encode(secrets.token_bytes(16)).decode()
        request = f'GET /agent/v1/realtime HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Extensions: permessage-deflate\r\nAuthorization: Bearer {self.token}\r\n\r\n'
        self.writer.write(request.encode())
        await self.writer.drain()
        response = await asyncio.wait_for(self.reader.readuntil(b'\r\n\r\n'), 15)
        require(response.startswith(b'HTTP/1.1 101'), f'{self.node}: upgrade rejected')
        require(b'sec-websocket-extensions: permessage-deflate' in response.lower(), f'{self.node}: compression absent')
        accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
        require(accept in response, 'WebSocket accept mismatch')
        self.encoder = zlib.compressobj(level=6, wbits=-15)
        self.decoder = zlib.decompressobj(wbits=-15)
        self.receive_task = asyncio.create_task(self.receive())
        deadline = time.monotonic() + 15
        while self.command != 'start' and time.monotonic() < deadline:
            require(not self.errors, f'{self.node}: reader failed')
            await asyncio.sleep(.02)
        require(self.command == 'start', f'{self.node}: demand did not reach the peer')

    async def send(self, opcode, payload, compressed=False):
        async with self.lock:
            if compressed:
                self.raw_bytes += len(payload)
                payload = self.encoder.compress(payload) + self.encoder.flush(zlib.Z_SYNC_FLUSH)
                payload = payload[:-4]
            flags = 0x80 | opcode | (0x40 if compressed else 0)
            length = len(payload)
            header = bytes([flags, 0x80 | length]) if length < 126 else bytes([flags, 0xfe]) + struct.pack('!H', length) if length <= 65535 else bytes([flags, 0xff]) + struct.pack('!Q', length)
            mask = secrets.token_bytes(4)
            masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
            self.writer.write(header + mask + masked)
            await self.writer.drain()
            if compressed:
                self.wire_bytes += len(header) + 4 + len(payload)
                self.messages += 1

    async def receive(self):
        try:
            while True:
                head = await self.reader.readexactly(2)
                opcode, size = head[0] & 15, head[1] & 127
                if size == 126:
                    size = struct.unpack('!H', await self.reader.readexactly(2))[0]
                elif size == 127:
                    size = struct.unpack('!Q', await self.reader.readexactly(8))[0]
                require(size <= 512 * 1024, 'server frame exceeds fixture budget')
                payload = await self.reader.readexactly(size)
                if opcode == 8:
                    return
                if opcode == 9:
                    await self.send(10, payload)
                elif opcode == 1:
                    if head[0] & 0x40:
                        payload = self.decoder.decompress(payload + b'\x00\x00\xff\xff', 512 * 1024 + 1)
                    require(len(payload) <= 512 * 1024, 'server output exceeds inflation budget')
                    self.command = json.loads(payload)['type']
        except (OSError, asyncio.IncompleteReadError, asyncio.CancelledError):
            return
        except Exception as error:
            self.errors.append(type(error).__name__)

    async def close(self):
        self.writer.close()
        self.receive_task.cancel()
        await asyncio.gather(self.receive_task, return_exceptions=True)

    async def report(self, rows=64):
        require(not self.errors, f'{self.node}: peer errors')
        self.sequence += 1
        self.last_rows = rows
        value = {'sequence': self.sequence, 'sampled_at_unix_millis': int(time.time() * 1000),
                 'elapsed_millis': 1000, 'interface': 'eth0', 'rx_bytes_per_sec': self.sequence * 1000,
                 'tx_bytes_per_sec': 678, 'has_gap': False, 'diagnostics_unchanged': False,
                 'user_traffic': {'state': 'ready', 'all_users': True, 'elapsed_millis': 1000,
                    'users': [{'tenant_id': 'lab', 'user_id': f'user-{index:04}', 'ingress_id': 'lab-entry',
                               'uplink_bytes_per_sec': self.sequence + index, 'downlink_bytes_per_sec': 999999 + index}
                              for index in range(rows)], 'truncated': False, 'has_gap': False}}
        payload = json.dumps(value, separators=(',', ':')).encode()
        require(len(payload) <= 512 * 1024, 'fixture generated an excessive sample')
        await self.send(1, payload, compressed=True)
        return len(payload)


class Observer:
    def __init__(self):
        self.stop = threading.Event()
        self.messages = self.reconnects = self.refreshes = self.lagged = 0
        self.error = None

    def run(self):
        while not self.stop.is_set():
            connection = http.client.HTTPConnection('127.0.0.1', 18080, timeout=20)
            try:
                connection.request('GET', '/realtime/nodes/events', headers={'Cookie': COOKIE})
                response = connection.getresponse()
                require(response.status == 200, 'fleet SSE rejected')
                self.reconnects += 1
                event_name = b'message'
                while not self.stop.is_set():
                    line = response.readline()
                    if not line:
                        break
                    if line.startswith(b'event:'):
                        event_name = line[6:].strip()
                    elif line.startswith(b'data:'):
                        if event_name == b'reset':
                            reason = line[5:].strip()
                            if reason == b'authorization_refresh':
                                self.refreshes += 1
                            elif reason == b'lagged':
                                self.lagged += 1
                            else:
                                raise RuntimeError('unexpected SSE reset reason')
                            break
                        json.loads(line[5:])
                        self.messages += 1
            except (OSError, http.client.HTTPException):
                if not self.stop.is_set():
                    time.sleep(.1)
            except Exception as error:
                self.error = type(error).__name__
                return
            finally:
                connection.close()


async def verify_real_rpc_wakes(proxy, node):
    paths = ['/agent/v1/agent-release', '/agent/v1/xray-release',
             '/agent/v1/source-access', '/agent/v1/traffic-limit',
             '/agent/v1/user-traffic/policies', '/agent/v1/unlock-probe/target',
             '/agent/v1/forwarding/tcp-ping-target']
    def count(path):
        return sum(c['up'].rpc_counts['GET ' + path] for c in proxy.connections if c['node'] == node)
    deadline = time.monotonic() + 20
    while not all(count(path) for path in paths):
        if time.monotonic() >= deadline:
            save(RESULTS / 'rpc-missing.json', {path:count(path) for path in paths})
            raise RuntimeError('real Agent did not migrate priority RPCs; inspect rpc-missing.json')
        await asyncio.sleep(.05)
    await asyncio.sleep(1)
    receipts = []
    for component, path in [('agent', paths[0]), ('xray', paths[1])]:
        before = count(path)
        started = time.monotonic()
        # A halted release cannot install anything. Its persisted target insertion still
        # exercises the exact commit trigger used by an approved running release.
        await asyncio.to_thread(sql, '''WITH release AS (
            INSERT INTO binary_releases(component,idempotency_key,build_id,version,artifacts,status,created_by,xray_strategy)
            SELECT item->>'component',item->>'key',repeat('0',64),'channel-wake-test',
                   jsonb_build_object('x86_64',repeat('0',64)),'halted',id,
                   CASE WHEN item->>'component'='xray' THEN 'restart' END
            FROM admin_operators CROSS JOIN jsonb_array_elements((SELECT data FROM lab_seed)) item
            WHERE role='system-admin' LIMIT 1 RETURNING id)
            INSERT INTO binary_release_targets(release_id,node_id,before_sha256)
            SELECT id,:'node',repeat('0',64) FROM release;'''.replace(":'node'", "'" + node + "'"),
            [{'component':component,'key':secrets.token_hex(16)}])
        while count(path) == before:
            require(time.monotonic() - started < 4, component + ' update worker was not woken immediately')
            await asyncio.sleep(.01)
        receipts.append({'domain':component, 'seconds':round(time.monotonic()-started, 3)})
    before = count(paths[2])
    started = time.monotonic()
    await asyncio.to_thread(sql, 'UPDATE source_access_state SET revision=revision+1')
    while count(paths[2]) == before:
        require(time.monotonic()-started < 4, 'source policy change did not wake the real Agent')
        await asyncio.sleep(.01)
    receipts.append({'domain':'source_access', 'seconds':round(time.monotonic()-started, 3)})
    event('priority-rpc-wakes', real_node=node, migrated_paths=paths, receipts=receipts,
          binary_replacement=False, rpc_responses=sum(c['down'].rpc_counts['rpc_result'] for c in proxy.connections if c['node'] == node))


async def load(args, tokens, console):
    peers = [Peer(node, token) for node, token in tokens.items() if node.startswith('sim-')]
    real = {token: node for node, token in tokens.items() if node.startswith('real-')}
    proxy = AgentProxy(real)
    proxy_server = await asyncio.start_server(proxy.accept, '127.0.0.1', 18082)
    observer = Observer()
    thread = threading.Thread(target=observer.run, daemon=True)
    thread.start()
    try:
        semaphore = asyncio.Semaphore(32)
        async def connect(peer):
            async with semaphore:
                await peer.connect()
        await asyncio.gather(*(connect(peer) for peer in peers))
        real_processes = []
        for token, node in real.items():
            state = ROOT / node
            state.mkdir()
            token_file = state / 'token'
            token_file.write_text(token)
            token_file.chmod(0o600)
            environment = dict(os.environ)
            # These are foreground peers, not extra instances of the guest fixture's systemd unit.
            for key in ['INVOCATION_ID', 'JOURNAL_STREAM', 'NOTIFY_SOCKET', 'SYSTEMD_EXEC_PID']:
                environment.pop(key, None)
            real_processes.append(process(node, [str(ROOT / 'bin/agent'), 'run', '--server', 'http://127.0.0.1:18082',
                '--token-file', str(token_file), '--state-dir', str(state), '--apply', 'state-dir'], environment))
        deadline = time.monotonic() + 30
        while len({c['node'] for c in proxy.connections if c['node']}) < len(real):
            require(time.monotonic() < deadline, 'not all real Agents established compressed channels')
            await asyncio.sleep(.2)
        event('fleet-connected', simulated=len(peers), real_agents=len(real), compression_required=True)
        await verify_real_rpc_wakes(proxy, next(iter(real.values())))
        started = time.monotonic()
        next_reconnect = args.seconds / 2
        reconnected = False
        samples = []
        iteration = 0
        while time.monotonic() - started < args.seconds:
            tick = time.monotonic()
            await asyncio.gather(*(peer.report() for peer in peers))
            iteration += 1
            if not reconnected and time.monotonic() - started >= next_reconnect:
                selected = peers[:min(32, len(peers))]
                await asyncio.gather(*(peer.close() for peer in selected))
                await asyncio.gather(*(connect(peer) for peer in selected))
                proxy.disconnect()
                reconnected = True
                event('forced-reconnect', simulated=len(selected), real_agents=len(real))
            if iteration % 5 == 0:
                sample = {'seconds': round(time.monotonic() - started, 2), 'console_rss_kib': rss(console.pid),
                          'fixture_rss_kib': rss(os.getpid()), 'fixture_open_fds': len(list(Path('/proc/self/fd').iterdir())), 'sent': sum(p.messages for p in peers),
                          'sse_messages': observer.messages, 'round_seconds': round(time.monotonic()-tick, 3)}
                samples.append(sample)
                save(RESULTS / 'load-progress.json', samples)
                print(json.dumps(sample), flush=True)
            require(console.poll() is None, 'Console exited during fleet load')
            require(all(p.poll() is None for p in real_processes), 'a real Agent exited during fleet load')
            require(observer.error is None, f'fleet SSE observer failed: {observer.error}')
            await asyncio.sleep(max(0, 2 - (time.monotonic() - tick)))
        # Near-limit samples carry the largest supported number of user-rate rows.
        large_peers = peers[:min(16, len(peers))]
        sizes = await asyncio.gather(*(peer.report(2048) for peer in large_peers))
        await asyncio.sleep(2)
        for peer in large_peers:
            snapshot, _ = await asyncio.to_thread(api, 'GET', f'/realtime/nodes/{peer.node}/snapshot')
            node = snapshot['nodes'][0]
            latest = node['samples'][-1]['sample']
            require(latest['sequence'] == peer.sequence, 'large sample sequence did not reach Console')
            require(latest['rx_bytes_per_sec'] == peer.sequence * 1000, 'sample content changed')
            require(len(latest['user_traffic']['users']) == 2048, 'large sample rows were lost')
            require(latest['user_traffic']['users'][-1]['user_id'] == 'user-2047', 'large sample tail changed')
        real_connections = [c for c in proxy.connections if c['node']]
        require(len(real_connections) >= 2 * len(real), 'real Agent reconnect was not observed')
        require(all(any(c['up'].compressed and c['down'].compressed for c in real_connections if c['node'] == node) for node in real.values()), 'real Agent frames were not compressed in both directions')
        require(observer.messages >= len(peers), 'fleet SSE did not deliver enough observations')
        raw = sum(p.raw_bytes for p in peers)
        wire = sum(p.wire_bytes for p in peers)
        require(wire < raw * .5, 'compression did not reduce this JSON workload sufficiently')
        event('large-sample-integrity', peers=len(large_peers), rows_per_sample=2048,
              raw_bytes_min=min(sizes), raw_bytes_max=max(sizes), checked_rows=2048*len(large_peers))
        event('compressed-load', duration_seconds=round(time.monotonic()-started, 2), simulated_nodes=len(peers),
              real_agents=len(real), messages=sum(p.messages for p in peers), raw_payload_bytes=raw,
              websocket_data_wire_bytes=wire, savings_pct=round((1-wire/raw)*100, 2),
              console_rss_peak_kib=max(s['console_rss_kib'] for s in samples),
              real_rpc_counts={node: dict(sum((c['up'].rpc_counts for c in real_connections if c['node'] == node), Counter())) for node in real.values()},
              console_rss_final_kib=rss(console.pid), sse_messages=observer.messages,
              sse_connections=observer.reconnects, sse_authorization_refreshes=observer.refreshes, sse_lagged_resets=observer.lagged, real_compressed_frames=sum(c['up'].compressed for c in real_connections),
              real_node_compression={node: {'connections':sum(c['node'] == node for c in real_connections),
                    'up_compressed':sum(c['up'].compressed for c in real_connections if c['node'] == node),
                    'down_compressed':sum(c['down'].compressed for c in real_connections if c['node'] == node)}
                    for node in real.values()})
    finally:
        observer.stop.set()
        await asyncio.gather(*(peer.close() for peer in peers if hasattr(peer, 'writer')), return_exceptions=True)
        proxy.disconnect()
        proxy_server.close()
        pending = list(proxy.tasks)
        for task in pending:
            task.cancel()
        await asyncio.gather(*pending, return_exceptions=True)
        await asyncio.wait_for(proxy_server.wait_closed(), 5)


def main():
    global RESULTS, COOKIE, OWNER
    parser = argparse.ArgumentParser()
    parser.add_argument('--runtime', choices=['docker', 'kvm'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--nodes', type=int, default=1000)
    parser.add_argument('--real-agents', type=int, default=32)
    parser.add_argument('--seconds', type=int, default=90)
    args = parser.parse_args()
    os.umask(0o077)
    RESULTS = args.output
    RESULTS.mkdir(parents=True, exist_ok=True)
    try:
        require(os.geteuid() == 0 and os.environ.get('BROCADE_REALTIME_DISPOSABLE') == '1', 'disposable guest marker missing')
        require(Path('/.dockerenv').exists() if args.runtime == 'docker' else subprocess.check_output(['systemd-detect-virt'], text=True).strip() == 'kvm', 'unexpected guest environment')
        require(1 <= args.nodes <= 5000 and 1 <= args.real_agents <= 64 and 10 <= args.seconds <= 600, 'fixture limits exceeded')
        require(not ROOT.exists(), 'refusing to reuse guest state')
        ROOT.mkdir(mode=0o700)
        soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        resource.setrlimit(resource.RLIMIT_NOFILE, (min(max(soft, 16384), hard), hard))
        if args.runtime == 'docker':
            routes = subprocess.check_output(['ip', 'route', 'show', 'default'], text=True)
            if not routes.strip():
                subprocess.run(['ip', 'route', 'add', 'default', 'dev', 'eth0'], check=True)
        config = json.loads(Path('/inputs/config.json').read_text())
        OWNER = (config['uid'], config['gid'])
        binary_directory = ROOT / 'bin'
        binary_directory.mkdir()
        for name in ['console', 'agent']:
            shutil.copy2(Path('/inputs') / name, binary_directory / name)
            (binary_directory / name).chmod(0o755)
        environment = {**os.environ, 'DATABASE_URL': config[args.runtime]['database_url'],
                       'BROCADE_ADMIN_BIND': '127.0.0.1:18080', 'BROCADE_AGENT_BIND': '127.0.0.1:18081',
                       'BROCADE_BOOTSTRAP_TOKEN': BOOTSTRAP,
                       'BROCADE_SECRET_KEY': base64.b64encode(secrets.token_bytes(32)).decode(),
                       'BROCADE_CACHE_DIR': str(ROOT / 'cache')}
        console = process('console', [str(binary_directory / 'console')], environment)
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            require(console.poll() is None, 'Console failed to start; inspect console.log')
            try:
                api('GET', '/healthz')
                break
            except (OSError, RuntimeError, http.client.HTTPException):
                time.sleep(.2)
        else:
            raise RuntimeError('Console startup timed out')
        _, headers = api('POST', '/auth/init', {'operator_id': 'root', 'display_name': 'Realtime transport lab',
                          'password': secrets.token_urlsafe(32), 'root_tenant': 'lab'}, bootstrap=True)
        COOKIE = headers['set-cookie'].split(';')[0]
        distribution = http.client.HTTPConnection('127.0.0.1', 18081, timeout=15)
        try:
            distribution.request('GET', '/brocade-agent/x86_64', headers={'Cookie': COOKIE})
            response = distribution.getresponse()
            require(response.status == 200, f'embedded Agent distribution failed: HTTP {response.status}')
            embedded = response.read(32 * 1024 * 1024 + 1)
            require(len(embedded) <= 32 * 1024 * 1024, 'embedded Agent exceeds fixture budget')
            require(hashlib.sha256(embedded).hexdigest() == hashlib.file_digest(Path('/inputs/agent').open('rb'), 'sha256').hexdigest(), 'Agent input does not match the Console embedded bytes')
        finally:
            distribution.close()
        tokens, rows = {}, []
        for kind, count in [('sim', args.nodes), ('real', args.real_agents)]:
            for index in range(count):
                node = f'{kind}-{index:04}'
                token = 'broc_node_' + secrets.token_urlsafe(32)
                tokens[node] = token
                rows.append({'id': node, 'ip': f'{node}.invalid', 'overlay': f'10.{72 if kind == "sim" else 73}.{index//254}.{index%254+1}',
                             'private': base64.b64encode(secrets.token_bytes(32)).decode(),
                             'public': base64.b64encode(secrets.token_bytes(32)).decode(),
                             'hash': hashlib.sha256(token.encode()).hexdigest()})
        sql('''INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,
                  wg_listen_port,api_port,overlay,egress_allowed,dns_kind,dns_servers)
               SELECT item->>'id','lab',item->>'id',item->>'ip',(item->>'overlay')::inet,item->>'private',item->>'public',
                      51820,10085,false,false,'system','[]'::jsonb
                 FROM lab_seed, jsonb_array_elements(data) item;
               INSERT INTO node_lifecycle_state(node_id) SELECT item->>'id' FROM lab_seed,jsonb_array_elements(data) item ON CONFLICT DO NOTHING;
               INSERT INTO unlock_probe_discovery(node_id,next_discovery_at)
               SELECT item->>'id',now()+interval '24 hours' FROM lab_seed,jsonb_array_elements(data) item
               ON CONFLICT(node_id) DO UPDATE SET next_discovery_at=EXCLUDED.next_discovery_at;
               INSERT INTO node_agent_state(node_id,token_hash,token_prefix,token_created_at)
               SELECT item->>'id',item->>'hash','broc_node_lab',now() FROM lab_seed,jsonb_array_elements(data) item
               ON CONFLICT(node_id) DO UPDATE SET token_hash=EXCLUDED.token_hash,token_prefix=EXCLUDED.token_prefix,token_created_at=EXCLUDED.token_created_at;''', rows)
        event('environment', runtime=args.runtime, kernel=os.uname().release,
              console_sha256=hashlib.file_digest(Path('/inputs/console').open('rb'), 'sha256').hexdigest(),
              agent_sha256=hashlib.file_digest(Path('/inputs/agent').open('rb'), 'sha256').hexdigest(),
              nodes=args.nodes, real_agents=args.real_agents, compression='permessage-deflate defaults',
              nofile_limits=list(resource.getrlimit(resource.RLIMIT_NOFILE)))
        asyncio.run(load(args, tokens, console))
        save(RESULTS / 'result.json', {'passed': True, 'events': EVENTS})
    except Exception:
        (RESULTS / 'failure.txt').write_text(traceback.format_exc())
        if OWNER is not None:
            os.chown(RESULTS / 'failure.txt', *OWNER)
        save(RESULTS / 'result.json', {'passed': False, 'events': EVENTS})
        raise
    finally:
        for child in reversed(PROCESSES):
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
        if OWNER is not None:
            for path in RESULTS.rglob('*'):
                os.chown(path, *OWNER)


if __name__ == '__main__':
    main()
