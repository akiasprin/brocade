#!/usr/bin/env python3
"""Real primary-NIC subscriber-chain acceptance in release_kvm_lab's owned Ubuntu VMs."""

import ipaddress
import hashlib
import json
from pathlib import Path
import shlex
import sys
import time
import uuid
from concurrent.futures import ThreadPoolExecutor

from release_kvm_acceptance import Acceptance

V4 = ['11.254.0.30', '11.254.0.31', '11.254.0.32']
V6 = ['2001:4860:ff00::30', '2001:4860:ff00::31', '2001:4860:ff00::32']


class ChainAcceptance(Acceptance):
    def candidate(self, build):
        build = Path(build)
        binaries = {
            'console': build / 'target/brocade-release/cvm/cargo/x86_64-unknown-linux-gnu/release/brocade-console',
            'agent': build / 'target/brocade-agent-build/cvm/x86_64-unknown-linux-musl/release/brocade-agent',
        }
        before = self.guest('gamma', 'cat /proc/sys/kernel/random/boot_id')
        digests = {}
        for role in ['compute', 'provider']:
            self.lab.remote(role, 'sudo systemctl stop brocade-lab-agent')
            # Only this disposable lab contains pre-release checkpoints from the deliberately
            # failed native-guard candidate. Preserve them privately rather than adopting them.
            self.lab.python(
                role,
                'from pathlib import Path;import json\nfor p in Path('
                + repr('/var/lib/bc-' + self.lab.state['task'][:6] + '/cvm-chain-egress')
                + ').glob("*/artifact.json"):\n d=json.loads(p.read_text())\n if not d.get("guard"):p.rename(p.with_name("artifact-before-frozen-guard.json"))',
            )
            for kind in ['agent'] + (['console'] if role == 'compute' else []):
                source = binaries[kind]
                target = self.prefix + '/' + kind + '-chain-76-3'
                self.lab.upload(role, source, target)
                digest = hashlib.sha256(source.read_bytes()).hexdigest()
                assert self.lab.remote(role, 'sudo sha256sum ' + target).stdout.decode().split()[0] == digest
                digests[kind] = digest
                code = (
                    'from pathlib import Path;p=Path('
                    + repr('/etc/systemd/system/brocade-lab-' + kind + '.service')
                    + ');s=p.read_text();lines=[]\nfor l in s.splitlines():\n if l.startswith("ExecStart="):l="ExecStart="+'
                    + repr(target)
                    + '+l[len(l.split()[0]):]\n lines.append(l)\np.write_text("\\n".join(lines)+"\\n")'
                )
                self.lab.python(role, code)
            self.lab.remote(role, 'sudo systemctl daemon-reload')
        self.lab.remote('compute', 'sudo systemctl restart brocade-lab-console')
        self.wait(
            'new Console login',
            lambda: self.lab.remote('compute', 'curl -fsS http://127.0.0.1:8080/healthz', check=False).returncode == 0,
        )
        self.call('POST', '/auth/login', {'operator_id': self.creds['operator_id'], 'password': self.creds['password']})
        current = self.detail(self.state['last_deployment'])
        if current['status'] != 'succeeded':
            self.call('POST', '/deployments/' + str(self.state['last_deployment']) + '/cancel', {}, expected=(200, 202))
        self.call(
            'POST',
            '/apps/app-67ca/chains',
            {
                'id': 'chn-67ca-843d',
                'tenant_id': 'platform',
                'name': 'KVM split chain (frozen guard)',
                'subscription_country': 'US',
            },
            expected=(200, 201),
        )
        deployment = self.publish()
        for role in ['compute', 'provider']:
            self.lab.remote(role, 'sudo systemctl start brocade-lab-agent')
        self.complete(deployment, timeout=240)
        item = self.instance_ready('gamma')
        assert item['report']['chain_egress'][0]['configured']
        assert self.guest('gamma', 'cat /proc/sys/kernel/random/boot_id') == before
        self.state['chain']['guest_boot'] = before
        self.record(
            'corrected frozen native guard candidate applies through a new global revision after explicit failed-order cancellation; original Guest boot preserved',
            binaries=digests,
            deployment=deployment,
        )

    def prepare(self, binary):
        client = self.lab.state['roles']['client']
        ips = V4 + V6 + [client['ipv4'], client['ipv6']]
        digest = hashlib.sha256(binary.read_bytes()).hexdigest()
        for role in self.lab.state['roles']:
            current = self.lab.remote(role, 'sha256sum ' + self.prefix + '/chain-probe', check=False)
            if current.returncode or current.stdout.decode().split()[0] != digest:
                if role == 'client':
                    self.lab.remote(role, 'sudo systemctl stop brocade-chain-probe', check=False)
                self.lab.upload(role, binary, self.prefix + '/chain-probe')
        for role in ['compute', 'provider']:
            code = """import json,subprocess
v4,v6=%r,%r
for family,destination,gateway in [('-4','11.254.0.0/24',v4),('-6','2001:4860:ff00::/64',v6)]:
 route=json.loads(subprocess.check_output(['ip',family,'-j','route','get',gateway]))[0]
 subprocess.run(['ip',family,'route','replace',destination,'via',gateway,'dev',route['dev']],check=True)
from pathlib import Path
p=Path('/etc/hosts');s=p.read_text();line='11.254.0.30 direct.solid.test blocked.solid.test default.solid.test\\n2001:4860:ff00::30 v6.direct.solid.test v6.blocked.solid.test v6.default.solid.test'
if line not in s:p.write_text(s+'\\n'+line+'\\n')
""" % (client['ipv4'], client['ipv6'])
            self.lab.python(role, code)
        self.lab.python(
            'client',
            'import subprocess\nfor ip in '
            + repr(V4 + V6)
            + ":\n subprocess.run(['ip','-6' if ':' in ip else '-4','addr','replace',ip+('/128' if ':' in ip else '/32'),'dev','lo'],check=True)",
        )
        unit = (
            '[Unit]\nAfter=network-online.target\n[Service]\nEnvironment=CHAIN_PROBE_UDP_BINDS='
            + ','.join(ips)
            + '\nExecStart='
            + self.prefix
            + '/chain-probe serve\nRestart=on-failure\n'
        )
        self.lab.python(
            'client',
            'from pathlib import Path;import sys;Path("/etc/systemd/system/brocade-chain-probe.service").write_bytes(sys.stdin.buffer.read())',
            unit.encode(),
        )
        self.lab.remote('client', 'sudo systemctl daemon-reload && sudo systemctl restart brocade-chain-probe')
        for role in ['compute', 'provider']:
            for network, address, port in [
                ('tcp4', V4[0], 9092),
                ('udp4', V4[0], 9093),
                ('tcp6', V6[0], 9092),
                ('udp6', V6[0], 9093),
            ]:
                endpoint = ('[' + address + ']' if ':' in address else address) + ':' + str(port)
                reply = json.loads(
                    self.lab.remote(
                        role, 'sudo ' + self.prefix + '/chain-probe ' + network + ' ' + shlex.quote(endpoint) + ' 4096'
                    ).stdout
                )
                assert reply['bytes'] == 4096
        self.record(
            'independent dual-stack TCP/UDP endpoints and exact-source observers are reachable from both owned KVM hosts'
        )

    def configure(self):
        self.call('POST', '/auth/login', {'operator_id': self.creds['operator_id'], 'password': self.creds['password']})
        self.gate(
            hold_desired=[], hold_machine=[], hold_nics=[], hold_entries=[], hold_cvm_desired=[], corrupt_artifact=[]
        )
        self.call('POST', '/apps', {'id': 'app-67ca', 'label': 'CVM KVM chain acceptance'}, expected=(200, 201))
        app = '/apps/app-67ca'
        self.call(
            'POST',
            app + '/chains',
            {'id': 'chn-67ca-843d', 'tenant_id': 'platform', 'name': 'KVM split chain', 'subscription_country': 'US'},
            expected=(200, 201),
        )

        def rule(match, action):
            return {'m': match, 'a': action}

        egress = {'t': 'egress'}
        rules = [
            rule({'t': 'domain_suffix', 'v': ['blocked.solid.test']}, {'t': 'block'}),
            rule({'t': 'port', 'v': ['9099']}, {'t': 'block'}),
            rule({'t': 'domain_suffix', 'v': ['direct.solid.test']}, egress),
            rule({'t': 'ip_cidr', 'v': [V4[1] + '/32', V6[1] + '/128']}, egress),
            rule({'t': 'port', 'v': ['9095']}, egress),
            rule({'t': 'all', 'v': [{'t': 'network', 'v': 'udp'}, {'t': 'port', 'v': ['9096']}]}, egress),
            rule(
                {'t': 'sniffing_failed'},
                {'t': 'forward', 'to': 'compute-b', 'dial': {'t': 'overlay'}, 'pool': {'t': 'none'}},
            ),
            rule({'t': 'any'}, {'t': 'forward', 'to': 'compute-b', 'dial': {'t': 'overlay'}, 'pool': {'t': 'none'}}),
        ]
        self.call(
            'POST',
            '/model/apply',
            {
                'ops': [
                    {
                        'op': 'put_step',
                        'app_id': 'app-67ca',
                        'chain_id': 'chn-67ca-843d',
                        'node_id': 'compute-b',
                        'step': {
                            'accept': {},
                            'hop_in': {'port': 54303, 'security': {'t': 'encryption'}},
                            'rules': [rule({'t': 'any'}, egress)],
                        },
                    },
                    {
                        'op': 'put_step',
                        'app_id': 'app-67ca',
                        'chain_id': 'chn-67ca-843d',
                        'node_id': 'compute-a',
                        'step': {'rules': rules},
                    },
                ]
            },
        )
        host = self.lab.state['roles']['compute']
        request = {
            'id': 'ing-67ca',
            'chain_id': 'chn-67ca-843d',
            'node_id': 'compute-a',
            'bind': '::',
            'port': 54301,
            'reality': {
                'fallback_mode': 'custom-site',
                'fallback_limits': {'mode': 'off'},
                'fallback_guard': True,
                'dest': 'www.example.com:443',
                'server_names': ['www.example.com'],
            },
            'wires': {'vless_encryption': {'port': 54301}},
            'projection': {
                'vless_encryption': {
                    'v4': {'host': host['ipv4'], 'port': 54301},
                    'v6': {'host': host['ipv6'], 'port': 54301},
                }
            },
            'guard': {
                'no_private': True,
                'no_bittorrent': True,
                'no_mail': True,
                'no_udp_amplification': True,
                'tcp_and_quic_only': False,
                'cn_source_block': 'off',
            },
        }
        self.call('POST', app + '/ingresses', request, expected=(200, 201))
        self.call(
            'POST',
            '/grants',
            {
                'app_id': 'app-67ca',
                'tenant_id': 'platform',
                'user_id': 'alice',
                'ingress_id': 'ing-67ca',
                'enabled': True,
            },
            expected=(200, 201),
        )
        self.state['chain'] = {'app': 'app-67ca', 'id': 'chn-67ca-843d', 'ingress': 'ing-67ca'}
        self.save()
        self.complete(self.publish(), timeout=240)
        self.record(
            'real user-granted VLESS ingress, WireGuard-carried hop and chain-internal domain/IP/protocol/port split configuration published globally'
        )

    def primary_guest(self):
        self.call('POST', '/auth/login', {'operator_id': self.creds['operator_id'], 'password': self.creds['password']})
        if 'gamma' not in self.state['instances']:
            self.create('gamma', 'compute-a')
            self.complete(self.publish(), timeout=240)
        item = self.instance('gamma')
        if not item.get('report'):
            item = self.instance_ready('gamma')
        _, _, address = self.guest_endpoint('gamma')
        words = [
            'ssh',
            '-i',
            self.prefix + '/guest.key',
            '-o',
            'IdentitiesOnly=yes',
            '-o',
            'StrictHostKeyChecking=accept-new',
            '-o',
            'UserKnownHostsFile=' + self.prefix + '/guest.known-' + item['desired']['id'],
            'cube@' + address,
            'cat > ~/chain-probe && chmod 700 ~/chain-probe',
        ]
        self.lab.remote('compute', 'sudo cat ' + self.prefix + '/chain-probe | sudo ' + shlex.join(words))
        boot = self.guest('gamma', 'cat /proc/sys/kernel/random/boot_id')
        allocation_count = self.sql('SELECT count(*) FROM cvm_nic_allocations')
        interfaces = item['desired']['spec']['interfaces']
        assert len(interfaces) == 1 and interfaces[0]['purpose'] == 'contact'
        catalog = self.call('GET', '/cvm/chain-egress?instance_id=' + item['desired']['id'])
        self.private('chain-catalog.json', catalog)
        assert catalog['supported'], catalog.get('reason')
        chain = next(c for c in catalog['chains'] if c['id'] == self.state['chain']['id'])
        assert chain['authorized'] and chain['ipv6'], chain
        routing = json.loads(json.dumps(item['desired']['spec']['routing']))
        routing['routes'] = [{'destination': net, 'chain_id': chain['id']} for net in ['0.0.0.0/0', '::/0']]
        routing['priorities'] = [{'chain_id': chain['id'], 'ipv4': 10, 'ipv6': 10}]
        body = {
            'request_id': uuid.uuid4().hex,
            'expected_generation': item['desired']['generation'],
            'expected_revision': routing['revision'],
            'routes': routing,
        }
        if any(r.get('chain_id') == chain['id'] for r in item['desired']['spec']['routing']['routes']):
            preview = json.loads((self.output / 'chain-primary-preview.json').read_text())
            compiled = preview['artifacts']['current']
            saved = item
        else:
            revision = self.revision()
            preview = self.call(
                'POST', '/cvm/' + item['desired']['id'] + '/artifacts/preview', {'kind': 'routes', 'request': body}
            )
            self.private('chain-primary-preview.json', preview)
            assert not preview['no_op'] and self.revision() == revision
            assert self.instance('gamma')['desired']['generation'] == item['desired']['generation']
            compiled = preview['artifacts']['current']
            saved = self.call(
                'PUT',
                '/cvm/' + item['desired']['id'] + '/routes',
                body,
                expected=202,
                extra_headers={'x-brocade-cvm-input-sha256': compiled['input_sha256']},
            )
        assert saved['desired']['spec']['interfaces'] == interfaces
        assert self.sql('SELECT count(*) FROM cvm_nic_allocations') == allocation_count
        last = self.detail(self.state['last_deployment'])
        deployment = self.state['last_deployment'] if last['revision_id'] == self.revision() else self.publish()
        self.complete(deployment, timeout=240)
        ready = self.instance_ready('gamma')
        actual = self.call('GET', '/cvm/' + item['desired']['id'] + '/artifacts')
        assert actual['current'] == compiled
        assert self.guest('gamma', 'cat /proc/sys/kernel/random/boot_id') == boot
        self.state['chain']['guest_boot'] = boot
        self.state['chain']['allocations'] = allocation_count
        self.private('chain-primary-applied.json', ready)
        self.record(
            'primary-only Guest selects dual-stack chain exit with zero NIC/IP allocation and no reboot; rollback-only preview bytes equal saved and published artifacts'
        )

    def probe(self, protocol, address, port, expected_role, size=4096, host=None, name='gamma'):
        endpoint = ('[' + address + ']' if ':' in address else address) + ':' + str(port)
        args = [protocol, endpoint, host, str(size)] if host else [protocol, endpoint, str(size)]
        reply = json.loads(self.guest(name, '~/chain-probe ' + shlex.join(args)))
        source = reply['source'].rsplit(':', 1)[0].strip('[]')
        family = 'ipv6' if ':' in address else 'ipv4'
        role = 'provider' if expected_role == 'nic-provider' else expected_role
        expected = self.lab.state['roles'][role][family]
        if expected_role == 'nic-provider' and family == 'ipv4':
            entries = self.call('GET', '/cvm/' + self.state['instances'][name] + '/entries')
            whole = [e['address'] for e in entries if e['mode']['kind'] == 'l3' and e['state'] == 'active']
            if whole:
                assert len(whole) == 1
                expected = whole[0]
        assert ipaddress.ip_address(source) == ipaddress.ip_address(expected), (args, reply, expected)
        assert reply['bytes'] == size
        return reply

    def try_probe(self, *args, **kwargs):
        try:
            return self.probe(*args, **kwargs)
        except RuntimeError:
            return None

    def blocked_probe(self, protocol, address, port, host=None, name='gamma'):
        endpoint = ('[' + address + ']' if ':' in address else address) + ':' + str(port)
        args = [protocol, endpoint, host, '4096'] if host else [protocol, endpoint, '4096']
        result = self.guest(
            name,
            'test -x ~/chain-probe || exit 127; timeout 4s ~/chain-probe '
            + shlex.join(args)
            + ' >/tmp/blocked-probe.out 2>/tmp/blocked-probe.err; echo $?',
        )
        assert result in ('1', '124'), (protocol, address, port, host, result)

    def traffic(self):
        self.instance_ready('gamma')
        for addresses, family in [(V4, '4'), (V6, '6')]:
            for protocol, port in [('tcp', 9092), ('udp', 9093)]:
                result = self.probe(protocol + family, addresses[0], port, 'provider')
                self.record(
                    'primary-only chain ' + protocol.upper() + '/IPv' + family + ' reaches actual remote landing',
                    observed=result,
                )
                result = self.probe(protocol + family, addresses[1], port, 'compute')
                self.record(
                    'chain-internal IP split ' + protocol.upper() + '/IPv' + family + ' exits at actual ingress',
                    observed=result,
                )
                self.probe(protocol + family, addresses[0], 9095, 'compute')
                self.blocked_probe(protocol + family, addresses[0], 9099)
            self.probe('udp' + family, addresses[0], 9096, 'compute')
            self.probe('tcp' + family, addresses[0], 9096, 'provider')
            self.probe(
                'http' + family,
                addresses[0],
                8088,
                'compute',
                host=('v6.' if family == '6' else '') + 'direct.solid.test',
            )
            self.probe(
                'http' + family,
                addresses[0],
                8088,
                'provider',
                host=('v6.' if family == '6' else '') + 'default.solid.test',
            )
            self.blocked_probe(
                'http' + family, addresses[0], 8088, host=('v6.' if family == '6' else '') + 'blocked.solid.test'
            )
            self.record(
                'chain internal domain, port, protocol split and explicit block IPv'
                + family
                + ' verified against independent destination observations'
            )
        self.record(
            'all original destinations preserved across primary TUN, authorized subscriber ingress and ordinary WireGuard next hop'
        )

    def reviewed_routes(self, name, routes, priorities, label):
        item = self.instance(name)
        before_boot = self.guest(name, 'cat /proc/sys/kernel/random/boot_id')
        routing = json.loads(json.dumps(item['desired']['spec']['routing']))
        routing.update(routes=routes, priorities=priorities)
        body = {
            'request_id': uuid.uuid4().hex,
            'expected_generation': item['desired']['generation'],
            'expected_revision': routing['revision'],
            'routes': routing,
        }
        revision = self.revision()
        count = self.sql('SELECT count(*) FROM cvm_nic_allocations')
        preview = self.call(
            'POST', '/cvm/' + item['desired']['id'] + '/artifacts/preview', {'kind': 'routes', 'request': body}
        )
        assert self.revision() == revision
        assert self.instance(name)['desired'] == item['desired']
        compiled = preview['artifacts']['current']
        self.call(
            'PUT',
            '/cvm/' + item['desired']['id'] + '/routes',
            body,
            expected=409,
            extra_headers={'x-brocade-cvm-input-sha256': '0' * 64},
        )
        assert self.revision() == revision and self.instance(name)['desired'] == item['desired']
        saved = self.call(
            'PUT',
            '/cvm/' + item['desired']['id'] + '/routes',
            body,
            expected=202,
            extra_headers={'x-brocade-cvm-input-sha256': compiled['input_sha256']},
        )
        assert self.sql('SELECT count(*) FROM cvm_nic_allocations') == count
        assert saved['desired']['spec']['interfaces'] == item['desired']['spec']['interfaces']
        if preview['no_op']:
            assert saved['desired'] == item['desired'] and self.revision() == revision
        else:
            self.complete(self.publish(), timeout=240)
            self.instance_ready(name)
        actual = self.call('GET', '/cvm/' + item['desired']['id'] + '/artifacts')
        assert actual['current'] == compiled
        assert self.guest(name, 'cat /proc/sys/kernel/random/boot_id') == before_boot
        self.private('chain-reviewed-' + label + '.json', {'preview': preview, 'saved': saved})
        self.record(
            'real reviewed route '
            + label
            + ': stale digest rejected, preview rolled back, exact saved/published bytes and Guest boot preserved',
            no_op=preview['no_op'],
        )
        return saved

    def refresh(self):
        item = self.instance('gamma')
        chain = self.state['chain']['id']
        self.reviewed_routes(
            'gamma',
            [{'destination': net, 'chain_id': chain} for net in ['0.0.0.0/0', '::/0']],
            [{'chain_id': chain, 'ipv4': 11, 'ipv6': 11}],
            'fresh-final-client',
        )
        self.state['chain']['guest_boot'] = self.guest('gamma', 'cat /proc/sys/kernel/random/boot_id')
        self.save()

    def mixed_routes(self):
        # The independent observer distinguishes ingress H from NIC provider P; matching
        # the destination alone cannot produce a false pass from an equivalent egress.
        # The NIC's return path crosses WG/1400. Keep route-selection datagrams
        # below that PMTU; nic_ipv6_pmtu separately verifies actual PTB discovery
        # and 4096-byte retransmission, rather than hiding a cold loss with retries.
        chain = self.state['chain']['id']
        nic = self.state['nic']['id']
        self.lab.remote(
            'compute',
            'sudo cat '
            + self.prefix
            + '/chain-probe | sudo '
            + shlex.join(
                [
                    'ssh',
                    '-i',
                    self.prefix + '/guest.key',
                    '-o',
                    'IdentitiesOnly=yes',
                    '-o',
                    'StrictHostKeyChecking=accept-new',
                    '-o',
                    'UserKnownHostsFile=' + self.prefix + '/guest.known-' + self.state['instances']['alpha'],
                    'cube@' + self.guest_endpoint('alpha')[2],
                    'cat > ~/chain-probe && chmod 700 ~/chain-probe',
                ]
            ),
        )
        defaults = [{'destination': net, 'interface_id': nic} for net in ['0.0.0.0/0', '::/0']]
        specific = [
            {'destination': address + ('/128' if ':' in address else '/32'), 'chain_id': chain}
            for address in [V4[1], V6[1]]
        ]
        port_specific = [
            {
                'destination': address + ('/128' if ':' in address else '/32'),
                'interface_id': nic,
                'service': {'protocol': 'tcp', 'port_start': 9095, 'port_end': 9095},
            }
            for address in [V4[1], V6[1]]
        ]
        priorities = [{'interface_id': nic, 'ipv4': 20, 'ipv6': 20}, {'chain_id': chain, 'ipv4': 10, 'ipv6': 10}]
        self.reviewed_routes('alpha', defaults + specific + port_specific, priorities, 'mixed-l3-l4')
        for family, addresses in [('4', V4), ('6', V6)]:
            for protocol, port in [('tcp', 9092), ('udp', 9093)]:
                size = 1024 if protocol == 'udp' and family == '6' else 4096
                self.probe(protocol + family, addresses[0], port, 'nic-provider', name='alpha', size=size)
                self.probe(protocol + family, addresses[1], port, 'compute', name='alpha')
            self.probe('tcp' + family, addresses[1], 9095, 'nic-provider', name='alpha')
            self.probe('udp' + family, addresses[1], 9095, 'compute', name='alpha')
        self.record('mixed real NIC/chain longest-prefix and equal-prefix L4 precedence verified for TCP/UDP IPv4/IPv6')
        both = defaults + [{'destination': net, 'chain_id': chain} for net in ['0.0.0.0/0', '::/0']]
        self.reviewed_routes('alpha', both, priorities, 'chain-priority')
        for family, address in [('4', V4[1]), ('6', V6[1])]:
            self.probe('tcp' + family, address, 9092, 'compute', name='alpha')
            self.probe('udp' + family, address, 9093, 'compute', name='alpha')
        priorities[0].update(ipv4=5, ipv6=5)
        self.reviewed_routes('alpha', both, priorities, 'nic-priority')
        for family, address in [('4', V4[1]), ('6', V6[1])]:
            self.probe('tcp' + family, address, 9092, 'nic-provider', name='alpha')
            self.probe(
                'udp' + family, address, 9093, 'nic-provider', name='alpha', size=1024 if family == '6' else 4096
            )
        self.record('equal-prefix mixed NIC/chain per-family exit priority changes actual egress without Guest reboot')
        self.reviewed_routes('alpha', defaults, [priorities[0]], 'remove-chain')
        assert not self.instance('alpha')['report'].get('chain_egress')
        self.reviewed_routes('alpha', defaults, [priorities[0]], 'unchanged-noop')

    def nic_ipv6_pmtu(self):
        peer = self.lab.state['roles']['provider']['ipv6']
        with ThreadPoolExecutor(max_workers=1) as pool:
            capture = pool.submit(
                self.lab.remote,
                'client',
                'sudo timeout 24 tcpdump -n -tt -i lab0 -l '
                + shlex.quote('icmp6 or (ip6 and host ' + peer + ' and host ' + V6[0] + ')'),
                timeout=30,
                check=False,
            )
            time.sleep(1)
            self.lab.remote('client', 'sudo ip -6 route flush cache')
            before = self.lab.remote('client', 'ip -6 route get ' + peer).stdout.decode()
            cold = self.try_probe('udp6', V6[0], 9093, 'nic-provider', size=4096, name='alpha')
            after = self.lab.remote('client', 'ip -6 route get ' + peer).stdout.decode()
            warm = self.probe('udp6', V6[0], 9093, 'nic-provider', size=4096, name='alpha')
            result = capture.result()
        packet_log = result.stdout.decode()
        assert 'packet too big, mtu 1400' in packet_log and 'mtu 1400' in after
        self.private(
            'nic-ipv6-pmtu.json', {'cold_reply': cold is not None, 'before': before, 'after': after, 'warm': warm}
        )
        (self.output / 'nic-ipv6-pmtu.log').write_bytes(result.stdout + result.stderr)
        self.record(
            'ordinary NIC IPv6 cold UDP discovers real WG/1400 PMTU through ICMPv6 PTB; an explicit subsequent 4096-byte datagram returns intact',
            cold_reply=cold is not None,
            warm_bytes=warm['bytes'],
        )

    def observer_setup(self):
        nft = """table inet brocade_accept_chain {
 counter requests { }
 chain observe {
 type filter hook input priority -290; policy accept;
 ip daddr 11.254.0.30 tcp dport 9092 tcp flags & (syn | ack) == syn counter name requests
 ip daddr 11.254.0.30 udp dport 9093 counter name requests
 ip6 daddr 2001:4860:ff00::30 tcp dport 9092 tcp flags & (syn | ack) == syn counter name requests
 ip6 daddr 2001:4860:ff00::30 udp dport 9093 counter name requests
 }
}
"""
        self.lab.remote('client', 'sudo nft delete table inet brocade_accept_chain', check=False)
        self.lab.remote('client', 'sudo nft -f -', data=nft.encode())

    def observed_packets(self):
        data = json.loads(
            self.lab.remote('client', 'sudo nft -j list counter inet brocade_accept_chain requests').stdout
        )
        return next(i['counter']['packets'] for i in data['nftables'] if 'counter' in i)

    def no_fallback(self, label):
        before = self.observed_packets()
        for family, address in [('4', V4[0]), ('6', V6[0])]:
            self.blocked_probe('tcp' + family, address, 9092)
            self.blocked_probe('udp' + family, address, 9093)
        after = self.observed_packets()
        assert after == before, (label, before, after)
        assert self.guest('gamma', 'cat /proc/sys/kernel/random/boot_id') == self.state['chain']['guest_boot']
        self.record(
            label
            + ': selected TCP/UDP IPv4/IPv6 stays unreachable, independent destination observes zero packets; primary SSH and Guest boot preserved',
            observed_packets=after - before,
        )

    def faults(self):
        self.observer_setup()
        self.probe('tcp4', V4[0], 9092, 'provider')
        assert self.observed_packets() > 0
        # Hold only this owned lab Agent to inspect the interval before its normal repair.
        self.lab.remote('compute', 'sudo systemctl kill --kill-whom=main --signal=STOP brocade-lab-agent')
        try:
            code = 'import os,signal;from pathlib import Path\nfound=[]\nfor p in Path("/proc").iterdir():\n if not p.name.isdigit():continue\n try:args=(p/"cmdline").read_bytes().split(b"\\0")\n except OSError:continue\n if len(args)>3 and args[0].endswith(b"/xray") and any(b"/cvm-chain-egress/" in a for a in args):found.append(int(p.name))\nassert len(found)==1\nos.kill(found[0],signal.SIGKILL)'
            self.lab.python('compute', code)
            self.no_fallback('managed client killed')
        finally:
            self.lab.remote('compute', 'sudo systemctl kill --kill-whom=main --signal=CONT brocade-lab-agent')
        self.wait('managed client repair', lambda: self.try_probe('tcp4', V4[0], 9092, 'provider'), timeout=180)
        self.record('Agent restarts killed subscriber client and traffic recovers without a Guest reboot')
        fault = (
            'table inet brocade_accept_chain_fault { chain output { type filter hook output priority -290; policy accept; ip daddr '
            + self.lab.state['roles']['compute']['ipv4']
            + ' tcp dport 54301 reject\n }\n}\n'
        )
        self.lab.remote('compute', 'sudo nft -f -', data=fault.encode())
        try:
            self.no_fallback('subscriber ingress unreachable')
        finally:
            self.lab.remote('compute', 'sudo nft delete table inet brocade_accept_chain_fault')
        self.wait('upstream restore', lambda: self.try_probe('tcp4', V4[0], 9092, 'provider'), timeout=180)
        self.probe('udp6', V6[0], 9093, 'provider')
        self.record('restored ingress returns authorized dual-stack traffic through original chain')

    def traffic_view(self):
        return next(
            v
            for v in self.call('GET', '/users/platform/alice/traffic-policy')
            if v['app_id'] == self.state['chain']['app']
        )

    def traffic_policy(self, policy):
        self.call('PUT', '/users/platform/alice/traffic-policy/access/' + self.state['chain']['app'], policy)
        return self.wait(
            'actual user traffic policy receipt',
            self.traffic_view,
            lambda v: not v['pending_nodes'] and not v['unsupported_nodes'],
        )

    def quotas(self):
        original = self.traffic_view()['policy']
        unlimited = {'quota_bytes': None, 'base_rate_bps': None, 'exhausted_action': {'kind': 'suspend'}}
        self.private('chain-original-user-policy.json', original)
        try:
            start_used = int(self.traffic_view()['used_bytes'])
            baseline = time.monotonic()
            for _ in range(3):
                self.probe('tcp4', V4[0], 9092, 'provider', size=32000)
            normal = time.monotonic() - baseline
            self.wait(
                'actual subscriber usage enters owner/app ledger',
                self.traffic_view,
                lambda v: int(v['used_bytes']) > start_used,
            )
            self.record(
                'primary-chain traffic uses the existing owner/app access ledger',
                used_before=start_used,
                used_after=int(self.traffic_view()['used_bytes']),
            )
            self.traffic_policy({**unlimited, 'base_rate_bps': 128000})
            started = time.monotonic()
            for _ in range(3):
                self.probe('tcp4', V4[0], 9092, 'provider', size=32000)
            limited = time.monotonic() - started
            assert limited >= 3.5 and limited > normal * 2, (normal, limited)
            self.record(
                'existing subscriber aggregate speed cap throttles actual bidirectional primary-chain payload',
                baseline_seconds=normal,
                limited_seconds=limited,
                rate_bps=128000,
            )
            self.traffic_policy(unlimited)
            used = int(self.traffic_view()['used_bytes'])
            limit = used + 512 * 1024
            self.traffic_policy({**unlimited, 'quota_bytes': str(limit)})
            for _ in range(24):
                self.try_probe('tcp4', V4[0], 9092, 'provider', size=32000)
                if self.traffic_view()['exhausted']:
                    break
            exhausted = self.wait(
                'actual chain usage exhausts existing app quota',
                self.traffic_view,
                lambda v: v['exhausted'] and v['effective']['mode'] == 'suspended',
            )
            assert int(exhausted['used_bytes']) >= limit
            self.wait(
                'quota suspension reaches actual ingress',
                lambda: self.try_probe('tcp4', V4[0], 9092, 'provider'),
                lambda reply: reply is None,
                timeout=90,
            )
            self.no_fallback('existing owner/app quota exhausted')
            self.record(
                'same user quota suspends chain traffic after real metered payload',
                quota_bytes=limit,
                used_bytes=int(exhausted['used_bytes']),
            )
        finally:
            self.traffic_policy(original or unlimited)
        self.wait('quota restored', lambda: self.try_probe('tcp4', V4[0], 9092, 'provider'))
        self.probe('udp6', V6[0], 9093, 'provider')
        self.record('restoring existing user policy restores the same chain and Guest identity')

    def grant_revocation(self):
        body = {
            'app_id': self.state['chain']['app'],
            'tenant_id': 'platform',
            'user_id': 'alice',
            'ingress_id': self.state['chain']['ingress'],
            'enabled': False,
        }
        self.call('POST', '/grants', body, expected=(200, 201))
        self.complete(self.publish(), timeout=240)
        catalog = self.call('GET', '/cvm/chain-egress?instance_id=' + self.state['instances']['gamma'])
        current = next(c for c in catalog['chains'] if c['id'] == self.state['chain']['id'])
        assert not current['authorized'] and self.state['instances']['gamma'] in current['used_by']
        desired = self.desired('compute-a')
        client = next(c for c in desired['chain_clients'] if c['instance_id'] == self.state['instances']['gamma'])
        assert client['blocked']
        self.no_fallback('container owner chain grant revoked')
        body['enabled'] = True
        self.call('POST', '/grants', body, expected=(200, 201))
        self.complete(self.publish(), timeout=240)
        self.wait('grant restored', lambda: self.try_probe('tcp4', V4[0], 9092, 'provider'))
        self.probe('udp6', V6[0], 9093, 'provider')
        self.record('restoring owner grant restores the existing chain binding without extra NIC/IP or Guest reboot')

    def reviewed_mutation(self, name, kind, request, endpoint, label):
        before = self.instance(name)
        revision = self.revision()
        allocations = self.sql('SELECT count(*) FROM cvm_nic_allocations')
        preview = self.call(
            'POST', '/cvm/' + before['desired']['id'] + '/artifacts/preview', {'kind': kind, 'request': request}
        )
        assert self.revision() == revision and self.instance(name)['desired'] == before['desired']
        assert self.sql('SELECT count(*) FROM cvm_nic_allocations') == allocations
        compiled = preview['artifacts']['current']
        saved = self.call(
            'POST',
            '/cvm/' + before['desired']['id'] + endpoint,
            request,
            expected=202,
            extra_headers={'x-brocade-cvm-input-sha256': compiled['input_sha256']},
        )
        if preview['no_op']:
            assert self.revision() == revision and saved['desired'] == before['desired']
        else:
            hold_detach = kind == 'command' and request.get('action') == 'detach'
            if hold_detach:
                self.gate(hold_cvm_desired=['compute-a'])
            deployment = self.publish()
            if hold_detach:
                try:
                    self.wait(
                        'pending physical NIC withdrawal',
                        lambda: self.call('GET', '/cvm/' + before['desired']['id'] + '/network'),
                        lambda n: any(b['phase'] == 'withdrawing' for b in n['bindings']),
                    )
                    start = time.monotonic()
                    while time.monotonic() - start < 18:
                        assert self.detail(deployment)['status'] not in ('failed', 'succeeded')
                        observed = json.loads(self.guest(name, 'ip -j address'))
                        wanted = next(
                            n['allocated']['mac']
                            for n in before['desired']['spec']['interfaces']
                            if n['id'] == request['interface_id']
                        )
                        assert any(n.get('address') == wanted for n in observed)
                        time.sleep(1)
                    self.record(
                        'withdrawal remains pending while actual Guest NIC is still attached; it neither fails the release nor frees the physical lease'
                    )
                finally:
                    self.gate(hold_cvm_desired=[])
            self.complete(deployment, timeout=240)
            self.instance_ready(name)
        actual = self.call('GET', '/cvm/' + before['desired']['id'] + '/artifacts')
        assert actual['current'] == compiled
        self.private('reviewed-' + label + '.json', preview)
        self.record(
            'real '
            + label
            + ' preview/save/publish shares exact immutable artifacts; preview leaves no revision, generation or allocation',
            no_op=preview['no_op'],
        )
        return saved

    def preview_lifecycle(self):
        # The owned fixture's four-vCPU user quota is real. Release the completed
        # two-vCPU Beta lifecycle fixture before reserving another sandbox.
        beta = self.instance('beta')
        if beta['reserved']:
            self.operation('beta', 'delete', delete_data=True)
            self.complete(self.publish(), timeout=240)
            self.record(
                'completed Beta lifecycle Guest physically deleted through global publication before reusing its quota'
            )
        if 'delta' not in self.state['instances']:
            self.create('delta', 'compute-a')
            self.complete(self.publish(), timeout=240)
        item = self.instance_ready('delta')
        body = {
            'request_id': uuid.uuid4().hex,
            'expected_generation': item['desired']['generation'],
            'publication_id': self.state['publication']['id'],
            'transport': 'wireguard',
            'use_dns_suggestions': False,
        }
        mounted = self.reviewed_mutation('delta', 'mount', body, '/interfaces', 'mount')
        allocated = [n for n in mounted['desired']['spec']['interfaces'] if n.get('allocated')]
        assert len(allocated) == 1
        item, role, address = self.guest_endpoint('delta')
        words = [
            'ssh',
            '-i',
            self.prefix + '/guest.key',
            '-o',
            'IdentitiesOnly=yes',
            '-o',
            'StrictHostKeyChecking=accept-new',
            '-o',
            'UserKnownHostsFile=' + self.prefix + '/guest.known-' + item['desired']['id'],
            'cube@' + address,
            'cat > ~/chain-probe && chmod 700 ~/chain-probe',
        ]
        self.lab.remote(role, 'sudo cat ' + self.prefix + '/chain-probe | sudo ' + shlex.join(words))
        self.probe('tcp4', V4[0], 9092, 'nic-provider', name='delta')
        self.probe('udp6', V6[0], 9093, 'nic-provider', name='delta')
        self.record('reviewed NIC mount has actual independent dual-stack TCP/UDP data delivery')
        self.reviewed_routes('delta', [], [], 'before-detach')
        item = self.instance('delta')
        body = {
            'request_id': uuid.uuid4().hex,
            'expected_generation': item['desired']['generation'],
            'action': 'detach',
            'interface_id': allocated[0]['id'],
        }
        self.reviewed_mutation('delta', 'command', body, '/operations', 'detach')
        self.operation('delta', 'stop')
        self.complete(self.publish(), timeout=240)
        item = self.instance_ready('delta')
        body = {
            'request_id': uuid.uuid4().hex,
            'expected_generation': item['desired']['generation'],
            'action': 'configure',
            'name': 'delta-reviewed',
            'vcpus': 2,
            'memory_bytes': 384 * 1024**2,
        }
        self.reviewed_mutation('delta', 'command', body, '/operations', 'configure-stopped')
        self.operation('delta', 'start')
        self.complete(self.publish(), timeout=240)
        self.finish_preview_lifecycle()

    def finish_preview_lifecycle(self):
        actual = self.instance_ready('delta')
        role = self.state['nodes'][actual['node_id']]['role']
        code = """from pathlib import Path
import json,socket,http.client
record=json.loads(Path('/data/cubelet/network-attachments-v2/'+%r+'.json').read_text())
path='/run/vc/vm/'+record['runtime']+'/chapi'
socket_=socket.socket(socket.AF_UNIX);socket_.settimeout(5);socket_.connect(path)
client=http.client.HTTPConnection('localhost',timeout=5);client.sock=socket_
client.request('GET','/api/v1/vm.info');response=client.getresponse()
value=json.loads(response.read());assert response.status==200
print(json.dumps({'state':value['state'],'vcpus':value['config']['cpus']['boot_vcpus'],'memory_bytes':value['config']['memory']['size']}))
""" % actual['desired']['id']
        capacity = json.loads(self.lab.python(role, code))
        assert capacity == {'state': 'Running', 'vcpus': 2, 'memory_bytes': 384 * 1024**2}
        assert int(self.guest('delta', 'getconf _NPROCESSORS_ONLN')) == 2
        self.record(
            'reviewed stopped sandbox change becomes actual two-vCPU 384MiB Guest after controlled start',
            live_vmm=capacity,
        )
        # The configure editor is deliberately available only after a shutdown,
        # including preview. Exercise its no-op in that supported state.
        self.operation('delta', 'stop')
        self.complete(self.publish(), timeout=240)
        self.instance_ready('delta')
        item = self.instance('delta')
        body = {
            'request_id': uuid.uuid4().hex,
            'expected_generation': item['desired']['generation'],
            'action': 'configure',
            'name': item['desired']['spec']['name'],
            'vcpus': 2,
            'memory_bytes': 384 * 1024**2,
        }
        self.reviewed_mutation('delta', 'command', body, '/operations', 'configure-noop')
        self.operation('delta', 'delete', delete_data=True)
        self.complete(self.publish(), timeout=240)
        self.record('temporary reviewed-preview Guest deleted through normal global publication')

    def ingress_guards(self):
        client = self.lab.state['roles']['client']
        cases = [
            ('tcp4', V4[0], 25),
            ('tcp6', V6[0], 25),
            ('udp4', V4[0], 123),
            ('udp6', V6[0], 123),
            ('tcp4', client['ipv4'], 9092),
            ('udp6', client['ipv6'], 9093),
        ]
        for protocol, address, port in cases:
            endpoint = ('[' + address + ']' if ':' in address else address) + ':' + str(port)
            control = json.loads(
                self.lab.remote(
                    'compute', 'sudo ' + self.prefix + '/chain-probe ' + protocol + ' ' + shlex.quote(endpoint) + ' 256'
                ).stdout
            )
            assert control['bytes'] == 256
            result = self.guest(
                'gamma',
                'timeout 4s ~/chain-probe '
                + protocol
                + ' '
                + shlex.quote(endpoint)
                + ' 256 >/tmp/guard.out 2>/tmp/guard.err; echo $?',
            )
            assert result in ('1', '124'), (protocol, address, port, result)
        self.probe('tcp4', V4[0], 9092, 'provider')
        self.probe('udp6', V6[0], 9093, 'provider')
        # The existing ingress policy deliberately allows DNS; UDP amplification
        # refers to its fixed NTP/SNMP/etc. port list, not all UDP or port 53.
        self.probe('udp4', V4[0], 53, 'provider', size=256)
        self.probe('udp6', V6[0], 53, 'provider', size=256)
        self.record(
            'ordinary chain ingress guards block mail, UDP amplification and private destinations for primary-only container traffic; independent endpoint controls and permitted traffic work'
        )

    def chain_icmp(self):
        runtime = (
            self.lab.python(
                'compute',
                'import json;from pathlib import Path;print(json.loads(Path('
                + repr('/data/cubelet/network-attachments-v2/' + self.state['instances']['gamma'] + '.json')
                + ').read_text())["runtime"])',
            )
            .decode()
            .strip()
        )

        def guest_command(args):
            words = [
                self.prefix + '/attachment-probe',
                'guest',
                runtime,
                '/data/cubelet/network-attachments-v2',
                *args,
            ]
            return self.lab.remote('compute', 'sudo ' + shlex.join(words), check=False)

        def guest_echo(family, address):
            return guest_command(['/home/cube/chain-probe', 'icmp' + family, address])

        def ping_group_range(value):
            result = guest_command(
                [
                    '/bin/sh',
                    '-c',
                    'set -e; printf "%s\\n" "$1" > /proc/sys/net/ipv4/ping_group_range; printf "{}"',
                    'owned-icmp',
                    value,
                ]
            )
            assert result.returncode == 0, result.stderr.decode()

        rules = (
            'table inet brocade_accept_chain_icmp { chain observe { type filter hook input priority -291; policy accept; ip daddr '
            + V4[0]
            + ' icmp type echo-request counter\n ip6 daddr '
            + V6[0]
            + ' icmpv6 type echo-request counter\n }\n}\n'
        )
        self.lab.remote('client', 'sudo nft -f -', data=rules.encode())

        def count():
            value = json.loads(
                self.lab.remote('client', 'sudo nft -j list table inet brocade_accept_chain_icmp').stdout
            )
            return sum(
                expr['counter']['packets']
                for item in value['nftables']
                if 'rule' in item
                for expr in item['rule']['expr']
                if 'counter' in expr
            )

        original = self.guest('gamma', 'cat /proc/sys/net/ipv4/ping_group_range')
        try:
            # This minimal test image disables every ping datagram group (1,0).
            # Permit root only inside this owned Guest and restore its value below.
            ping_group_range('0 0')
            for family, loopback in [('4', '127.0.0.1'), ('6', '::1')]:
                result = guest_echo(family, loopback)
                assert result.returncode == 0, result.stderr.decode()
            for family, address in [('4', V4[0]), ('6', V6[0])]:
                self.lab.remote('compute', 'ping -' + family + ' -c 1 -W 2 ' + address)
            before = count()
            assert before >= 2
            for family, address in [('4', V4[0]), ('6', V6[0])]:
                result = guest_echo(family, address)
                assert result.returncode == 1 and b'Guest command exited with status 1' in result.stderr, (
                    result.stderr.decode()
                )
            assert count() == before
            self.record(
                'selected chain does not claim ICMP echo support or fall back to host NAT; real dual-stack destinations observe zero Guest echo requests while direct controls work'
            )
        finally:
            ping_group_range(original)
            self.lab.remote('client', 'sudo nft delete table inet brocade_accept_chain_icmp')

    def entry_existing(self):
        provider = self.lab.state['roles']['provider']['ipv4']
        whole = '198.51.100.211'
        self.lab.remote('client', 'sudo ip route replace ' + whole + '/32 via ' + provider + ' dev lab0')
        self.lab.upload('client', self.output / 'guest.key', self.prefix + '/guest.key', 0o600)
        payload = b'entry-full-duplex-' * 16384
        for address, port in [(provider, 55080), (whole, 22)]:
            words = [
                'ssh',
                '-i',
                self.prefix + '/guest.key',
                '-o',
                'IdentitiesOnly=yes',
                '-o',
                'StrictHostKeyChecking=accept-new',
                '-o',
                'UserKnownHostsFile=' + self.prefix + '/entry.known',
                '-o',
                'ConnectTimeout=5',
                '-p',
                str(port),
                'cube@' + address,
                'cat',
            ]
            result = self.lab.remote('client', 'sudo ' + shlex.join(words), data=payload, timeout=60)
            assert result.stdout == payload
        self.guest('alpha', 'nohup ~/probe udp-server 0.0.0.0:18081 >~/entry.log 2>&1 </dev/null &')
        for endpoint in [provider + ':55081', whole + ':18081']:
            result = self.lab.remote(
                'client', 'sudo ' + self.prefix + '/probe udp4 ' + endpoint + ' ' + ('u' * 4096), timeout=30
            )
            assert result.stdout == b'u' * 4096
        self.lab.remote('client', 'ping -c 2 -W 3 ' + whole)
        self.record(
            'real cross-host WG entries carry L4/whole-IP SSH full-duplex 288KiB, fragmented UDP and whole-IP ICMP; original primary reply isolation preserved'
        )

    def period(self):
        original = self.traffic_view()['policy']
        unlimited = {'quota_bytes': None, 'base_rate_bps': None, 'exhausted_action': {'kind': 'suspend'}}
        assert self.sql("SELECT traffic_reset_day FROM users WHERE tenant_id='platform' AND id='alice'") == '1'
        try:
            self.traffic_policy({**unlimited, 'quota_bytes': '1'})
            self.wait('old cycle exhausted', self.traffic_view, lambda v: v['exhausted'])
            self.wait(
                'old cycle physical suspension',
                lambda: self.try_probe('tcp4', V4[0], 9092, 'provider'),
                lambda v: v is None,
                timeout=90,
            )
            revision = self.revision()
            orders = self.sql("SELECT count(*) FROM deployments WHERE kind='config'")
            generation = self.instance('gamma')['desired']['generation']
            before = int(self.traffic_view()['used_bytes'])
            clocks = {}
            code = 'import datetime,subprocess;d=datetime.datetime.now(datetime.timezone.utc);t=datetime.datetime(d.year+(d.month==12),1 if d.month==12 else d.month+1,1,0,1,tzinfo=datetime.timezone.utc);subprocess.run(["date","-u","-s",t.isoformat()],check=True)'
            for role in ['compute', 'provider', 'client']:
                clocks[role] = self.lab.python(role, code).decode().strip()
            self.cookies.clear()
            self.call(
                'POST', '/auth/login', {'operator_id': self.creds['operator_id'], 'password': self.creds['password']}
            )
            fresh = self.wait(
                'new billing cycle restores ordinary user policy',
                self.traffic_view,
                lambda v: not v['exhausted'] and not v['pending_nodes'],
            )
            assert int(fresh['used_bytes']) < before
            self.traffic_policy({**unlimited, 'quota_bytes': str(4 * 1024**2)})
            self.wait('new cycle actual chain traffic', lambda: self.try_probe('tcp4', V4[0], 9092, 'provider'))
            self.probe('udp6', V6[0], 9093, 'provider')
            assert self.sql("SELECT count(*) FROM deployments WHERE kind='config'") == orders
            assert self.instance('gamma')['desired']['generation'] == generation
            assert self.guest('gamma', 'cat /proc/sys/kernel/random/boot_id') == self.state['chain']['guest_boot']
            self.record(
                'actual isolated VM/PostgreSQL/Console/Agent clocks cross UTC month; existing subscriber billing cycle resets and chain traffic returns without a new configuration approval, ledger rewriting or Guest reboot',
                vm_clocks=clocks,
                revision_before=revision,
                revision_after=self.revision(),
                used_before=before,
                used_new_cycle=int(fresh['used_bytes']),
            )
        finally:
            self.traffic_policy(original or unlimited)

    def source_guard(self):
        chain = self.state['chain']['id']
        nic = self.state['nic']['id']
        defaults = [{'destination': net, 'interface_id': nic} for net in ['0.0.0.0/0', '::/0']]
        specific = [
            {'destination': address + ('/128' if ':' in address else '/32'), 'chain_id': chain}
            for address in [V4[1], V6[1]]
        ]
        services = [
            {
                'destination': address + ('/128' if ':' in address else '/32'),
                'interface_id': nic,
                'service': {'protocol': 'tcp', 'port_start': 9095, 'port_end': 9095},
            }
            for address in [V4[1], V6[1]]
        ]
        priorities = [{'interface_id': nic, 'ipv4': 5, 'ipv6': 5}, {'chain_id': chain, 'ipv4': 10, 'ipv6': 10}]
        self.reviewed_routes('alpha', defaults + specific + services, priorities, 'source-guard')
        item = self.instance('alpha')
        allocated = next(n['allocated'] for n in item['desired']['spec']['interfaces'] if n['id'] == nic)
        for family, address in [('4', V4[1]), ('6', V6[1])]:
            source = allocated['ipv4' if family == '4' else 'ipv6'].split('/')[0]
            endpoint = '[' + address + ']' if family == '6' else address
            env = 'env CHAIN_PROBE_SOURCE=' + shlex.quote(source) + ' ~/chain-probe '
            control = json.loads(
                self.guest('alpha', env + 'tcp' + family + ' ' + shlex.quote(endpoint + ':9095') + ' 256')
            )
            assert control['bytes'] == 256
            for protocol, port in [('tcp', 9092), ('udp', 9093)]:
                result = self.guest(
                    'alpha',
                    'timeout 4s '
                    + env
                    + protocol
                    + family
                    + ' '
                    + shlex.quote(endpoint + ':' + str(port))
                    + ' 256 >/tmp/bound.out 2>/tmp/bound.err; echo $?',
                )
                assert result in ('1', '124'), (protocol, family, result)
        self.record(
            'binding the real business source address cannot bypass a chain-selected prefix for TCP/UDP IPv4/IPv6; same-address permitted NIC L4 control works'
        )
        self.reviewed_routes('alpha', defaults, [priorities[0]], 'source-guard-cleanup')

    def cleanup_owned(self):
        changed = False
        for name in self.state['instances']:
            if self.instance(name)['reserved']:
                self.operation(name, 'delete', delete_data=True)
                changed = True
        if changed:
            self.complete(self.publish(), timeout=300)
        for name in self.state['instances']:
            item = self.instance(name)
            assert item['report']['power'] == 'deleted' and item['report']['volume_deleted'] and not item['reserved']
        for role in ('compute', 'provider'):
            proof = self.terminal_runtime_proof(role)
            assert 'primary_chain_routes' in proof['instance_identity_maps']
            self.record('actual Guest/chain cleanup completed with ownerless Ready TAP pool on ' + role, **proof)
        for table, condition in [
            ('cvm_nic_allocations', 'NOT released'),
            ('cvm_nic_bindings', 'NOT cleaned'),
            ('cvm_entries', 'NOT cleaned'),
            ('cvm_entry_tunnels', 'NOT cleaned'),
        ]:
            assert self.sql('SELECT count(*) FROM ' + table + ' WHERE ' + condition) == '0', table
        for node in self.state['nodes']:
            assert not self.call('GET', '/agent/v1/cvm/nics/desired', authorization=self.state['nodes'][node]['token'])[
                'bindings'
            ]
        self.record('all owned NIC/entry/tunnel claims released; empty network snapshot acknowledged by real Agents')

    def unauthorized_selection(self):
        if 'epsilon' not in self.state['instances'] or not self.instance('epsilon')['reserved']:
            self.create('epsilon', 'compute-a')
            self.complete(self.publish(), timeout=240)
        self.instance_ready('epsilon')
        boot = self.guest('epsilon', 'cat /proc/sys/kernel/random/boot_id')
        grant = {
            'app_id': self.state['chain']['app'],
            'tenant_id': 'platform',
            'user_id': 'alice',
            'ingress_id': self.state['chain']['ingress'],
            'enabled': False,
        }
        try:
            self.call('POST', '/grants', grant, expected=(200, 201))
            # This Guest has no chain reference yet. Its owner authorization
            # changes selection admission, but produces no configuration delta.
            item = self.instance('epsilon')
            routing = json.loads(json.dumps(item['desired']['spec']['routing']))
            routing.update(
                routes=[{'destination': '0.0.0.0/0', 'chain_id': self.state['chain']['id']}],
                priorities=[{'chain_id': self.state['chain']['id'], 'ipv4': 10, 'ipv6': 10}],
            )
            body = {
                'request_id': uuid.uuid4().hex,
                'expected_generation': item['desired']['generation'],
                'expected_revision': routing['revision'],
                'routes': routing,
            }
            revision = self.revision()
            allocations = self.sql('SELECT count(*) FROM cvm_nic_allocations')
            catalog = self.call('GET', '/cvm/chain-egress?instance_id=' + item['desired']['id'])
            assert catalog['supported'] and catalog['hidden_unauthorized'] >= 1
            assert not any(c['id'] == self.state['chain']['id'] for c in catalog['chains'])
            self.call(
                'POST',
                '/cvm/' + item['desired']['id'] + '/artifacts/preview',
                {'kind': 'routes', 'request': body},
                expected=403,
            )
            self.call('PUT', '/cvm/' + item['desired']['id'] + '/routes', body, expected=403)
            assert self.revision() == revision and self.instance('epsilon')['desired'] == item['desired']
            assert self.sql('SELECT count(*) FROM cvm_nic_allocations') == allocations
            assert self.guest('epsilon', 'cat /proc/sys/kernel/random/boot_id') == boot
            self.record(
                'real KVM Console hides unauthorized unused chain and rejects both compiler preview and direct route submission with HTTP 403, even for administrator; owner grant, generation, allocation and Guest remain unchanged'
            )
        finally:
            grant['enabled'] = True
            self.call('POST', '/grants', grant, expected=(200, 201))
        self.cleanup_owned()


if __name__ == '__main__':
    a = ChainAcceptance(sys.argv[1])
    if sys.argv[2] == 'prepare':
        a.prepare(Path(sys.argv[3]))
    elif sys.argv[2] == 'configure':
        a.configure()
    elif sys.argv[2] == 'primary':
        a.primary_guest()
    elif sys.argv[2] == 'traffic':
        a.traffic()
    elif sys.argv[2] in (
        'refresh',
        'mixed_routes',
        'nic_ipv6_pmtu',
        'faults',
        'quotas',
        'grant_revocation',
        'preview_lifecycle',
        'finish_preview_lifecycle',
        'ingress_guards',
        'chain_icmp',
        'cleanup_owned',
        'unauthorized_selection',
        'entry_existing',
        'period',
        'source_guard',
    ):
        getattr(a, sys.argv[2])()
    elif sys.argv[2] == 'candidate':
        a.candidate(sys.argv[3])
    else:
        raise SystemExit('unknown acceptance stage')
