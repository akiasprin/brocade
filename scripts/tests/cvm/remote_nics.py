"""Real Guest acceptance for reusable Store allocations; no local-LAN compatibility."""
import hashlib
import json
import shlex
import time
import uuid

# Longer than any IPv4 address the echo returns; the probe reads exactly this many bytes.
PEER_REQUEST = 'peer-address-' + 'x' * 27


def verify(args, created, prefix, unit, call, guest, py, remote, upload, wait_instance, record, primary_nat_endpoint):
    node = created['node_id']; identity = created['desired']['id']; network_unit = unit + '-nics'
    lab_namespace = 'bcn-lab-' + uuid.uuid4().hex[:10]
    root_link = 'wn' + uuid.uuid4().hex[:10]
    unit_path = '/etc/systemd/system/' + network_unit + '.service'
    upload(args.xray_binary, 'xray', True)
    upload(args.network_probe_binary, 'network-probe', True)
    config = json.loads(py('''import json,subprocess,socket,sys
links=json.loads(subprocess.check_output(['ip','-j','route','get','1.1.1.1']))
route=links[0];address=route.get('prefsrc') or route.get('src')
assert address and socket.inet_pton(socket.AF_INET,address)
print(json.dumps({'public4':address}))
'''))
    public4, public6 = config['public4'], '2001:db8:cafe:10::20'
    context = {'namespace': lab_namespace, 'link': root_link, 'public6': public6, 'unit': network_unit, 'prefix': prefix}
    py('''import json,pathlib,subprocess,sys
c=json.load(sys.stdin);base=pathlib.Path(c['prefix']);p=base/'nic-lab.json';p.write_text(json.dumps(c));p.chmod(0o600)
def run(*args):subprocess.run(args,check=True,stdout=subprocess.DEVNULL)
assert not pathlib.Path('/run/netns',c['namespace']).exists()
run('ip','netns','add',c['namespace']);run('ip','link','add',c['link'],'type','veth','peer','name','eth0','netns',c['namespace'])
run('ip','link','set',c['link'],'alias','brocade-cvm-test:'+c['unit'],'mtu','1400','up')
run('ip','addr','add','203.0.113.1/24','dev',c['link']);run('ip','-6','addr','add','2001:db8:cafe::1/64','dev',c['link'],'nodad')
run('ip','-6','addr','add',c['public6']+'/128','dev','lo','nodad')
for args in [('link','set','lo','up'),('link','set','eth0','mtu','1400','up'),('addr','add','203.0.113.10/24','dev','eth0'),('addr','add','203.0.113.11/24','dev','eth0'),('-6','addr','add','2001:db8:cafe::10/64','dev','eth0','nodad'),('-6','addr','add','2001:db8:cafe::11/64','dev','eth0','nodad'),('route','add','default','via','203.0.113.1'),('-6','route','add','default','via','2001:db8:cafe::1')]:run('ip','-n',c['namespace'],*args)
''', json.dumps(context).encode())
    echo = r'''import socket,threading,pathlib
sockets=[]
def serve(family,kind,port,address=None):
 s=socket.socket(family,kind)
 if family==socket.AF_INET6:s.setsockopt(socket.IPPROTO_IPV6,socket.IPV6_V6ONLY,1)
 s.bind((address or ('::' if family==socket.AF_INET6 else '0.0.0.0'),port));sockets.append(s)
 if kind==socket.SOCK_STREAM:
  s.listen(16)
  def connection(c):
   with c:
    c.settimeout(150)
    while True:
     body=c.recv(65535)
     if not body:break
     # The probe reads exactly its request length: pad the address to it.
     c.sendall(c.getpeername()[0].encode().ljust(len(body)) if body.startswith(b'peer-address') else body)
  while True:
   c,_=s.accept()
   threading.Thread(target=connection,args=(c,),daemon=True).start()
 else:
  while True:
   body,peer=s.recvfrom(65535);s.sendto(body,peer)
for f in (socket.AF_INET,socket.AF_INET6):
 threading.Thread(target=serve,args=(f,socket.SOCK_STREAM,9090),daemon=True).start()
 # A wildcard UDP socket can select the namespace's other address for replies.
 # Bind each destination so connected clients receive the matching source.
 for address in (('203.0.113.10','203.0.113.11') if f==socket.AF_INET else ('2001:db8:cafe::10','2001:db8:cafe::11')):
  threading.Thread(target=serve,args=(f,socket.SOCK_DGRAM,9091,address),daemon=True).start()
pathlib.Path(__file__).with_suffix('.ready').touch()
threading.Event().wait()
'''
    py('import pathlib,sys;p=pathlib.Path(' + repr(prefix + '/nic-echo.py') + ');p.write_bytes(sys.stdin.buffer.read());p.chmod(0o600)', echo.encode())
    remote('sudo sh -c ' + shlex.quote('ip netns exec ' + lab_namespace + ' python3 ' + prefix + '/nic-echo.py >' + prefix + '/nic-echo.log 2>&1 & echo $! >' + prefix + '/nic-echo.pid'))
    environment = {'BROCADE_RUN_CVM_KVM': '1', 'BROCADE_AGENT_SERVER': args.guest_visible_console.format(port=json.loads((args.output / 'admin.credentials.json').read_text())['origin'].rsplit(':', 1)[1]), 'BROCADE_NODE_TOKEN_FILE': prefix + '/node.token', 'BROCADE_AGENT_STATE_DIR': prefix + '/agent-state', 'BROCADE_AGENT_APPLY': 'linux', 'BROCADE_CVM_CONFIG': prefix + '/config.json', 'BROCADE_CVM_STATE_DIR': prefix + '/runtime', 'BROCADE_XRAY_BIN': prefix + '/xray'}
    service = '# Brocade isolated CVM acceptance\n[Unit]\nAfter=network-online.target\n[Service]\nType=simple\nLogNamespace=' + network_unit + '\n' + ''.join('Environment=' + json.dumps(k + '=' + v) + '\n' for k, v in environment.items()) + 'ExecStart=' + shlex.join([prefix + '/agent-test', '--exact', 'cvm::nics::tests::kvm_network_service', '--ignored', '--nocapture']) + '\nKillMode=control-group\nTimeoutStopSec=10\n'
    py('import pathlib,sys;p=pathlib.Path(' + repr(unit_path) + ');assert not p.exists();p.write_bytes(sys.stdin.buffer.read());p.chmod(0o600)', service.encode())
    remote('sudo systemctl daemon-reload && sudo systemctl start ' + shlex.quote(network_unit))
    # A static probe keeps the Guest test independent of curl/Python packages.
    words = ['ssh', '-i', prefix + '/guest.key', '-o', 'IdentitiesOnly=yes', '-o', 'StrictHostKeyChecking=accept-new', '-o', 'UserKnownHostsFile=' + prefix + '/guest.known-' + identity, 'cube@' + created['report']['access_address'], 'cat > ~/cvm-probe && chmod 0700 ~/cvm-probe']
    remote('sudo cat ' + shlex.quote(prefix + '/network-probe') + ' | sudo ' + shlex.join(words))
    assert guest('sha256sum ~/cvm-probe').split()[0] == hashlib.sha256(args.network_probe_binary.read_bytes()).hexdigest()
    # CubeVS sends primary NAT packets through the physical underlay. The
    # business-network namespace above lives behind the host FIB instead and
    # is intentionally not a public-uplink fixture.
    assert guest('~/cvm-probe tcp4 ' + shlex.quote(primary_nat_endpoint) + ' primary-nat') == 'primary-nat'
    record('real primary-only Guest uses the host NAT as its lowest-priority public egress')
    for _ in range(80):
        catalog = call('GET', '/cvm/networks')
        provider = next((p for p in catalog['providers'] if p['node_id'] == node and p['fresh']), None)
        if provider and set(provider['capabilities']['transports']) == {'xray', 'wireguard'}: break
        time.sleep(.3)
    else: raise RuntimeError('both real IP transport implementations were not reported')
    spec = {'name': 'Reusable KVM acceptance', 'provider_node': node, 'resource_domain': 'pool-' + uuid.uuid4().hex, 'ipv4_pool': '10.120.10.0/24', 'ipv6_pool': 'fd12:3456::/120', 'public_ipv4_pool': None, 'public_ipv6_pool': None, 'shared_ipv4': public4, 'shared_ipv6': public6, 'address_mode': 'shared_nat', 'destinations': ['0.0.0.0/0', '::/0'], 'route_presets': ['0.0.0.0/0', '::/0'], 'dns_suggestions': [], 'transports': [{'kind': kind, 'listen_side': 'provider', 'endpoint': public4, 'port_start': start, 'port_end': start + 100, 'mtu': 1400} for kind, start in [('wireguard', 42000), ('xray', 43000)]], 'grants': [{'tenant_id': created['tenant_id'], 'user_id': created['user_id']}], 'limits': {'max_attachments': 4, 'upload_bps': None, 'download_bps': None, 'monthly_bytes': None}}
    publication = call('POST', '/cvm/networks', {'request_id': uuid.uuid4().hex, 'expected_revision': 0, 'spec': spec}, expected=201)
    path = '/cvm/' + identity
    boot = guest('cat /proc/sys/kernel/random/boot_id')
    def ready(item):
        for _ in range(100):
            state = call('GET', path + '/network')
            good = True
            for nic in item['desired']['spec']['interfaces']:
                if not nic.get('allocated'): continue
                binding = next((b for b in state['bindings'] if b['interface_id'] == nic['id'] and b['phase'] == 'active' and b['transport'] == nic['allocated']['transport']), None)
                good &= bool(binding and all(binding[s] and binding[s]['generation'] == binding['generation'] and binding[s]['configured'] and binding[s]['reachable'] is True and not binding[s]['error'] for s in ('consumer', 'provider')))
            if good and all(b['phase'] == 'active' for b in state['bindings']): return state
            time.sleep(.3)
        raise RuntimeError('allocated KVM NIC transport did not become ready')
    def accepted(suffix, body):
        nonlocal created
        created = call('PUT' if suffix != '/interfaces' else 'POST', path + suffix, {'request_id': uuid.uuid4().hex, 'expected_generation': created['desired']['generation'], **body}, expected=202)
        created = wait_instance(); return created
    def reachable(command, expected):
        # Applied Guest routes and a retained binding report can precede ARP/ND
        # or the restarted controller's first round. Prove current data flow
        # before asserting the next transition; a cached report is insufficient.
        deadline = time.monotonic() + 15
        while True:
            try:
                if guest(command) == expected:
                    return
            except RuntimeError:
                pass
            if time.monotonic() >= deadline:
                raise RuntimeError('actual Guest data flow did not converge')
            time.sleep(.3)
    accepted('/interfaces', {'publication_id': publication['id'], 'transport': 'wireguard'})
    ready(created)
    first = created['desired']['spec']['interfaces'][1]
    # Admission is independent of already assigned data and reconnection.
    call('PUT', '/cvm/networks/' + publication['id'] + '/admission', {'request_id': uuid.uuid4().hex, 'expected_revision': 1, 'admitting': False})
    call('POST', path + '/interfaces', {'request_id': uuid.uuid4().hex, 'expected_generation': created['desired']['generation'], 'publication_id': publication['id'], 'transport': 'wireguard'}, expected=409)
    assert guest('~/cvm-probe tcp4 203.0.113.10:9090 keep-existing') == 'keep-existing'
    call('PUT', '/cvm/networks/' + publication['id'] + '/admission', {'request_id': uuid.uuid4().hex, 'expected_revision': 1, 'admitting': True})
    accepted('/interfaces', {'publication_id': publication['id'], 'transport': 'wireguard'})
    ready(created); second = created['desired']['spec']['interfaces'][2]
    assert first['id'] != second['id'] and first['allocated']['ipv4'] != second['allocated']['ipv4'] and first['allocated']['mac'] != second['allocated']['mac']
    assert len(json.loads(guest('ip -j link show'))) >= 4
    record('one publication creates two distinct real virtio NICs; stopped admission preserves existing business data')
    routing = created['desired']['spec']['routing']
    routing['routes'] = [{'destination': '0.0.0.0/0', 'interface_id': first['id']}, {'destination': '::/0', 'interface_id': first['id']}, {'destination': '203.0.113.10/32', 'interface_id': second['id']}, {'destination': '2001:db8:cafe::10/128', 'interface_id': second['id']}, {'destination': '203.0.113.11/32', 'interface_id': second['id'], 'service': {'protocol': 'tcp', 'port_start': 9090, 'port_end': 9090}}]
    accepted('/routes', {'expected_revision': routing['revision'], 'routes': routing})
    for protocol, address in [('tcp4', '203.0.113.10:9090'), ('udp4', '203.0.113.10:9091'), ('tcp6', '[2001:db8:cafe::10]:9090'), ('udp6', '[2001:db8:cafe::10]:9091')]: reachable('~/cvm-probe ' + protocol + ' ' + shlex.quote(address) + ' routed-' + protocol, 'routed-' + protocol)
    actual = {n['id']: n for n in created['report']['interfaces']}
    assert json.loads(guest('ip -j route get 203.0.113.10'))[0]['dev'] == actual[second['id']]['device']
    # Numeric protocols: minimal Guest images have no /etc/protocols, so iproute2 rejects "tcp".
    assert json.loads(guest('ip -j route get 203.0.113.11 ipproto 6 dport 9090'))[0]['dev'] == actual[second['id']]['device']
    assert json.loads(guest('ip -j route get 203.0.113.11 ipproto 17 dport 9091'))[0]['dev'] == actual[first['id']]['device']
    assert guest('~/cvm-probe udp4 203.0.113.11:9091 wrong-card ' + second['allocated']['ipv4'] + ' >/dev/null 2>&1; printf "%s" "$?"') != '0'
    import entry_delivery
    inbound, release_entries, entry_source = entry_delivery.prepare(created, prefix, call, guest, remote, lab_namespace, public4, record)
    reachable('~/cvm-probe tcp4 203.0.113.10:9090 ' + PEER_REQUEST, entry_source)
    record('whole-IP entry is also the source of NAT egress through the same machine')
    # Freeze only the owned data controller; the other card remains a positive control.
    remote('sudo systemctl stop ' + shlex.quote(network_unit))
    py('''import json,pathlib,subprocess,sys
c=json.load(sys.stdin);a=json.loads(pathlib.Path(c['prefix'],'agent-state','cvm-nics','anchor-'+c['nic']+'-c.json').read_text());links=json.loads(subprocess.check_output(['ip','-n',a['namespace'],'-j','link','show']));link=next(x['ifname'] for x in links if x.get('ifalias')=='brocade-binding:'+c['binding']);subprocess.run(['ip','-n',a['namespace'],'link','set',link,'down'],check=True)
''', json.dumps({'prefix': prefix, 'nic': second['id'], 'binding': next(b['id'] for b in ready(created)['bindings'] if b['interface_id'] == second['id'] and b['phase'] == 'active')}).encode())
    assert guest('~/cvm-probe udp4 203.0.113.11:9091 healthy-other') == 'healthy-other'
    assert guest('~/cvm-probe tcp4 203.0.113.10:9090 no-fallback >/dev/null 2>&1; printf "%s" "$?"') != '0'
    assert guest('~/cvm-probe tcp4 203.0.113.11:9090 no-service-fallback >/dev/null 2>&1; printf "%s" "$?"') != '0'
    remote('sudo systemctl start ' + shlex.quote(network_unit)); ready(created)
    reachable('~/cvm-probe tcp4 203.0.113.10:9090 restarted-controller', 'restarted-controller')
    record('prefix and protocol/port routing agree with source validation; a failed selected NIC never falls back to the healthy default NIC')
    for kind in ('xray', 'wireguard'):
        accepted('/interfaces/' + second['id'] + '/transport', {'transport': kind}); ready(created)
        current = next(n for n in created['desired']['spec']['interfaces'] if n['id'] == second['id'])
        for field in ('allocation_id', 'mac', 'ipv4', 'ipv6'): assert current['allocated'][field] == second['allocated'][field]
        assert guest('~/cvm-probe tcp4 203.0.113.10:9090 switch-' + kind) == 'switch-' + kind
        assert guest('cat /proc/sys/kernel/random/boot_id') == boot
        inbound()
    record('real Guest identity, MAC and assigned IPs survive both transport changes without reboot')
    # Rebuild a deliberately drifted managed route table from the same intent.
    guest('sudo -n ip route flush table 30001')
    created = wait_instance()
    assert guest('~/cvm-probe tcp4 203.0.113.10:9090 recovered-route') == 'recovered-route'
    record('managed Guest routes converge after drift from an unchanged desired generation')
    # Withdraw entries before traffic leaves through the host uplink: the lab's
    # whole-IP entry address is routed only on the lab link, and while it exists
    # it is (correctly) the egress source of this machine's NAT NICs.
    release_entries()
    reachable('~/cvm-probe tcp4 203.0.113.10:9090 ' + PEER_REQUEST, public4)
    import network_quota
    network_quota.verify(args, prefix, created, call, guest, wait_instance, record, primary_nat_endpoint)
    # Normal unmount cannot erase references; delete its routes first.
    call('POST', path + '/operations', {'request_id': uuid.uuid4().hex, 'expected_generation': created['desired']['generation'], 'action': 'detach', 'interface_id': second['id']}, expected=409)
    for nic in (second, first):
        routing = created['desired']['spec']['routing']; routing['routes'] = [r for r in routing['routes'] if r['interface_id'] != nic['id']]; routing['priorities'] = [r for r in routing['priorities'] if r['interface_id'] != nic['id']]
        accepted('/routes', {'expected_revision': routing['revision'], 'routes': routing})
        created = call('POST', path + '/operations', {'request_id': uuid.uuid4().hex, 'expected_generation': created['desired']['generation'], 'action': 'detach', 'interface_id': nic['id']}, expected=202); created = wait_instance()
    for _ in range(100):
        if not any(n['publication_id'] == publication['id'] for n in call('GET', '/cvm/networks')['attachments']): break
        time.sleep(.3)
    else: raise RuntimeError('physical NIC cleanup or allocation release is unconfirmed')
    call('DELETE', '/cvm/networks/' + publication['id'], {'request_id': uuid.uuid4().hex, 'expected_revision': 1, 'admitting': False})
    # A terminal Store receipt precedes local cache garbage collection by one
    # worker round. Do not kill the owner before it removes its NAT tables.
    for _ in range(100):
        absent = json.loads(py('import pathlib,json;print(json.dumps(not any(pathlib.Path(' + repr(prefix + '/agent-state/cvm-nics') + ').glob("anchor-*.json"))))'))
        if absent: break
        time.sleep(.3)
    else: raise RuntimeError('released NIC controller has not removed its ownership anchors')
    remote('sudo systemctl stop ' + shlex.quote(network_unit))
    py('''import json,pathlib,subprocess,sys,os,signal
c=json.load(sys.stdin);p=pathlib.Path('/etc/systemd/system/'+c['unit']+'.service');assert p.read_text().startswith('# Brocade isolated CVM acceptance');p.unlink()
subprocess.run(['ip','link','del',c['link']],check=True);subprocess.run(['ip','netns','del',c['namespace']],check=True);subprocess.run(['ip','-6','addr','del',c['public6']+'/128','dev','lo'],check=True)
pid=int(pathlib.Path(c['prefix'],'nic-echo.pid').read_text());cmd=pathlib.Path('/proc',str(pid),'cmdline').read_bytes();assert (c['prefix']+'/nic-echo.py').encode() in cmd;os.kill(pid,signal.SIGTERM)
''', json.dumps(context).encode())
    remote('sudo systemctl daemon-reload')
    record('dependency-checked hot unmount proves Guest device, host TAP, both endpoints and allocation cleanup before deleting the publication')
    return created
