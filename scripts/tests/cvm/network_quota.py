"""Actual Guest traffic checks for Store-allocated shared host quotas."""
import shlex
import subprocess
import selectors


def verify(args, prefix, created, call, guest, wait_instance, record, primary):
    node = created['node_id']
    workspace = call('GET', '/cvm')
    if not workspace['hosts'][0]['capabilities'].get('network_quota'):
        raise RuntimeError('network quota acceptance requires an actual supported runtime')
    quota = next(q for q in workspace['quotas'] if q['user_id'] == 'alice')

    def apply(maximum):
        limits = {'max_states': maximum, 'new_per_second': 1000, 'burst': 2000}
        p = call('POST', '/cvm/quota/network/preview', {
            'tenant_id': quota['tenant_id'], 'user_id': quota['user_id'], 'node_id': node, 'limits': limits})
        assert p['fits']
        call('PUT', '/cvm/quota', {**quota, 'host_network_quotas': [{
            'node_id': node, 'expected_revision': p['revision'], 'budget_revision': p['budget_revision'], 'limits': limits}]}, expected=204)
        wait_instance()
        q = next(q for q in call('GET', '/cvm')['network_quotas'] if q['node_id'] == node and q['user_id'] == 'alice')
        assert q['fresh'] and q['report']['applied'] and q['report']['revision'] == q['revision']
        return q

    # Keep the management stream and business TCP stream open before pausing.
    # Trigger the second payload only after the actual kernel acknowledgement;
    # fixed sleeps would race slow convergence and produce false evidence.
    script = ('~/cvm-probe tcp-hold 203.0.113.10:9090 quota-kept; '
              'printf "\\nHELD:%s\\n" "$?"; '
              '~/cvm-probe tcp4 ' + shlex.quote(primary) + ' paused-primary >/dev/null 2>&1; '
              'printf "PRIMARY:%s\\n" "$?"; '
              '~/cvm-probe tcp4 203.0.113.10:9090 paused-business >/dev/null 2>&1; '
              'printf "BUSINESS:%s\\n" "$?"')
    words = ['ssh', '-i', prefix + '/guest.key', '-o', 'IdentitiesOnly=yes',
             '-o', 'StrictHostKeyChecking=accept-new', '-o', 'ConnectTimeout=5',
             '-o', 'UserKnownHostsFile=' + prefix + '/guest.known-' + created['desired']['id'],
             'cube@' + created['report']['access_address'], script]
    with (args.output / 'quota-stream-private.log').open('wb') as log:
        process = subprocess.Popen(['sh', str(args.ssh_helper), str(args.ssh_port), 'sudo ' + shlex.join(words)],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                if not selector.select(15) or process.stdout.readline() != b'READY\n':
                    raise RuntimeError('long-lived Guest probe did not connect')
            paused = apply(0)
            assert paused['held_states'] == 100000 and paused['report']['states'] > 0
            output, _ = process.communicate(b'continue\n', timeout=20)
            assert process.returncode == 0 and output == b'quota-kept\nHELD:0\nPRIMARY:1\nBUSINESS:1\n', output
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=10)
            apply(100000)
    assert guest('~/cvm-probe tcp4 ' + shlex.quote(primary) + ' recovered-primary') == 'recovered-primary'
    assert guest('~/cvm-probe tcp4 203.0.113.10:9090 recovered-business') == 'recovered-business'
    record('actual primary and business NICs share the quota; pause blocks both while the same established TCP stream survives and recovery admits new traffic')
