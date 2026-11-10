"""Independent IPv4 entries delivered to the real Guest primary NIC."""
import json
import shlex
import time
import uuid


def prepare(created, prefix, call, guest, remote, namespace, public4, record):
    identity = created['desired']['id']
    route = json.loads(guest('ip -j route show default'))[0]
    gateway = route['gateway']
    # CubeVS presents the Guest its own link-local primary address; the host-side
    # access address is only the DNAT target and never appears inside the Guest.
    inner = json.loads(guest('ip -j -4 addr show dev ' + shlex.quote(route['dev'])))[0]['addr_info'][0]['local']
    whole = '198.51.100.211'
    publication = call('POST', '/cvm/entry-publications', {
        'request_id': uuid.uuid4().hex, 'expected_revision': 0,
        'spec': {
            'name': 'Independent KVM entries', 'provider_node': created['node_id'],
            'addresses': [
                {'address': public4, 'kind': 'shared', 'port_start': 55080, 'port_end': 55081},
                {'address': whole, 'kind': 'dedicated'},
            ],
            'transports': [],
            'grants': [{'tenant_id': created['tenant_id'], 'user_id': created['user_id']}],
            'max_entries': 3,
        },
    }, expected=201)
    entries = []
    for address, mode in [
        (public4, {'kind': 'l4', 'protocol': 'tcp', 'listen_port': 55080, 'target_port': 22}),
        (public4, {'kind': 'l4', 'protocol': 'udp', 'listen_port': 55081, 'target_port': 18081}),
        (whole, {'kind': 'l3'}),
    ]:
        entries.append(call('POST', '/cvm/entries', {
            'request_id': uuid.uuid4().hex, 'instance_id': identity,
            'publication_id': publication['id'], 'address': address, 'mode': mode,
        }, expected=201))
    expected = {item['id'] for item in entries}
    for _ in range(100):
        current = call('GET', '/cvm/' + identity + '/entries')
        if {item['id'] for item in current if item['state'] == 'active'} == expected:
            break
        time.sleep(.3)
    else:
        raise RuntimeError('real Guest primary entries did not become active')
    guest('nohup ~/cvm-probe udp-server ' + shlex.quote(inner + ':18081') +
          ' >~/entry-udp.log 2>&1 </dev/null & echo $! >~/entry-udp.pid')

    def check():
        for address, port in [(public4, 55080), (whole, 22)]:
            command = ['ip', 'netns', 'exec', namespace, 'ssh', '-b', '203.0.113.11',
                       '-i', prefix + '/guest.key', '-o', 'IdentitiesOnly=yes',
                       '-o', 'StrictHostKeyChecking=accept-new',
                       '-o', 'UserKnownHostsFile=' + prefix + '/entry.known',
                       '-o', 'ConnectTimeout=5', '-p', str(port), 'cube@' + address,
                       'printf "%s" "$SSH_CONNECTION"']
            peer = remote('sudo ' + shlex.join(command)).stdout.decode().split()
            assert peer[0] == gateway and peer[2] == inner, 'entry did not use primary delivery and gateway SNAT: ' + ' '.join(peer)
        for endpoint in [public4 + ':55081', whole + ':18081']:
            payload = 'f' * 4096
            command = ['ip', 'netns', 'exec', namespace, prefix + '/network-probe',
                       'udp4', endpoint, payload, '203.0.113.11']
            assert remote('sudo ' + shlex.join(command)).stdout.decode() == payload
        remote('sudo ' + shlex.join(['ip', 'netns', 'exec', namespace, 'ping',
                                    '-c', '1', '-W', '2', whole]))

    def cleanup():
        for entry in entries:
            call('DELETE', '/cvm/entries/' + entry['id'], {
                'request_id': uuid.uuid4().hex, 'expected_revision': entry['revision'],
            }, expected=202)
        for _ in range(100):
            if not call('GET', '/cvm/' + identity + '/entries'):
                break
            time.sleep(.3)
        else:
            raise RuntimeError('entry cleanup receipts did not release the entries')
        call('DELETE', '/cvm/entry-publications/' + publication['id'], {
            'request_id': uuid.uuid4().hex, 'expected_revision': publication['revision'],
            'admitting': False,
        })
        guest('kill "$(cat ~/entry-udp.pid)" && rm ~/entry-udp.pid')
        record('independent entries are withdrawn and their address claims released after node receipts')

    check()
    record('real primary NIC receives shared TCP/fragmented UDP and whole-IP TCP/UDP/ICMP; Guest sees gateway source')
    return check, cleanup, whole
