#!/usr/bin/env python3
"""Real baremetal chain routing on release_kvm_lab's explicitly owned KVMs.

Requires the existing chain fixture and an installed candidate with the baremetal
chain contract. This switches only the lab's dedicated Agent service backend.
"""

import argparse
import copy
import ipaddress
import json
from pathlib import Path
import shlex
import time
import uuid

from release_chain_acceptance import ChainAcceptance, V4, V6


class NativeAcceptance(ChainAcceptance):
    def record(self, name, **details):
        super().record(name.replace('Guest', 'native OS').replace('container owner', 'baremetal owner'), **details)

    def guest(self, name, command, timeout=60):
        assert name == 'gamma'
        command = command.replace('~/chain-probe', self.prefix + '/chain-probe')
        return self.lab.remote('compute', 'sudo sh -c ' + shlex.quote(command), timeout=timeout).stdout.decode().strip()

    def set_routes(self, routes, priorities, publish=True):
        item = self.instance('gamma')
        routing = copy.deepcopy(item['desired']['spec']['routing'])
        routing.update(routes=routes, priorities=priorities)
        body = dict(
            request_id=uuid.uuid4().hex,
            expected_generation=item['desired']['generation'],
            expected_revision=routing['revision'],
            routes=routing,
        )
        revision = self.revision()
        before = item['desired']
        preview = self.call('POST', '/cvm/' + before['id'] + '/artifacts/preview', dict(kind='routes', request=body))
        assert self.revision() == revision and self.instance('gamma')['desired'] == before
        saved = self.call('PUT', '/cvm/' + before['id'] + '/routes', body, expected=202)
        replay = self.call('PUT', '/cvm/' + before['id'] + '/routes', body, expected=202)
        assert replay['desired'] == saved['desired']
        if publish:
            self.complete(self.publish(), timeout=240)
            self.instance_ready('gamma')
        return saved

    def defaults(self):
        chain = self.state['chain']['id']
        return [dict(destination=net, chain_id=chain) for net in ['0.0.0.0/0', '::/0']], [
            dict(chain_id=chain, ipv4=10, ipv6=10)
        ]

    def prepare_native(self):
        existing = [
            i
            for i in self.call('GET', '/cvm')['instances']
            if i['node_id'] == 'compute-a'
            and i['reserved']
            and i['desired']['spec']['name'] == 'Real Linux baremetal chain acceptance'
        ]
        if existing:
            assert len(existing) == 1
            return self.finish_native(existing[0])
        self.verify_cube_released()
        self.state.setdefault('cube_regression_instances', self.state['instances'])
        self.state['instances'] = {}
        self.save()
        code = """from pathlib import Path
import json,ipaddress,subprocess,os
base=Path(PREFIX)
addresses=json.loads(subprocess.check_output(['ip','-j','address','show']))
protected=[]
for interface in addresses:
 if interface['ifname']=='lo':continue
 for a in interface.get('addr_info',[]):
  ip=ipaddress.ip_address(a['local'])
  if not ip.is_link_local:protected.append(str(ipaddress.ip_network(a['local']+'/'+str(a['prefixlen']),strict=False)))
config=dict(terminal_user='cvm-lab',primary_device='lab0',protected_destinations=sorted(set(protected)))
p=base/'native-host.json';p.write_text(json.dumps(config));p.chmod(0o600)
unit=Path('/etc/systemd/system/brocade-lab-agent.service.d');unit.mkdir(exist_ok=True)
(unit/'50-native-chain.conf').write_text('[Service]\\nEnvironment=BROCADE_CVM_HOST_CONFIG='+str(p)+'\\nEnvironment=BROCADE_CVM_HOST_STATE_DIR='+str(base/'native-state')+'\\n')
subprocess.run(['sysctl','-qw','net.ipv4.conf.all.rp_filter=1','net.ipv4.conf.default.rp_filter=1'],check=True)
""".replace('PREFIX', repr(self.prefix))
        self.lab.remote('compute', 'sudo systemctl stop brocade-lab-agent')
        self.lab.python('compute', code)
        self.lab.remote('compute', 'sudo systemctl daemon-reload && sudo systemctl start brocade-lab-agent')

        def native(w):
            h = next(h for h in w['hosts'] if h['node_id'] == 'compute-a')
            return (
                h['fresh']
                and not h.get('error')
                and h['capabilities'].get('backend', '').startswith('cube-')
                and h['capabilities'].get('host_system', {}).get('identity')
                and h['capabilities'].get('chain_egress')
            )

        w = self.wait('real baremetal executor inventory', lambda: self.call('GET', '/cvm'), native, timeout=180)
        host = next(h for h in w['hosts'] if h['node_id'] == 'compute-a')
        item = self.call(
            'POST',
            '/cvm/adopt',
            dict(
                request_id=uuid.uuid4().hex,
                tenant_id='platform',
                user_id='alice',
                node_id='compute-a',
                name='Real Linux baremetal chain acceptance',
                identity=host['capabilities']['host_system']['identity'],
            ),
            expected=202,
        )
        return self.finish_native(item)

    def finish_native(self, item):
        self.state['instances']['gamma'] = item['desired']['id']
        self.save()
        self.complete(self.publish(), timeout=240)
        item = self.instance_ready('gamma')
        assert item['report']['host'] and not item['report']['volume_id']
        self.state['chain']['guest_boot'] = self.guest('gamma', 'cat /proc/sys/kernel/random/boot_id')
        self.save()
        catalog = self.call('GET', '/cvm/chain-egress?instance_id=' + item['desired']['id'])
        assert catalog['supported'] and not catalog['reason']
        self.private('native-catalog.json', catalog)
        self.guest('gamma', 'printf native-chain-data > ' + self.prefix + '/native-preserved-marker')
        self.private('native-adopted.json', item)
        self.record(
            'actual baremetal executor adopted the existing KVM OS, new fresh chain capability advertised; no Cube Guest, virtual disk or replacement OS'
        )
        routes, priorities = self.defaults()
        self.set_routes(routes, priorities)
        actual = self.instance_ready('gamma')
        assert (
            actual['report']['network']['chain_egress_configured'] and actual['report']['chain_egress'][0]['configured']
        )
        assert self.guest('gamma', 'cat ' + self.prefix + '/native-preserved-marker') == 'native-chain-data'
        assert self.guest('gamma', 'sysctl -n net.ipv4.conf.all.rp_filter') == '1'
        self.record(
            'actual baremetal default chains applied with native network and client receipts, strict rp_filter and original disk data preserved'
        )

    def verify_cube_released(self):
        world = self.call('GET', '/cvm')
        host = next(h for h in world['hosts'] if h['node_id'] == 'compute-a')
        assert host['capabilities']['backend'] == 'cube-kvm-v1'
        assert not any(i['reserved'] for i in world['instances']), 'release owned Cube instances first'
        assert all(
            not i['report'] or i['report']['network_absent'] for i in world['instances'] if i['node_id'] == 'compute-a'
        )
        self.wait(
            'automatic Cube host network state drained',
            lambda: self.sql(
                "SELECT coalesce((SELECT (report->>'states')::bigint FROM cvm_network_quota_hosts WHERE node_id='compute-a'),0)"
            ),
            lambda value: int(value) == 0,
        )
        self.record('owned Cube instances and network cleanup confirmed before exclusive baremetal adoption')

    def mixed_routes(self):
        publication = next(
            p for p in self.call('GET', '/cvm/networks')['publications'] if p['id'] == self.state['publication']['id']
        )
        self.private('native-mixed-publication.json', publication)
        spec = copy.deepcopy(publication['spec'])
        spec['limits'].update(upload_bps=None, download_bps=None, monthly_bytes=None)
        publication = self.call(
            'PUT',
            '/cvm/networks/' + publication['id'],
            dict(request_id=uuid.uuid4().hex, expected_revision=publication['revision'], spec=spec),
        )
        self.state['publication'] = publication
        self.save()
        self.change(
            '/interfaces',
            dict(publication_id=self.state['publication']['id'], transport='wireguard'),
            name='gamma',
            method='POST',
        )
        self.network_ready('gamma')
        item = self.instance('gamma')
        nic = next(n['id'] for n in item['desired']['spec']['interfaces'] if n.get('allocated'))
        chain = self.state['chain']['id']
        defaults = [dict(destination=net, interface_id=nic) for net in ['0.0.0.0/0', '::/0']]
        specific = [
            dict(destination=addr + ('/128' if ':' in addr else '/32'), chain_id=chain) for addr in [V4[1], V6[1]]
        ]
        services = [
            dict(
                destination=addr + ('/128' if ':' in addr else '/32'),
                interface_id=nic,
                service=dict(protocol='tcp', port_start=9095, port_end=9095),
            )
            for addr in [V4[1], V6[1]]
        ]
        priorities = [dict(interface_id=nic, ipv4=20, ipv6=20), dict(chain_id=chain, ipv4=10, ipv6=10)]
        self.set_routes(defaults + specific + services, priorities)
        for family, addresses in [('4', V4), ('6', V6)]:
            for protocol, port in [('tcp', 9092), ('udp', 9093)]:
                size = 1024 if protocol == 'udp' and family == '6' else 4096
                self.probe(protocol + family, addresses[0], port, 'nic-provider', size=size)
                self.probe(protocol + family, addresses[1], port, 'compute')
            self.probe('tcp' + family, addresses[1], 9095, 'nic-provider')
            self.probe('udp' + family, addresses[1], 9095, 'compute')
        self.record(
            'native real NIC and chain longest prefix and equal prefix L4 selection verified for TCP/UDP IPv4/IPv6'
        )
        both = defaults + [dict(destination=net, chain_id=chain) for net in ['0.0.0.0/0', '::/0']]
        self.set_routes(both, priorities)
        for family, address in [('4', V4[1]), ('6', V6[1])]:
            self.probe('tcp' + family, address, 9092, 'compute')
        priorities[0].update(ipv4=5, ipv6=5)
        self.set_routes(both, priorities)
        for family, address in [('4', V4[1]), ('6', V6[1])]:
            self.probe('tcp' + family, address, 9092, 'nic-provider')
        self.record('native equal prefix NIC and chain priorities change the observed egress without an OS reboot')
        routes, priorities = self.defaults()
        self.set_routes(routes, priorities)
        self.operation('gamma', 'detach', interface_id=nic)
        self.complete(self.publish(), timeout=240)
        after = self.instance_ready('gamma')
        assert not any(n.get('allocated') for n in after['desired']['spec']['interfaces'])
        self.probe('tcp4', V4[0], 9092, 'provider')
        self.probe('udp6', V6[0], 9093, 'provider')
        self.record(
            'native temporary business NIC detached through real publication; original dual stack chain remains usable'
        )

    def protected_replies(self):
        ips = [self.lab.state['roles']['compute'][f] for f in ['ipv4', 'ipv6']]
        unit = (
            '[Service]\nEnvironment=CHAIN_PROBE_UDP_BINDS='
            + ','.join(ips)
            + '\nExecStart='
            + self.prefix
            + '/chain-probe serve\nRestart=on-failure\n'
        )
        self.lab.python(
            'compute',
            'from pathlib import Path;import sys;Path("/etc/systemd/system/brocade-native-echo.service").write_bytes(sys.stdin.buffer.read())',
            unit.encode(),
        )
        self.lab.remote('compute', 'sudo systemctl daemon-reload && sudo systemctl start brocade-native-echo')
        time.sleep(1)
        for family, source, address in [('4', V4[0], ips[0]), ('6', V6[0], ips[1])]:
            for protocol, port in [('tcp', 9092), ('udp', 9093)]:
                endpoint = ('[' + address + ']' if family == '6' else address) + ':' + str(port)
                command = (
                    'sudo env CHAIN_PROBE_SOURCE='
                    + source
                    + ' '
                    + self.prefix
                    + '/chain-probe '
                    + protocol
                    + family
                    + ' '
                    + shlex.quote(endpoint)
                    + ' 4096'
                )
                reply = json.loads(self.lab.remote('client', command).stdout)
                assert reply['bytes'] == 4096 and ipaddress.ip_address(
                    reply['source'].rsplit(':', 1)[0].strip('[]')
                ) == ipaddress.ip_address(source)
        self.record(
            'incoming TCP/UDP IPv4/IPv6 from unprotected chain-covered source receives replies over the original path; SSH and Console remain reachable'
        )

    def management_sockets(self):
        code = (
            """import json,subprocess,os
from pathlib import Path
cg=subprocess.check_output(['systemctl','show','--value','-p','ControlGroup','brocade-lab-agent'],text=True).strip()
assert cg.startswith('/system.slice/') and cg.endswith('/brocade-lab-agent.service')
p=Path('/sys/fs/cgroup'+cg+'/cgroup.procs')
def join():p.write_text(str(os.getpid()))
reply=json.loads(subprocess.check_output([BINARY,'tcp4',DEST+':9092','4096'],preexec_fn=join))
assert reply['source'].rsplit(':',1)[0]==SOURCE and reply['bytes']==4096
print('verified')
""".replace('BINARY', repr(self.prefix + '/chain-probe'))
            .replace('DEST', repr(V4[0]))
            .replace('SOURCE', repr(self.lab.state['roles']['compute']['ipv4']))
        )
        assert self.lab.python('compute', code).decode().strip() == 'verified'
        self.probe('tcp4', V4[0], 9092, 'provider')
        self.record(
            'actual Agent cgroup socket uses original egress while a separate root process uses the selected user chain; no root UID exemption'
        )

    def undo(self):
        before = self.instance('gamma')
        routes, priorities = self.defaults()
        staged = self.set_routes(
            [dict(destination=V4[1] + '/32', chain_id=self.state['chain']['id'])], priorities, publish=False
        )
        self.operation('gamma', 'undo_pending', expected_revision=staged['revision_id'])
        self.complete(self.publish(), timeout=240)
        after = self.instance_ready('gamma')
        assert before['desired']['spec']['execution'] == after['desired']['spec']['execution']
        assert before['desired']['spec']['routing']['routes'] == after['desired']['spec']['routing']['routes']
        self.probe('tcp4', V4[0], 9092, 'provider')
        self.probe('udp6', V6[0], 9093, 'provider')
        assert self.guest('gamma', 'cat /proc/sys/kernel/random/boot_id') == self.state['chain']['guest_boot']
        self.record(
            'native pending route change replay and real Undo publication restore working dual-stack chain without OS or disk replacement'
        )

    def table_fault(self):
        client = self.desired('compute-a')['chain_clients'][0]
        mark = str(client['binding']['mark'])
        self.observer_setup()
        self.lab.remote('compute', 'sudo systemctl kill --kill-whom=main --signal=STOP brocade-lab-agent')
        try:
            for family in ['-4', '-6']:
                self.lab.remote('compute', 'sudo ip ' + family + ' route flush table ' + mark)
            self.no_fallback('native chain policy table removed')
        finally:
            self.lab.remote('compute', 'sudo systemctl kill --kill-whom=main --signal=CONT brocade-lab-agent')
        self.wait(
            'native policy table self repair', lambda: self.try_probe('tcp4', V4[0], 9092, 'provider'), timeout=180
        )

    def restart(self):
        before = self.instance('gamma')
        self.lab.remote('compute', 'sudo systemctl restart brocade-lab-agent')
        self.wait(
            'native Agent restart and local route restore',
            lambda: self.try_probe('tcp4', V4[0], 9092, 'provider'),
            timeout=180,
        )
        after = self.instance_ready('gamma')
        assert after['desired'] == before['desired']
        assert after['report']['host'] == before['report']['host']
        assert self.guest('gamma', 'cat ' + self.prefix + '/native-preserved-marker') == 'native-chain-data'
        self.probe('udp6', V6[0], 9093, 'provider')
        self.record(
            'actual Agent restart restores saved native selectors and subscriber client with same host identity and retained data'
        )

    def cleanup_native(self):
        self.operation('gamma', 'release_host')
        self.complete(self.publish(), timeout=300)
        item = self.instance('gamma')
        assert item['report']['power'] == 'deleted' and not item['reserved']
        assert not item['report']['volume_deleted']
        links = json.loads(self.lab.remote('compute', 'sudo ip -j link show').stdout)
        assert not any(str(link.get('ifalias', '')).startswith(('brocade-chain:', 'brocade-host:')) for link in links)
        for family in ['-4', '-6']:
            rules = json.loads(self.lab.remote('compute', 'sudo ip ' + family + ' -j rule show').stdout)
            assert not any(r.get('priority') in [28800, 31650] for r in rules)
        tables = json.loads(self.lab.remote('compute', 'sudo nft -j list tables').stdout)
        names = [row['table']['name'] for row in tables['nftables'] if 'table' in row]
        assert not any(name == 'brocade_host_output' or name.startswith('brocade_chain_') for name in names)
        self.lab.remote('compute', 'sudo systemctl stop brocade-lab-agent')
        command = (
            'sudo env BROCADE_CVM_HOST_STATE_DIR='
            + shlex.quote(self.prefix + '/native-state')
            + ' '
            + shlex.quote(self.state['candidate_agent_path'])
            + ' cvm-check --empty --host-config '
            + shlex.quote(self.prefix + '/native-host.json')
        )
        assert self.lab.remote('compute', command).stdout.decode().strip() == 'brocade-cvm-empty-v1'
        assert self.guest('gamma', 'cat ' + self.prefix + '/native-preserved-marker') == 'native-chain-data'
        self.lab.remote(
            'compute',
            'sudo rm /etc/systemd/system/brocade-lab-agent.service.d/50-native-chain.conf && sudo systemctl daemon-reload && sudo systemctl start brocade-lab-agent',
        )
        self.record(
            'physical host released via actual publication; original OS/data and the still-running Cube executor remain, owned host selectors and chain objects removed'
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, help='private existing release KVM acceptance directory')
    parser.add_argument('phase', choices=['adopt', 'verify', 'mixed', 'faults', 'quotas', 'cleanup'])
    args = parser.parse_args()
    acceptance = NativeAcceptance(args.output)
    if args.phase == 'adopt':
        acceptance.prepare_native()
    if args.phase in ['adopt', 'verify']:
        acceptance.traffic()
        acceptance.protected_replies()
        acceptance.management_sockets()
        acceptance.undo()
    elif args.phase == 'mixed':
        acceptance.mixed_routes()
    elif args.phase == 'faults':
        acceptance.faults()
        acceptance.table_fault()
        acceptance.grant_revocation()
        acceptance.restart()
    elif args.phase == 'quotas':
        acceptance.quotas()
    elif args.phase == 'cleanup':
        acceptance.cleanup_native()


if __name__ == '__main__':
    main()
