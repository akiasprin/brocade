"""Independent, read-only observation acceptance on an explicitly owned KVM lab."""
import json
import shlex
import time


def verify(args, created, network, prefix, call, guest, py, remote, converge, wait_instance, record):
    identifier = created['desired']['id']
    route = '/cvm/' + identifier + '/monitoring'
    monitor = call('GET', route)
    latest = monitor['latest']
    diagnostics = latest['diagnostics']
    assert monitor['source'] == 'independent'
    assert monitor['health']['runtime_state'] == 'running'
    assert monitor['health']['guest_state'] == 'reachable'
    assert monitor['health']['ssh_state'] == 'reachable'
    assert any(c['cpu'] == -1 for c in diagnostics['cpus'])
    assert any(c['cpu'] >= 0 for c in diagnostics['cpus'])
    assert diagnostics['load'] is not None and diagnostics['memory']['Cached'] >= 0
    assert diagnostics['inode_total'] > 0 and diagnostics['tcp']['CurrEstab'] >= 0
    assert all(n['rx_pps'] is not None and n['drops'] is not None for n in latest['interfaces'])
    assert diagnostics['disk_io'] is not None or 'root_block_io' in diagnostics['unsupported']
    for seconds in (3600, 21600, 86400):
        history = call('GET', route + '?window_secs=' + str(seconds))
        assert history['range_end_unix_millis'] - history['range_start_unix_millis'] == seconds * 1000
        assert history['samples'] and len(history['samples']) <= 2000
    record('independent rich Guest diagnostics, packet counters, explicit disk I/O support and all three history ranges reach owner-scoped Docker HTTP')
    primary = next(n for n in created['desired']['spec']['interfaces'] if n['purpose'] == 'contact')
    target = {'id': identifier, 'generation': created['desired']['generation'], 'power': 'running', 'services': [
        {'id': 'accept-ssh', 'interface_id': primary['id'], 'host': created['report']['access_address'], 'port': 22},
        {'id': 'accept-closed', 'interface_id': primary['id'], 'host': created['report']['access_address'], 'port': 65000},
        {'id': 'accept-unowned', 'interface_id': primary['id'], 'host': '127.0.0.1', 'port': 22},
    ]}
    request = {'targets': [target], 'active_ids': [identifier]}
    input_path, output_path = prefix + '/observation-input.json', prefix + '/observation-output.json'
    py('import pathlib,sys,os;p=pathlib.Path(' + repr(input_path) + ');p.write_bytes(sys.stdin.buffer.read());p.chmod(0o600)', json.dumps(request).encode())
    environment = {'BROCADE_RUN_CVM_KVM': '1', 'BROCADE_CVM_CONFIG': prefix + '/config.json',
                   'BROCADE_CVM_STATE_DIR': prefix + '/runtime', 'BROCADE_CVM_OBSERVATION_INPUT': input_path,
                   'BROCADE_CVM_OBSERVATION_OUTPUT': output_path}
    name = 'cvm::tests::kvm_observation_cycle'
    result = remote('sudo env ' + ' '.join(shlex.quote(k + '=' + v) for k, v in environment.items()) + ' '
                    + shlex.quote(prefix + '/agent-test') + ' --exact ' + name + ' --ignored --nocapture')
    assert b'1 passed' in result.stdout and (name + ' ... ok').encode() in result.stdout
    observations = json.loads(py('import pathlib;print(pathlib.Path(' + repr(output_path) + ').read_text())'))
    matches = [o for o in observations if len(o['services']) == 3]
    assert matches
    states = {p['id']: p['state'] for p in matches[-1]['services']}
    assert states == {'accept-ssh': 'reachable', 'accept-closed': 'unreachable', 'accept-unowned': 'unknown'}
    record('actual machine-local TCP distinguishes open SSH, a closed service and an unowned address without probing the unowned target')
    converge()  # Production targets remove the temporary probe scope and its cache.
    original_boot = guest('cat /proc/sys/kernel/random/boot_id')
    original_generation = created['desired']['generation']
    bridge = next(b for b in network['bridges'] if b['id'] == 'business-b')
    address = shlex.quote(bridge['ipv4_gateway'] + '/24')
    device = shlex.quote(bridge['bridge'])
    began = int(time.time() * 1000)
    remote('sudo ip addr del ' + address + ' dev ' + device)
    try:
        for _ in range(45):
            converge()
            host = next(h for h in call('GET', '/cvm')['hosts'] if h['node_id'] == created['node_id'])
            current = call('GET', '/cvm/' + identifier)
            value = call('GET', route)
            if (not host['fresh'] and host['error'] and current['status'] != 'applied'
                    and value['state'] == 'live' and value['latest']['observed_at_unix_millis'] > began):
                assert value['health']['guest_state'] == 'reachable'
                assert value['health']['runtime_state'] == 'running'
                assert current['desired']['generation'] == original_generation
                assert guest('cat /proc/sys/kernel/random/boot_id') == original_boot
                break
            time.sleep(1)
        else:
            raise RuntimeError('Guest monitoring was gated on failed inventory or lifecycle application')
        record('new actual Guest samples continue through a real native catalogue and configuration failure while Guest boot and desired generation stay unchanged')
    finally:
        remote('sudo ip addr replace ' + address + ' dev ' + device)
    return wait_instance()
