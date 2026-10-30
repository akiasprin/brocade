#!/usr/bin/env python3
"""Container-only real HTTP and TCP continuity test for changing a chain's group."""
import support as lab
from support import *
import http.server
import sys

MODE = sys.argv[1] if len(sys.argv) > 1 else 'default'
require(MODE in ['default', 'handoff'], 'unknown test mode')

class PublicationFailure(Exception):
    pass

class HttpTarget(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        if self.path == '/stream':
            self.send_header('Content-Length', str(800 * 64))
            self.end_headers()
            try:
                for number in range(800):
                    self.wfile.write(number.to_bytes(8, 'big') + b'h' * 56)
                    self.wfile.flush()
                    time.sleep(.05)
            except OSError:
                pass
        else:
            payload = self.path.encode()
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
    def log_message(self, *args):
        pass

class HttpRequests:
    def __init__(self):
        self.stop = threading.Event()
        self.success = 0
        self.errors = []
        self.latencies = []
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()
    def run(self):
        number = 0
        while not self.stop.is_set():
            client = http.client.HTTPConnection('127.0.0.1', 32003, timeout=2)
            started = time.monotonic()
            try:
                path = '/request-' + str(number)
                client.request('GET', path)
                response = client.getresponse()
                require(response.status == 200 and response.read() == path.encode(), 'HTTP payload mismatch')
                self.success += 1
                self.latencies.append(time.monotonic() - started)
            except Exception as error:
                self.errors.append({'request': number, 'error': type(error).__name__})
            finally:
                client.close()
            number += 1
            self.stop.wait(.1)
    def close(self):
        self.stop.set()
        self.thread.join(4)

class LongHttp:
    def __init__(self):
        self.samples = 0
        self.error = None
        self.finished_at = None
        self.connects = 0
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()
    def run(self):
        owner = self
        class Connection(http.client.HTTPConnection):
            def connect(self):
                owner.connects += 1
                super().connect()
        client = Connection('127.0.0.1', 32003, timeout=3)
        try:
            client.request('GET', '/stream')
            response = client.getresponse()
            require(response.status == 200, 'long request rejected')
            for number in range(800):
                require(response.read(64) == number.to_bytes(8, 'big') + b'h' * 56, 'long HTTP body truncated or changed')
                self.samples += 1
            self.finished_at = time.monotonic()
        except Exception as error:
            self.error = type(error).__name__
            self.finished_at = time.monotonic()
        finally:
            client.close()

def main():
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
    lab.COOKIE = headers['set-cookie'].split(';')[0]
    api('PUT', '/deployment-settings', {'unlock_detection_mode': 'manual'})
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


    # Start an HTTP origin and a real VLESS client for HTTP; every short request creates a new
    # connection. LongHttp keeps exactly one HTTP request and never reconnects or retries.
    serve(http.server.ThreadingHTTPServer(('11.0.0.2', 31001), HttpTarget))
    http_config = json.loads((ROOT / 'alice-client.json').read_text())
    http_config['inbounds'][0]['port'] = 32003
    http_config['inbounds'][0]['settings']['port'] = 31001
    save(ROOT / 'http-client.json', http_config)
    start_process('http-client', [str(BIN / 'xray'), 'run', '-config', str(ROOT / 'http-client.json')])
    def http_ready():
        client = http.client.HTTPConnection('127.0.0.1', 32003, timeout=2)
        try:
            client.request('GET', '/ready')
            return client.getresponse().read() == b'/ready'
        finally:
            client.close()
    wait_for('HTTP client readiness', http_ready)
    wait_for('real source-group ledger', lambda: int(sql("SELECT coalesce(sum(uplink_bytes+downlink_bytes),0)::bigint FROM usage_rollups WHERE app_id='app-lab'")) > 0, 90)
    old_ledger = int(sql("SELECT coalesce(sum(uplink_bytes+downlink_bytes),0)::bigint FROM usage_rollups WHERE app_id='app-lab'"))
    settings, _ = api('GET', '/deployment-settings')
    require(settings['xray_handoff_beta'] is False, 'test default must have handoff disabled')
    if MODE == 'handoff':
        api('PUT', '/deployment-settings', {'xray_handoff_beta': True})
    flow_map = {'moved_alice': alice, 'moved_bob': bob, 'forward_alice': alice_forward, 'forward_bob': bob_forward}
    requests = HttpRequests()
    long = LongHttp()
    wait_for('live HTTP requests before move', lambda: requests.success >= 5 and long.samples >= 5)
    baseline = {name: flow.samples for name, flow in flow_map.items()}
    before_http = requests.success
    event('live-request-baseline', mode=MODE, tcp_flows={name:{'samples':flow.samples,'error':flow.error} for name,flow in flow_map.items()},
          long_samples=long.samples,long_error=long.error,short_success=requests.success,short_errors=requests.errors)
    require(all(flow.error is None for flow in flow_map.values()) and long.error is None and not requests.errors,
            'initial traffic was already unhealthy before the move')
    ops = [{'op':'create_app','app':{'id':'app-a1b2','label':'New billing group'}},
           {'op':'move_chain','app_id':'app-lab','chain_id':'chn-lab','target_app_id':'app-a1b2'}]
    preview, _ = api('POST', '/model/preview', {'ops':ops})
    require(sql("SELECT app_id FROM chains WHERE id='chn-lab'") == 'app-lab', 'preview persisted the move')
    applied, _ = api('POST', '/model/apply', {'ops':ops})
    revision = applied['revision_id']
    require(sql("SELECT app_id FROM chains WHERE id='chn-lab'") == 'app-a1b2', 'move was not committed')
    event('draft-and-commit-preserve-live-requests', mode=MODE, revision=revision, long_samples=long.samples,
          short_success=requests.success, source_ledger_bytes=old_ledger,
          tcp_errors={name:flow.error for name,flow in flow_map.items()},long_error=long.error,short_errors=requests.errors)
    require(all(flow.error is None for flow in flow_map.values()) and long.error is None and not requests.errors, 'draft or commit interrupted traffic')
    plan, _ = api('POST', '/deployments/plan', {'revision_id':revision})
    save(RESULTS / 'move-plan.json', plan)
    moved, _ = api('POST', '/deployments', {'revision_id':revision,'idempotency_key':'chain-move-'+MODE}, expected=201)
    def finished():
        detail, _ = api('GET', '/deployments/' + str(moved['deployment_id']))
        save(RESULTS / 'move-deployment.json', detail)
        if detail['status'] in ['failed','halted','paused']:
            raise PublicationFailure('publication did not converge: '+detail['status'])
        return detail['status'] == 'succeeded'
    wait_for('real Agent move publication', finished, 200)
    publication_finished = time.monotonic()
    wait_for('complete original HTTP request', lambda: long.finished_at is not None, 55)
    time.sleep(5)
    requests.close()
    flows = {name:{'before':baseline[name],'after':flow.samples,'error':flow.error,'connect_calls':1} for name,flow in flow_map.items()}
    event('real-publication-traffic', mode=MODE, deployment_id=moved['deployment_id'], tcp_flows=flows,
          long_http={'chunks':long.samples,'expected_chunks':800,'error':long.error,'connect_calls':long.connects,
                     'spanned_publication':long.finished_at >= publication_finished},
          new_http={'before':before_http,'success':requests.success,'errors':requests.errors,
                    'max_latency_seconds':max(requests.latencies,default=0)},
          initial_xray_identity=server_identity,final_xray_identity=identity())
    for flow in flow_map.values():
        flow.close()
    require(all(row['error'] is None and row['after'] > row['before'] for row in flows.values()), 'configuration publication interrupted an original TCP connection')
    require(long.error is None and long.samples == 800 and long.connects == 1 and long.finished_at >= publication_finished,
            'the original long HTTP request did not span publication intact')
    require(not requests.errors and requests.success > before_http, 'new HTTP request failed during publication')
    wait_for('real target-group ledger', lambda: int(sql("SELECT coalesce(sum(uplink_bytes+downlink_bytes),0)::bigint FROM usage_rollups WHERE app_id='app-a1b2'")) > 0, 90)
    old_after = int(sql("SELECT coalesce(sum(uplink_bytes+downlink_bytes),0)::bigint FROM usage_rollups WHERE app_id='app-lab'"))
    new_after = int(sql("SELECT coalesce(sum(uplink_bytes+downlink_bytes),0)::bigint FROM usage_rollups WHERE app_id='app-a1b2'"))
    require(old_after >= old_ledger and new_after > 0, 'billing history lost or new owner never charged')
    event('real-billing-switch', mode=MODE, old_before=old_ledger,old_after=old_after,new_after=new_after)

if __name__ == '__main__':
    require(os.geteuid() == 0 and os.environ.get('BROCADE_USER_SERVICE_DISPOSABLE') == '1' and Path('/.dockerenv').exists(), 'isolated Docker required')
    os.umask(0o077)
    result = {'passed':False,'mode':MODE}
    try:
        main()
        result['passed'] = True
    except Exception:
        (RESULTS / 'failure.txt').write_text(traceback.format_exc())
    finally:
        if (STATE / 'logs').exists():
            shutil.copytree(STATE / 'logs', RESULTS / 'xray-logs', dirs_exist_ok=True)
        for process in reversed(PROCESSES):
            stop_process(process)
        result['events'] = EVENTS
        save(RESULTS / 'result.json', result)
