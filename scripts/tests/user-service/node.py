#!/usr/bin/env python3
"""Container-only, real traffic acceptance for user billing cycles and service validity."""
import base64
from datetime import datetime, timedelta, timezone
import hashlib
import http.client
import json
import os
from pathlib import Path
import secrets
import signal
import shutil
import socket
import socketserver
import ssl
import subprocess
import threading
import time
import traceback

RUN_ID = Path('/lab/inputs/run-id').read_text().strip() if Path('/lab/inputs/run-id').exists() else ''
ROOT = Path('/var/lib/bsvc-' + RUN_ID[:12])
STATE = ROOT / 'agent-state'
RESULTS = Path('/lab/results')
BIN = ROOT / 'bin'
EVENTS = []
COOKIE = None
PASSWORD = secrets.token_urlsafe(32)
BOOTSTRAP = secrets.token_urlsafe(40)
NODE_TOKEN = 'broc_node_' + secrets.token_urlsafe(32)
PROCESSES = []


def require(value, message):
    if not value:
        raise RuntimeError(message)


def save(path, value):
    temporary = path.with_name(path.name + '.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def event(name, **values):
    EVENTS.append({'name': name, **values})
    save(RESULTS / 'evidence.json', EVENTS)


def command(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=45, **kwargs)


def sql(query, data=None):
    # The host runner executes this request only in the newly owned PostgreSQL sidecar.
    # No Docker socket or database-management capability is mounted into this container.
    request_id = secrets.token_hex(16)
    save(RESULTS / 'sql-request.json', {'id': request_id, 'query': query, 'data': data})
    deadline = time.monotonic() + 40
    while time.monotonic() < deadline:
        response = RESULTS / 'sql-response.json'
        if response.exists():
            value = json.loads(response.read_text())
            if value['id'] == request_id:
                require(value['ok'], 'laboratory SQL failed; inspect private SQL log')
                return value['result'].strip()
        time.sleep(.1)
    raise RuntimeError('laboratory PostgreSQL request timed out')


def api(method, path, body=None, expected=200, bootstrap=False, binary=False):
    client = http.client.HTTPConnection('127.0.0.1', 18080, timeout=8)
    headers = {'Content-Type': 'application/json'}
    if COOKIE:
        headers['Cookie'] = COOKIE
    if bootstrap:
        headers['Authorization'] = 'Bearer ' + BOOTSTRAP
    try:
        client.request(method, path, json.dumps(body) if body is not None else None, headers)
        response = client.getresponse()
        payload = response.read(128 * 1024 * 1024 + 1)
        require(len(payload) <= 128 * 1024 * 1024, 'response exceeded laboratory budget')
        if response.status != expected:
            (RESULTS / 'api-error-private.txt').write_bytes(payload)
            raise RuntimeError(f'{method} {path.split("?")[0]}: HTTP {response.status}, expected {expected}')
        return (payload if binary else json.loads(payload) if payload else None), dict(response.getheaders())
    finally:
        client.close()


def wait_for(label, condition, timeout=125):
    deadline, last = time.monotonic() + timeout, None
    while time.monotonic() < deadline:
        try:
            value = condition()
            if value:
                return value
        except (ConnectionError, OSError, RuntimeError) as error:
            last = type(error).__name__
        time.sleep(.5)
    raise RuntimeError(f'timeout waiting for {label}; last error class={last}')


def start_process(name, argv, env=None):
    log = (RESULTS / (name + '.log')).open('ab')
    process = subprocess.Popen(argv, env=env, stdout=log, stderr=subprocess.STDOUT)
    log.close()
    PROCESSES.append(process)
    return process


def stop_process(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(40)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(5)


class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(600)
        try:
            while data := self.request.recv(65536):
                self.request.sendall(data)
        except OSError:
            pass


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def serve(server):
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def exchange(connection, data):
    connection.sendall(data)
    received = bytearray()
    while len(received) < len(data):
        block = connection.recv(len(data) - len(received))
        require(block, 'unexpected EOF')
        received.extend(block)
    require(received == data, 'echo payload mismatch')


def can_echo(port):
    try:
        with socket.create_connection(('127.0.0.1', port), 1) as connection:
            connection.settimeout(1)
            exchange(connection, b'brocade-user-service-payload')
        return True
    except (OSError, RuntimeError):
        return False


class Flow:
    def __init__(self, port):
        self.samples, self.error = 0, None
        self.stop = threading.Event()
        self.connection = socket.create_connection(('127.0.0.1', port), 2)
        self.connection.settimeout(2)
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        try:
            while not self.stop.is_set():
                exchange(self.connection, self.samples.to_bytes(8, 'big') + b'x' * 56)
                self.samples += 1
                self.stop.wait(.1)
        except (OSError, RuntimeError) as error:
            self.error = type(error).__name__
        finally:
            self.connection.close()

    def close(self):
        self.stop.set()
        self.thread.join(3)


def identity():
    rows = []
    for path in Path('/proc').iterdir():
        if not path.name.isdigit():
            continue
        try:
            argv = (path / 'cmdline').read_bytes().split(b'\0')
            if str(STATE / 'xray.json').encode() in argv and b'run' in argv:
                rows.append({'pid': int(path.name), 'start_ticks': (path / 'stat').read_text().rsplit(') ', 1)[1].split()[19]})
        except (FileNotFoundError, ProcessLookupError):
            pass
    return rows


def policy(day, start=None, end=None):
    body = {'traffic_reset_day': day, 'service_starts_at': start, 'service_expires_at': end}
    preview, _ = api('POST', '/users/lab/alice/service-policy/preview', body)
    saved, _ = api('PUT', '/users/lab/alice/service-policy', body)
    require(saved['traffic_reset_day'] == day, 'policy was not saved')
    return preview


def grant(enabled):
    return api('POST', '/grants', {'tenant_id': 'lab', 'user_id': 'alice', 'app_id': 'app-lab',
                                  'ingress_id': 'ing-lab', 'enabled': enabled})[0]


def main():
    global COOKIE
    require(os.geteuid() == 0 and os.environ.get('BROCADE_USER_SERVICE_DISPOSABLE') == '1', 'container-only disposable marker missing')
    require(Path('/.dockerenv').exists(), 'this fixture requires an isolated Docker container')
    require(len(RUN_ID) == 32 and all(c in '0123456789abcdef' for c in RUN_ID), 'invalid disposable run identity')
    require(not ROOT.exists(), 'refusing to reuse an existing container state directory')
    os.umask(0o077)
    ROOT.mkdir(mode=0o700)
    STATE.mkdir()
    BIN.mkdir()
    # Internal Docker networks omit a default route. This route stays in this network
    # namespace and lets the Agent discover its interface without external connectivity.
    command(['ip', 'route', 'add', 'default', 'dev', 'eth0'])
    os.environ['PATH'] = str(BIN) + os.pathsep + os.environ['PATH']
    shutil.copy2('/lab/inputs/console', BIN / 'console')
    for name in ['geoip.dat', 'geosite.dat']:
        shutil.copy2(Path('/lab/inputs') / name, BIN / name)
    (BIN / 'console').chmod(0o755)
    event('environment', kernel=os.uname().release, os_release=Path('/etc/os-release').read_text(),
          boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(), runtime='docker')
    console_env = {**os.environ, 'DATABASE_URL': 'postgres://brocade@database:5432/brocade_service',
                   'BROCADE_ADMIN_BIND': '127.0.0.1:18080', 'BROCADE_BOOTSTRAP_TOKEN': BOOTSTRAP,
                   'BROCADE_SECRET_KEY': base64.b64encode(secrets.token_bytes(32)).decode(),
                   'BROCADE_CACHE_DIR': str(ROOT / 'cache')}
    console = start_process('console', [str(BIN / 'console')], console_env)
    wait_for('Console startup', lambda: api('GET', '/healthz')[0] is not None, 90)
    _, headers = api('POST', '/auth/init', {'operator_id': 'root', 'display_name': 'Disposable Docker lab',
                        'password': PASSWORD, 'root_tenant': 'lab'}, expected=201, bootstrap=True)
    COOKIE = headers['set-cookie'].split(';')[0]
    binaries = {}
    for name, route in [('agent', 'brocade-agent'), ('xray', 'brocade-xray')]:
        data, _ = api('GET', f'/{route}/x86_64', binary=True)
        path = BIN / name
        path.write_bytes(data)
        path.chmod(0o755)
        binaries[name + '_sha256'] = hashlib.sha256(data).hexdigest()
    event('distributed-binaries', **binaries)
    command(['ip', 'addr', 'add', '11.0.0.2/32', 'dev', 'lo'])
    serve(Server(('11.0.0.2', 31000), Echo))
    command(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256',
             '-nodes', '-keyout', str(ROOT / 'cover.key'), '-out', str(ROOT / 'cover.crt'),
             '-subj', '/CN=local-cover.test', '-days', '365'])
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.minimum_version = ssl.TLSVersion.TLSv1_3
    tls.set_alpn_protocols(['h2', 'http/1.1'])
    tls.load_cert_chain(ROOT / 'cover.crt', ROOT / 'cover.key')
    class Cover(Server):
        def get_request(self):
            connection, peer = super().get_request()
            return tls.wrap_socket(connection, server_side=True, do_handshake_on_connect=False), peer
    serve(Cover(('127.0.0.1', 34443), Echo))
    keys = dict(line.split(': ', 1) for line in command([str(BIN / 'xray'), 'x25519']).stdout.strip().splitlines())
    seed = {'private': keys['PrivateKey'], 'public': keys['Password (PublicKey)'],
            'wg_private': base64.b64encode(secrets.token_bytes(32)).decode(),
            'wg_public': base64.b64encode(secrets.token_bytes(32)).decode(),
            'token_hash': hashlib.sha256(NODE_TOKEN.encode()).hexdigest()}
    sql('''INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,
              wg_listen_port,api_port,overlay,egress_allowed,dns_kind,dns_servers)
           SELECT 'n1','lab','Docker service node','11.0.0.2','10.66.0.1',data->>'wg_private',data->>'wg_public',
                  51820,10085,false,true,'system','[]'::jsonb FROM lab_seed;
           INSERT INTO node_lifecycle_state(node_id) VALUES('n1') ON CONFLICT DO NOTHING;
           INSERT INTO node_agent_state(node_id,token_hash,token_prefix,token_created_at)
           SELECT 'n1',data->>'token_hash','broc_node_disposable',now() FROM lab_seed
           ON CONFLICT(node_id) DO UPDATE SET token_hash=EXCLUDED.token_hash;
           INSERT INTO apps(id,label,position) VALUES('app-lab','Service acceptance',10);
           INSERT INTO chains(id,app_id,tenant_id,name,position) VALUES('chn-lab','app-lab','lab','Local Reality',0);
           INSERT INTO ingresses(id,app_id,chain_id,node_id,bind,port,transport_kind,reality_private_key,
               reality_public_key,reality_short_ids,reality_dest,reality_server_names,reality_flow,reality_fallback_mode)
           SELECT 'ing-lab','app-lab','chn-lab','n1','0.0.0.0',24443,'vless-reality',data->>'private',data->>'public',
                  '["0123456789abcdef"]'::jsonb,'127.0.0.1:34443','["local-cover.test"]'::jsonb,
                  'xtls-rprx-vision','custom-site' FROM lab_seed;
           INSERT INTO ingress_client_settings(ingress_id,reality_fingerprint) VALUES('ing-lab','chrome');
           INSERT INTO steps(chain_id,node_id,rules) VALUES('chn-lab','n1','[{"m":{"t":"any"},"a":{"t":"egress","send_through":null}}]');''', seed)
    users = {}
    rules = {}
    for user, access_port, forward_port in [('alice', 32001, 20001), ('bob', 32002, 20002)]:
        created, _ = api('POST', '/users', {'tenant_id': 'lab', 'id': user}, expected=201)
        users[user] = created['user']['uuid']
        api('POST', '/grants', {'tenant_id': 'lab', 'user_id': user, 'app_id': 'app-lab', 'ingress_id': 'ing-lab', 'enabled': True})
        api('PUT', f'/users/lab/{user}/forwarding-nodes/n1', {'enabled': True, 'tcp_allowed': True,
            'udp_allowed': True, 'listen_port_start': 20000, 'listen_port_end': 20999, 'max_rules': 8})
        body = {'node_id': 'n1', 'name': 'Actual payload', 'protocol': 'tcp', 'listen_port': forward_port,
                'target_host': '11.0.0.2', 'target_port': 31000, 'enabled': True}
        view, _ = api('POST', f'/users/lab/{user}/forwarding', body, expected=201)
        rules[user] = (view['rules'][0]['id'], body)
        config = {'log': {'loglevel': 'warning'}, 'inbounds': [{'tag': 'plain', 'listen': '127.0.0.1',
            'port': access_port, 'protocol': 'dokodemo-door', 'settings': {'address': '11.0.0.2', 'port': 31000}}],
            'outbounds': [{'tag': 'proxy', 'protocol': 'vless', 'settings': {'vnext': [{'address': '127.0.0.1',
            'port': 24443, 'users': [{'id': users[user], 'encryption': 'none', 'flow': 'xtls-rprx-vision'}]}]},
            'streamSettings': {'network': 'tcp', 'security': 'reality', 'realitySettings': {
            'serverName': 'local-cover.test', 'fingerprint': 'chrome', 'password': seed['public'], 'shortId': '0123456789abcdef'}}}]}
        path = ROOT / (user + '-client.json')
        save(path, config)
        start_process(user + '-client', [str(BIN / 'xray'), 'run', '-config', str(path)])
    revision = int(sql('SELECT current_revision FROM control_state'))
    deployment, _ = api('POST', '/deployments', {'revision_id': revision, 'idempotency_key': 'service-docker-initial'}, expected=201)
    token_file = ROOT / 'node-token'
    token_file.write_text(NODE_TOKEN)
    agent_env = {**os.environ, 'BROCADE_XRAY_BIN': str(BIN / 'xray')}
    agent_args = [str(BIN / 'agent'), 'run', '--server', 'http://127.0.0.1:18080', '--token-file', str(token_file),
                  '--state-dir', str(STATE), '--apply', 'linux']
    agent = start_process('agent', agent_args, agent_env)
    def deployed():
        detail, _ = api('GET', f'/deployments/{deployment["deployment_id"]}')
        save(RESULTS / 'initial-deployment.json', detail)
        return detail['status'] == 'succeeded' and detail['targets'] and all(
            target['status'] in ['succeeded', 'skipped'] for target in detail['targets'])
    wait_for('initial deployment receipt', deployed, 180)
    wait_for('initial configuration and actual access', lambda: all(can_echo(port) for port in [32001, 32002, 20001, 20002]), 180)
    wait_for('initial serving checkpoint', lambda: api('GET', '/sub/v1/' + users['alice'] + '/clash.yaml', binary=True)[0])
    server_identity = identity()
    require(len(server_identity) == 1, 'expected exactly one managed Xray')
    event('initial-full-deployment', deployment_id=deployment['deployment_id'], status='succeeded',
          xray_identity=server_identity)
    bob = Flow(32002)
    bob_forward = Flow(20002)
    alice, alice_forward = Flow(32001), Flow(20001)
    wait_for('continuous payload baseline', lambda: min(f.samples for f in [bob, bob_forward, alice, alice_forward]) >= 5)

    def healthy_others():
        require(bob.error is None and bob_forward.error is None, 'another user lost an original connection')
        require(can_echo(32002) and can_echo(20002), 'another user cannot open a new connection')
        require(identity() == server_identity, 'permission change restarted Xray')

    now = datetime.now(timezone.utc)
    end = (now + timedelta(seconds=8)).isoformat()
    policy(1, end=end)
    _, headers = api('GET', '/sub/v1/' + users['alice'] + '/clash.yaml', binary=True)
    require('; expire=' in headers.get('subscription-userinfo', ''), 'subscription omitted real expiry')
    began, before = time.monotonic(), bob.samples
    wait_for('natural expiration and actual session revocation', lambda: alice.error and alice_forward.error
             and not can_echo(32001) and not can_echo(20001), 125)
    healthy_others()
    require(bob.samples > before, 'unaffected user made no progress')
    event('natural-expiry-revokes-existing-and-new-sessions', elapsed_secs=time.monotonic() - began,
          bob_samples=bob.samples, bob_forward_samples=bob_forward.samples, xray_identity=identity())
    alice.close()
    alice_forward.close()
    policy(1)
    wait_for('renewal', lambda: can_echo(32001) and can_echo(20001))
    _, headers = api('GET', '/sub/v1/' + users['alice'] + '/clash.yaml', binary=True)
    require('; expire=' not in headers.get('subscription-userinfo', ''), 'unlimited validity invented an expiry')
    healthy_others()
    event('renewal-restores-both-services', xray_identity=identity())
    grant(False)
    rule_id, rule = rules['alice']
    api('PUT', f'/users/lab/alice/forwarding/{rule_id}', {**rule, 'enabled': False})
    wait_for('manual disable', lambda: not can_echo(32001) and not can_echo(20001))
    policy(1, end='2020-01-01T00:00:00Z')
    policy(1)
    time.sleep(6)
    require(not can_echo(32001) and not can_echo(20001), 'renewal bypassed a manual disable')
    healthy_others()
    event('manual-disabled-grant-and-rule-remain-disabled', xray_identity=identity())
    grant(True)
    api('PUT', f'/users/lab/alice/forwarding/{rule_id}', rule)
    wait_for('explicit operator restoration', lambda: can_echo(32001) and can_echo(20001))
    api('PUT', '/quotas', {'tenant_id': 'lab', 'user_id': 'alice', 'app_id': 'app-lab', 'limit_bytes': 524288})
    api('PUT', '/users/lab/alice/forwarding-nodes/n1/quota', {'limit_bytes': 524288})
    for port in [32001, 20001]:
        with socket.create_connection(('127.0.0.1', port), 2) as connection:
            connection.settimeout(3)
            for _ in range(64):
                exchange(connection, b'u' * 16384)
    wait_for('real metering and both quota suspensions', lambda: not can_echo(32001) and not can_echo(20001), 150)
    healthy_others()
    original_total = int(sql("SELECT coalesce(sum(uplink_bytes+downlink_bytes),0)::bigint FROM usage_rollups WHERE user_id='alice'"))
    require(original_total >= 524288, 'quota case did not use real user accounting')
    event('real-access-and-forwarding-quota-exhaustion', access_ledger_bytes=original_total, xray_identity=identity())
    tomorrow = datetime.now(timezone(timedelta(hours=8))).date() + timedelta(days=1)
    preview = policy(tomorrow.day)
    require(preview['access_used_bytes'] >= 524288 and preview['forwarding_used_bytes'] >= 524288,
            'reset-day edit cleared existing consumption')
    require(not can_echo(32001) and not can_echo(20001), 'reset-day edit granted free current-period traffic')
    event('reset-day-preview-preserves-real-ledgers', access_used_bytes=preview['access_used_bytes'],
          forwarding_used_bytes=preview['forwarding_used_bytes'])
    # Calendar month-end/leap boundaries are covered against the same PostgreSQL function
    # in pg_integration. Docker shares the host clock, so recovery here uses real quota edits.
    policy(tomorrow.day, end='2020-01-01T00:00:00Z')
    api('PUT', '/quotas', {'tenant_id': 'lab', 'user_id': 'alice', 'app_id': 'app-lab', 'limit_bytes': None})
    api('PUT', '/users/lab/alice/forwarding-nodes/n1/quota', {'limit_bytes': None})
    def expired_after_recovery():
        user, _ = api('GET', '/users?include_disabled=true')
        owner = next(u for u in user['users'] if u['id'] == 'alice')
        quotas, _ = api('GET', '/quotas')
        forwarding, _ = api('GET', '/users/lab/alice/forwarding')
        return owner['service_state'] == 'expired' and all(not q['suspended_ingresses'] for q in quotas['quotas']) \
            and not forwarding['entitlements'][0]['quota_suspended'] and not can_echo(32001) and not can_echo(20001)
    wait_for('quota recovery while service remains expired', expired_after_recovery, 125)
    healthy_others()
    require(int(sql("SELECT coalesce(sum(uplink_bytes+downlink_bytes),0)::bigint FROM usage_rollups WHERE user_id='alice'")) >= original_total,
            'quota recovery rewrote the historical ledger')
    event('quota-recovery-does-not-bypass-expiry', original_access_ledger_bytes=original_total, xray_identity=identity())
    policy(tomorrow.day)
    wait_for('renewal after quota recovery', lambda: can_echo(32001) and can_echo(20001))
    healthy_others()
    event('renewal-after-quota-recovery', xray_identity=identity())
    # Leave time for the normal 15-second Agent poll to demonstrate the paused state
    # before the service worker activates this policy; never accelerate production timers.
    start = (datetime.now(timezone.utc) + timedelta(seconds=45)).isoformat()
    policy(tomorrow.day, start=start)
    wait_for('scheduled access initially paused', lambda: not can_echo(32001) and not can_echo(20001))
    wait_for('scheduled activation from the normal background worker', lambda: can_echo(32001) and can_echo(20001), 125)
    healthy_others()
    event('scheduled-start-restores-real-traffic', xray_identity=identity())
    policy(tomorrow.day, end='2020-01-01T00:00:00Z')
    wait_for('expiry before process restarts', lambda: not can_echo(32001) and not can_echo(20001))
    stop_process(console)
    console = start_process('console', [str(BIN / 'console')], console_env)
    wait_for('Console restart', lambda: api('GET', '/healthz')[0] is not None, 90)
    stop_process(agent)
    agent = start_process('agent', agent_args, agent_env)
    time.sleep(18)
    require(not can_echo(32001) and not can_echo(20001), 'process restart bypassed persisted expiry')
    healthy_others()
    event('console-and-agent-restarts-preserve-expiry-and-other-user-sockets',
          bob_samples=bob.samples, bob_forward_samples=bob_forward.samples, xray_identity=identity())
    bob.close()
    bob_forward.close()
    migrations = json.loads(sql("SELECT json_agg(version ORDER BY version) FROM _sqlx_migrations"))
    require(migrations == [1, 2], 'unexpected migration chain')
    event('final-state', migrations=migrations, historical_ledger_preserved=True, other_user_errors=0)


if __name__ == '__main__':
    if os.geteuid() != 0 or os.environ.get('BROCADE_USER_SERVICE_DISPOSABLE') != '1':
        raise SystemExit('container-only disposable marker missing')
    if not Path('/.dockerenv').exists():
        raise SystemExit('this fixture requires an isolated Docker container')
    os.umask(0o077)
    result = {'passed': False}
    try:
        main()
        result['passed'] = True
    except Exception:
        (RESULTS / 'failure.txt').write_text(traceback.format_exc())
    finally:
        if not result['passed']:
            # A blocked listener can still leave gRPC receipts looking healthy. Preserve
            # Go's goroutine dump before this disposable runtime is cleaned up.
            for process in identity():
                try:
                    os.kill(process['pid'], signal.SIGQUIT)
                except ProcessLookupError:
                    pass
            time.sleep(1)
        if (STATE / 'logs').exists():
            shutil.copytree(STATE / 'logs', RESULTS / 'xray-logs')
        for process in reversed(PROCESSES):
            stop_process(process)
        result['events'] = EVENTS
        save(RESULTS / 'result.json', result)
