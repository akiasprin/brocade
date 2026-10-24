#!/usr/bin/env python3
"""Disposable Docker acceptance using the Console's actual embedded Agent/Xray."""
import support as lab
from support import *


def main():
    require(os.geteuid() == 0 and os.environ.get('BROCADE_USER_SERVICE_DISPOSABLE') == '1' and Path('/.dockerenv').exists(), 'isolated Docker required')
    require(not ROOT.exists() and len(RUN_ID) == 32, 'new disposable runtime required')
    os.umask(0o077)
    ROOT.mkdir(mode=0o700)
    STATE.mkdir()
    BIN.mkdir()
    command(['ip', 'route', 'add', 'default', 'dev', 'eth0'])
    os.environ['PATH'] = str(BIN) + os.pathsep + os.environ['PATH']
    shutil.copy2('/lab/inputs/console', BIN / 'console')
    for name in ['geoip.dat', 'geosite.dat']:
        shutil.copy2(Path('/lab/inputs') / name, BIN / name)
    (BIN / 'console').chmod(0o755)
    env = {**os.environ, 'DATABASE_URL': 'postgres://brocade@database:5432/brocade_service',
           'BROCADE_ADMIN_BIND': '127.0.0.1:18080', 'BROCADE_BOOTSTRAP_TOKEN': BOOTSTRAP,
           'BROCADE_SECRET_KEY': base64.b64encode(secrets.token_bytes(32)).decode(),
           'BROCADE_CACHE_DIR': str(ROOT / 'cache')}
    console = start_process('console', [str(BIN / 'console')], env)
    wait_for('Console startup', lambda: api('GET', '/healthz')[0] is not None, 120)
    _, headers = api('POST', '/auth/init', {'operator_id': 'root', 'display_name': 'Source access lab', 'password': PASSWORD, 'root_tenant': 'lab'}, expected=201, bootstrap=True)
    lab.COOKIE = headers['set-cookie'].split(';')[0]
    binaries = {}
    for name, route in [('agent', 'brocade-agent'), ('xray', 'brocade-xray')]:
        data, _ = api('GET', f'/{route}/x86_64', binary=True)
        (BIN / name).write_bytes(data)
        (BIN / name).chmod(0o755)
        binaries[name + '_sha256'] = hashlib.sha256(data).hexdigest()
    require('source-access-v1' in command([str(BIN / 'xray'), 'version']).stdout, 'current Xray capability missing')
    event('distributed-binaries', **binaries)
    command(['ip', 'addr', 'add', '11.0.0.2/32', 'dev', 'lo'])
    serve(Server(('11.0.0.2', 31000), Echo))
    command(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes', '-keyout', str(ROOT / 'cover.key'), '-out', str(ROOT / 'cover.crt'), '-subj', '/CN=local-cover.test', '-days', '1'])
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
    seed = {'private': keys['PrivateKey'], 'public': keys['Password (PublicKey)'], 'wg_private': base64.b64encode(secrets.token_bytes(32)).decode(), 'wg_public': base64.b64encode(secrets.token_bytes(32)).decode(), 'token_hash': hashlib.sha256(NODE_TOKEN.encode()).hexdigest()}
    sql('''INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,api_port,overlay,egress_allowed,dns_kind,dns_servers)
        SELECT 'n1','lab','Source access node','11.0.0.2','10.66.0.1',data->>'wg_private',data->>'wg_public',51820,10085,false,true,'system','[]'::jsonb FROM lab_seed;
        INSERT INTO node_lifecycle_state(node_id) VALUES('n1') ON CONFLICT DO NOTHING;
        INSERT INTO node_agent_state(node_id,token_hash,token_prefix,token_created_at) SELECT 'n1',data->>'token_hash','broc_node_disposable',now() FROM lab_seed ON CONFLICT(node_id) DO UPDATE SET token_hash=EXCLUDED.token_hash;
        INSERT INTO apps(id,label,position) VALUES('app-lab','Source access acceptance',10);
        INSERT INTO chains(id,app_id,tenant_id,name,position) VALUES('chn-lab','app-lab','lab','Local Reality',0);
        INSERT INTO ingresses(id,app_id,chain_id,node_id,bind,port,transport_kind,reality_private_key,reality_public_key,reality_short_ids,reality_dest,reality_server_names,reality_flow,reality_fallback_mode)
        SELECT 'ing-lab','app-lab','chn-lab','n1','::',24443,'vless-reality',data->>'private',data->>'public','["0123456789abcdef"]','127.0.0.1:34443','["local-cover.test"]','xtls-rprx-vision','custom-site' FROM lab_seed;
        INSERT INTO ingress_client_settings(ingress_id,reality_fingerprint) VALUES('ing-lab','chrome');
        INSERT INTO steps(chain_id,node_id,rules) VALUES('chn-lab','n1','[{"m":{"t":"any"},"a":{"t":"egress","send_through":null}}]');''', seed)
    users = {}
    for user in ['alice', 'bob']:
        created, _ = api('POST', '/users', {'tenant_id': 'lab', 'id': user}, expected=201)
        users[user] = created['user']['uuid']
        api('POST', '/grants', {'tenant_id': 'lab', 'user_id': user, 'app_id': 'app-lab', 'ingress_id': 'ing-lab', 'enabled': True})
    for name, user, port, source, address in [('alice', 'alice', 32001, '127.0.0.1', '127.0.0.1'), ('bob', 'bob', 32002, '127.0.0.1', '127.0.0.1'), ('alice-other', 'alice', 32003, '127.0.0.2', '127.0.0.1'), ('alice-v6', 'alice', 32004, '::1', '::1')]:
        config = {'log': {'loglevel': 'warning'}, 'inbounds': [{'listen': '127.0.0.1', 'port': port, 'protocol': 'dokodemo-door', 'settings': {'address': '11.0.0.2', 'port': 31000}}],
            'outbounds': [{'tag': 'proxy', 'sendThrough': source, 'protocol': 'vless', 'settings': {'vnext': [{'address': address, 'port': 24443, 'users': [{'id': users[user], 'encryption': 'none', 'flow': 'xtls-rprx-vision'}]}]},
            'streamSettings': {'network': 'tcp', 'security': 'reality', 'realitySettings': {'serverName': 'local-cover.test', 'fingerprint': 'chrome', 'password': seed['public'], 'shortId': '0123456789abcdef'}}}]}
        path = ROOT / (name + '-client.json')
        save(path, config)
        start_process(name + '-client', [str(BIN / 'xray'), 'run', '-config', str(path)])
    revision = int(sql('SELECT current_revision FROM control_state'))
    deployment, _ = api('POST', '/deployments', {'revision_id': revision, 'idempotency_key': 'source-access-initial'}, expected=201)
    token_file = ROOT / 'node-token'
    token_file.write_text(NODE_TOKEN)
    agent_env = {**os.environ, 'BROCADE_XRAY_BIN': str(BIN / 'xray')}
    agent_args = [str(BIN / 'agent'), 'run', '--server', 'http://127.0.0.1:18080', '--token-file', str(token_file), '--state-dir', str(STATE), '--apply', 'linux']
    agent = start_process('agent', agent_args, agent_env)
    wait_for('real initial traffic', lambda: all(can_echo(port) for port in [32001, 32002, 32003, 32004]), 180)
    wait_for('successful deployment', lambda: api('GET', f'/deployments/{deployment["deployment_id"]}')[0]['status'] == 'succeeded')
    initial = identity()
    require(len(initial) == 1, 'one managed Xray required')
    alice, bob, other = Flow(32001), Flow(32002), Flow(32003)
    wait_for('original carrier payload', lambda: min(flow.samples for flow in [alice, bob, other]) >= 5)
    event('initial-real-traffic', xray_identity=initial)
    user_path, node_path = '/users/lab/alice/source-access', '/nodes/n1/source-access'
    def update(path, ips):
        view, headers = api('GET', path)
        require(dict((key.lower(), value) for key, value in headers.items()).get('cache-control') == 'no-store', 'source policy must not be cached')
        result, _ = api('PUT', path, {'revision': view['revision'], 'ips': ips})
        return result
    def applied(path):
        view, _ = api('GET', path)
        return view['nodes'] and all(node['phase'] == 'applied' for node in view['nodes'])
    update(user_path, ['::ffff:127.0.0.1'])
    wait_for('specific user source revoked', lambda: alice.error is not None and not can_echo(32001))
    wait_for('user receipt', lambda: applied(user_path))
    require(bob.error is None and other.error is None and can_echo(32002) and can_echo(32003), 'user ban harmed another user or another source')
    event('user-ban-isolates-ip-and-account', alice_closed=True, bob_samples=bob.samples, other_samples=other.samples)
    update(node_path, ['127.0.0.1'])
    wait_for('node source revoked', lambda: bob.error is not None and not can_echo(32002))
    update(user_path, [])
    wait_for('user unban receipt', lambda: applied(user_path))
    scope_checks = {'alice_blocked': not can_echo(32001), 'bob_blocked': not can_echo(32002), 'other_original_alive': other.error is None, 'other_new_allowed': can_echo(32003)}
    event('scope-union-diagnostics', **scope_checks)
    require(all(scope_checks.values()), 'clearing user ban bypassed node ban or harmed another source')
    event('node-ban-and-scope-union', bob_closed=True, other_samples=other.samples)
    update(node_path, [])
    wait_for('unban restores both accounts', lambda: can_echo(32001) and can_echo(32002))
    v6 = Flow(32004)
    wait_for('IPv6 baseline', lambda: v6.samples >= 3)
    update(user_path, ['::1'])
    wait_for('IPv6 source revoked', lambda: v6.error is not None and not can_echo(32004))
    require(can_echo(32001) and other.error is None, 'IPv6 ban harmed IPv4')
    event('ipv6-isolation-and-unban', ipv6_closed=True, ipv4_continues=True)
    update(user_path, ['127.0.0.1'])
    wait_for('ban before restart', lambda: not can_echo(32001))
    wait_for('ban checkpoint receipt', lambda: applied(user_path))
    policy_only_samples = other.samples
    require(other.error is None and policy_only_samples > 10, 'policy edits interrupted unrelated source')
    # Budget admission is a separate Agent service. Its availability during an Agent
    # restart is not evidence for source policy, so verify the durable gate directly
    # while offline and check selective real traffic again when services return.
    other.close()
    stop_process(console)
    stop_process(agent)
    agent = start_process('agent', agent_args, agent_env)
    time.sleep(8)
    cached = json.loads((STATE / 'source-access-policy.json').read_text())
    require(cached['users'] and cached['users'][0]['ips'] == ['127.0.0.1'] and not can_echo(32001), 'offline restart released cached ban')
    receipts = [json.loads(path.read_text()) for path in STATE.glob('source-access-policy.json.*.json')]
    require(any(receipt['healthy'] and receipt['revision'] == cached['revision'] for receipt in receipts), 'offline Xray did not retain the policy')
    console = start_process('console', [str(BIN / 'console')], env)
    wait_for('Console restart', lambda: api('GET', '/healthz')[0] is not None, 90)
    wait_for('restart receipt', lambda: applied(user_path))
    wait_for('unrelated source after restart', lambda: can_echo(32003))
    require(not can_echo(32001), 'restart allowed banned source while other source recovered')
    require(identity() == initial, 'policy operations restarted Xray')
    event('offline-console-and-agent-restart-preserve-ban', other_samples=policy_only_samples, xray_identity=identity())
    update(user_path, [])
    wait_for('final unban', lambda: can_echo(32001) and can_echo(32004))
    before = api('GET', user_path)[0]
    update(user_path, ['127.0.0.9'])
    api('PUT', user_path, {'revision': before['revision'], 'ips': []}, expected=409)
    api('PUT', user_path, {'revision': -1, 'ips': ['example.com']}, expected=400)
    event('api-conflict-and-validation', conflict_rejected=True, invalid_rejected=True)
    update(user_path, [])
    migrations = json.loads(sql('SELECT json_agg(version ORDER BY version) FROM _sqlx_migrations'))
    require(migrations == [1, 2], 'migration chain changed')
    require(int(sql('SELECT current_revision FROM control_state')) == revision, 'source policy created model revision')
    require(policy_only_samples > 10, 'missing original-flow isolation evidence')
    event('final-state', migrations=migrations, no_model_release=True, unchanged_xray_identity=identity() == initial, original_flow_samples=policy_only_samples)
    for flow in [alice, bob, other, v6]:
        flow.close()


if __name__ == '__main__':
    result = {'passed': False}
    try:
        main()
        result['passed'] = True
    except Exception:
        (RESULTS / 'failure.txt').write_text(traceback.format_exc())
    finally:
        if (STATE / 'logs').exists():
            shutil.copytree(STATE / 'logs', RESULTS / 'xray-logs', dirs_exist_ok=True)
        for checkpoint in STATE.glob('source-access*'):
            if checkpoint.is_file(): shutil.copy2(checkpoint, RESULTS / checkpoint.name)
        result['events'] = lab.EVENTS
        save(RESULTS / 'result.json', result)
        for process in reversed(lab.PROCESSES):
            stop_process(process)
