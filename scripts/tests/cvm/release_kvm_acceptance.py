#!/usr/bin/env python3
"""Real production Console/Agent acceptance on owned Ubuntu KVMs, with resumable evidence."""

from datetime import datetime, timezone
import hashlib
import http.cookiejar
import json
import re
from pathlib import Path
import shlex
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
import guest_password

from release_kvm_lab import Lab

AGENT_PROTOCOL_VERSION = re.search(
    r'pub const AGENT_PROTOCOL_VERSION: u32 = (\d+);',
    (Path(__file__).resolve().parents[3] / 'crates/brocade-deployment/src/protocol.rs').read_text(),
).group(1)


class Acceptance:
    def __init__(self, output):
        self.output = Path(output)
        self.lab = Lab(self.output / 'lab-3')
        self.prefix = self.lab.state['prefix']
        self.creds = json.loads((self.output / 'services/credentials.json').read_text())
        self.origin = 'http://127.0.0.1:' + str(self.lab.state['console_port'])
        self.cookies = http.cookiejar.MozillaCookieJar(str(self.output / 'cookies'))
        if (self.output / 'cookies').exists():
            self.cookies.load(ignore_discard=True, ignore_expires=True)
        self.admin = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.cookies))
        path = self.output / 'acceptance.json'
        self.state = json.loads(path.read_text()) if path.exists() else {'checks': [], 'nodes': {}, 'instances': {}}

    def private(self, name, value):
        path = self.output / name
        path.write_text(json.dumps(value, indent=2, ensure_ascii=False))
        path.chmod(0o600)

    def save(self):
        self.private('acceptance.json', self.state)
        self.cookies.save(ignore_discard=True, ignore_expires=True)
        Path(self.cookies.filename).chmod(0o600)

    def record(self, name, **details):
        self.state['checks'].append({'name': name, 'at': datetime.now(timezone.utc).isoformat(), **details})
        self.save()
        print('PASS: ' + name, flush=True)

    def call(self, method, path, body=None, expected=200, authorization=None, opener=None, extra_headers=None):
        headers = {'Content-Type': 'application/json', 'x-brocade-protocol-version': AGENT_PROTOCOL_VERSION}
        headers.update(extra_headers or {})
        if authorization:
            headers['Authorization'] = 'Bearer ' + authorization
        request = urllib.request.Request(
            self.origin + path,
            method=method,
            headers=headers,
            data=json.dumps(body).encode() if body is not None else None,
        )
        try:
            response = (opener or self.admin).open(request, timeout=45)
        except urllib.error.HTTPError as error:
            response = error
        data = response.read()
        try:
            value = json.loads(data) if data else None
        except json.JSONDecodeError:
            value = {'non_json': data[:2000].decode(errors='replace')}
        if response.status not in (expected if isinstance(expected, tuple) else (expected,)):
            self.private(
                'last-http-error.json', {'method': method, 'path': path, 'status': response.status, 'response': value}
            )
            raise RuntimeError(
                f'{method} {path}: expected {expected}, received {response.status}; see private evidence'
            )
        self.save()
        return value

    def sql(self, query):
        return (
            self.lab.remote('compute', 'sudo -u postgres psql -d brocade -At -v ON_ERROR_STOP=1', data=query.encode())
            .stdout.decode()
            .strip()
        )

    def revision(self):
        return int(self.sql('SELECT current_revision FROM control_state WHERE id=TRUE'))

    def gate(self, **flags):
        current = self.state.setdefault('gate', {})
        current.update(flags)
        current['nodes'] = {
            hashlib.sha256(('Bearer ' + data['token']).encode()).hexdigest(): node
            for node, data in self.state['nodes'].items()
        }
        self.lab.python(
            'compute',
            'from pathlib import Path;import os,sys;p=Path('
            + repr(self.prefix + '/gate/control.json')
            + ');p.parent.mkdir(exist_ok=True);t=p.with_suffix(".tmp");t.write_bytes(sys.stdin.buffer.read());os.chmod(t,0o600);t.replace(p)',
            json.dumps(current).encode(),
        )
        self.save()
        # Close an already-upgraded channel before enabling HTTP fault injection.
        # With no fault active, the production Agent channel remains available.
        self.lab.remote(
            'compute',
            'if sudo systemctl is-active --quiet brocade-lab-gate; then sudo systemctl restart brocade-lab-gate; fi',
        )

    def wait(self, description, function, predicate=lambda x: bool(x), timeout=180):
        start = time.monotonic()
        last = None
        next_update = 30
        while time.monotonic() - start < timeout:
            last = function()
            if predicate(last):
                return last
            if time.monotonic() - start > next_update:
                print('WAIT: ' + description, flush=True)
                next_update += 30
            time.sleep(1)
        self.private('last-wait.json', {'description': description, 'value': last})
        raise RuntimeError('timed out: ' + description)

    def detail(self, deployment):
        return self.call('GET', '/deployments/' + str(deployment))

    def publish(self):
        result = self.call(
            'POST',
            '/deployments',
            {'revision_id': self.revision(), 'idempotency_key': uuid.uuid4().hex},
            expected=(200, 201),
        )
        self.state['last_deployment'] = result['deployment_id']
        self.save()
        return result['deployment_id']

    def complete(self, deployment, timeout=180):
        return self.wait(
            'global deployment ' + str(deployment),
            lambda: self.detail(deployment),
            lambda d: d['status'] == 'succeeded',
            timeout=timeout,
        )

    def instance(self, name):
        return self.call('GET', '/cvm/' + self.state['instances'][name])

    def instance_ready(self, name):
        return self.wait(name + ' execution receipt', lambda: self.instance(name), lambda i: i['status'] == 'applied')

    def desired(self, node):
        return self.call(
            'GET', '/agent/v1/cvm/desired', authorization=self.state['nodes'][node]['token'], expected=(200, 204)
        )

    def guest_endpoint(self, name):
        item = self.instance(name)
        role = self.state['nodes'][item['node_id']]['role']
        address = item['report']['access_address']
        if not address:
            taps = json.loads(self.lab.remote(role, 'curl -fsS http://127.0.0.1:18080/v1/network/taps').stdout)['taps']
            address = next(t['sandboxIP'] for t in taps if t['ownerSandboxID'] == item['desired']['id'])
        return item, role, address

    def guest(self, name, command, timeout=60, *, cached_revision=None):
        item, role, address = self.guest_endpoint(name)
        if cached_revision is None:
            words = self.prepare_guest_password(item, role)
        else:
            guest_password.validate_cached(self.output, item, cached_revision)
            words = guest_password.ssh(self.prefix, item['desired']['id'])
        words += [
            '-o',
            'StrictHostKeyChecking=accept-new',
            '-o',
            'UserKnownHostsFile=' + self.prefix + '/guest.known-' + item['desired']['id'],
            '-o',
            'ConnectTimeout=5',
            'cube@' + address,
            command,
        ]
        return self.lab.remote(role, 'sudo ' + shlex.join(words), timeout=timeout).stdout.decode().strip()

    def operation(self, instance_name, action, **body):
        item = self.instance(instance_name)
        return self.call(
            'POST',
            '/cvm/' + item['desired']['id'] + '/operations',
            {
                'action': action,
                'request_id': uuid.uuid4().hex,
                'expected_generation': item['desired']['generation'],
                **body,
            },
            expected=202,
        )

    def bootstrap(self):
        self.call(
            'POST',
            '/auth/init',
            {
                'operator_id': self.creds['operator_id'],
                'display_name': 'KVM acceptance',
                'password': self.creds['password'],
                'root_tenant': 'platform',
            },
            expected=201,
            authorization=self.creds['bootstrap'],
        )
        for user in ('alice', 'bob'):
            self.call('POST', '/users', {'tenant_id': 'platform', 'id': user}, expected=201)
        dist = self.call('GET', '/enroll/dist')
        for role, node in [('compute', 'compute-a'), ('provider', 'compute-b')]:
            result = self.call(
                'POST',
                '/nodes/provision',
                {
                    'id': node,
                    'tenant_id': 'platform',
                    'name': node,
                    'public_ipv4': self.lab.state['roles'][role]['ipv4'],
                    'public_ipv6': self.lab.state['roles'][role]['ipv6'],
                    'wg_listen_port': 51820,
                    'api_port': 10085,
                    'overlay': True,
                    'egress_allowed': True,
                    'dns': {'t': 'system'},
                },
                expected=201,
            )
            token = self.call('POST', '/nodes/' + node + '/agent-token', {}, expected=201)['token']
            self.state['nodes'][node] = {'role': role, 'token': token, 'provision_revision': result['revision_id']}
            self.save()
            self.lab.python(
                role,
                'from pathlib import Path;import os,sys;p=Path('
                + repr(self.prefix + '/node.token')
                + ');p.write_bytes(sys.stdin.buffer.read());os.chmod(p,0o600)',
                token.encode(),
            )
            self.lab.remote(
                role,
                'sudo curl -fsS http://'
                + self.lab.state['roles']['compute']['ipv4']
                + ':8080/brocade-xray/x86_64 -o '
                + self.prefix
                + '/xray && sudo chmod 700 '
                + self.prefix
                + '/xray',
            )
            actual = self.lab.remote(role, 'sudo sha256sum ' + self.prefix + '/xray').stdout.decode().split()[0]
            assert actual == dist['xray_bin_sha256_x86_64']
        self.gate()
        unit = (
            '[Service]\nExecStart=/usr/bin/python3 '
            + self.prefix
            + '/proxy.py --root '
            + self.prefix
            + '/gate --port 18081\nRestart=on-failure\n'
        )
        self.lab.python(
            'compute',
            'from pathlib import Path;import sys;Path("/etc/systemd/system/brocade-lab-gate.service").write_bytes(sys.stdin.buffer.read())',
            unit.encode(),
        )
        self.lab.remote('compute', 'sudo systemctl daemon-reload && sudo systemctl start brocade-lab-gate')
        for node, data in self.state['nodes'].items():
            role = data['role']
            environment = {
                'BROCADE_AGENT_SERVER': 'http://' + self.lab.state['roles']['compute']['ipv4'] + ':18081',
                'BROCADE_NODE_TOKEN_FILE': self.prefix + '/node.token',
                'BROCADE_AGENT_STATE_DIR': '/var/lib/bc-' + self.lab.state['task'][:6],
                'BROCADE_AGENT_APPLY': 'linux',
                'BROCADE_CVM_CONFIG': '/opt/brocade/cvm/current/generated/cube.json',
                'BROCADE_CVM_STATE_DIR': self.prefix + '/runtime',
                'BROCADE_XRAY_BIN': self.prefix + '/xray',
                'PATH': self.prefix + ':/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin',
            }
            unit = (
                '[Unit]\nAfter=network-online.target brocade-cvm-cubelet.service\n[Service]\n'
                + ''.join('Environment=' + json.dumps(k + '=' + v) + '\n' for k, v in environment.items())
                + 'ExecStart=/usr/local/bin/brocade-agent run\nRestart=always\nKillMode=control-group\nTimeoutStopSec=20\n'
            )
            self.lab.python(
                role,
                'from pathlib import Path;import sys;Path("/etc/systemd/system/brocade-lab-agent.service").write_bytes(sys.stdin.buffer.read())',
                unit.encode(),
            )
            self.lab.remote(role, 'sudo systemctl daemon-reload && sudo systemctl start brocade-lab-agent')
        workspace = self.wait(
            'both real Agent inventories and verified native image manifests',
            lambda: self.call('GET', '/cvm'),
            lambda w: (
                len(w['hosts']) == 2
                and all(h['fresh'] and h['capabilities'].get('artifact_compiler') for h in w['hosts'])
            ),
        )
        self.private('initial-workspace.json', workspace)
        self.call('PUT', '/deployment-settings', {'unattended': True})
        deployment = self.publish()
        self.complete(deployment)
        self.record(
            'production Console, two production CVM Agents and ordinary WireGuard configurations converge',
            deployment=deployment,
        )
        for role in ('compute', 'provider'):
            self.lab.remote(role, 'sudo wg show && sudo ip -j link show wg0')
        self.record('both Ubuntu 26.04 kernels hold the published ordinary WireGuard interfaces')
        self.authorize()

    def authorize(self):
        workspace = self.call('GET', '/cvm')
        for host in workspace['available_hosts']:
            self.call(
                'PUT',
                '/cvm/hosts/' + host['node_id'],
                {'enabled': True, 'expected_revision': host['host_revision']},
                expected=200,
            )
        self.record(
            'actual host inventory and image manifests ready; instances are administrator-created without user quotas'
        )

    def prepare_guest_password(self, item, role):
        guest_password.prepare(
            self.call,
            self.output,
            item,
            lambda source, name, executable: self.lab.upload(
                role, source, self.prefix + '/' + name, 0o700 if executable else 0o600
            ),
        )
        return guest_password.ssh(self.prefix, item['desired']['id'])

    def create(self, name, node, resources=None):
        host = next(h for h in self.call('GET', '/cvm')['hosts'] if h['node_id'] == node)
        request = {
            'request_id': uuid.uuid4().hex,
            'tenant_id': 'platform',
            'user_id': 'alice',
            'node_id': node,
            'name': name,
            'image_id': host['capabilities']['images'][0]['id'],
            'resources': resources or {'vcpus': 1, 'memory_bytes': 256 * 1024**2, 'root_disk_bytes': 3 * 1024**3},
            'interfaces': [
                {
                    'source_id': host['capabilities']['network_sources'][0]['id'],
                    'purpose': 'contact',
                    'default_ipv4': False,
                    'default_ipv6': False,
                }
            ],
        }
        item = self.call('POST', '/cvm', request, expected=202)
        replay = self.call('POST', '/cvm', request, expected=202)
        assert replay['desired']['id'] == item['desired']['id']
        self.state['instances'][name] = item['desired']['id']
        self.save()
        return item

    def publish_barriers(self):
        self.call('PUT', '/deployment-settings', {'unattended': False})
        first = self.create('alpha', 'compute-a')
        second = self.create('beta', 'compute-b')
        revision = self.revision()
        preview = self.call('POST', '/deployments/plan/artifacts', {'revision_id': revision})
        self.private('mixed-preview.json', preview)
        assert preview['revision'] == revision
        for target in preview['targets']:
            assert 'cvm' in target['desired_structure'] and 'wireguard' in target['desired_structure']
            assert target['desired_structure']['cvm']['revision_id'] == revision
        time.sleep(8)
        assert not self.desired('compute-a')['instances'] and not self.desired('compute-b')['instances']
        self.record(
            'saving two native instances and previewing mixed artifacts uses one global R and executes neither',
            revision=revision,
        )
        self.gate(hold_desired=['compute-a', 'compute-b'])
        cancelled = self.publish()
        self.call('POST', f'/deployments/{cancelled}/cancel', {}, expected=(200, 202))
        assert not self.desired('compute-a')['instances']
        assert self.revision() == revision
        self.record(
            'unclaimed mixed release cancellation delivers no CVM intent and preserves global R', deployment=cancelled
        )
        self.gate(hold_desired=[], hold_machine=['compute-a'])
        deployment = self.publish()
        self.state['barrier_deployment'] = deployment
        self.save()
        self.wait(
            'actual ordinary receipt intercepted',
            lambda: (
                self.lab.remote(
                    'compute', 'sudo test -s ' + self.prefix + '/gate/compute-a-machine.json', check=False
                ).returncode
                == 0
            ),
        )
        detail = self.detail(deployment)
        assert next(t for t in detail['targets'] if t['node_id'] == 'compute-a')['status'] == 'dispatched'
        assert not self.desired('compute-a')['instances']
        assert not self.instance('alpha')['report']
        self.record(
            'actual ordinary machine success receipt is delayed; CVM stays physically absent behind its barrier',
            deployment=deployment,
        )
        # Break only the owned runtime dependency, after successful machine execution.
        self.lab.remote('compute', 'sudo systemctl stop brocade-cvm-cubelet')
        self.gate(hold_machine=[])
        unavailable = self.wait(
            'runtime loss keeps held identity', lambda: self.instance('alpha'), lambda i: i['status'] == 'unknown'
        )
        assert unavailable['reserved']
        assert not self.desired('compute-b')['instances']
        self.record('unavailable runtime retains reservations and keeps later-wave Guest absent', deployment=deployment)
        before = self.lab.remote(
            'compute', 'sudo sha256sum /var/lib/bc-' + self.lab.state['task'][:6] + '/wireguard.conf'
        ).stdout.decode()
        self.lab.remote('compute', 'sudo systemctl start brocade-cvm-cubelet')
        first = self.instance_ready('alpha')
        self.wait(
            'first mixed target succeeds',
            lambda: self.detail(deployment),
            lambda d: next(t for t in d['targets'] if t['node_id'] == 'compute-a')['status'] == 'succeeded',
        )
        after = self.lab.remote(
            'compute', 'sudo sha256sum /var/lib/bc-' + self.lab.state['task'][:6] + '/wireguard.conf'
        ).stdout.decode()
        assert before == after and self.revision() == revision
        self.record('runtime recovery starts actual Guest without changing ordinary WG bytes or global R')
        self.complete(deployment, timeout=240)
        self.instance_ready('beta')
        self.record(
            'non-disruptive creation advances automatically to wave two only after wave one completes',
            deployment=deployment,
        )
        self.native_guests()

    def finish_initial(self):
        self.complete(self.state['barrier_deployment'], timeout=240)
        self.record(
            'non-disruptive creation advances automatically to wave two only after wave one completes',
            deployment=self.state['barrier_deployment'],
        )
        self.native_guests()

    def integrity_failure(self):
        self.operation('alpha', 'restart')
        self.operation('beta', 'restart')
        revision = self.revision()
        boot = self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id')
        credential_revision = self.instance('alpha')['desired']['spec']['login_revision']
        before = self.lab.remote(
            'compute', 'sudo sha256sum /var/lib/bc-' + self.lab.state['task'][:6] + '/wireguard.conf'
        ).stdout.decode()
        self.gate(corrupt_artifact=['compute-a'])
        deployment = self.publish()
        detail = self.wait(
            'real Agent rejects corrupted frozen native artifact',
            lambda: self.detail(deployment),
            lambda d: d['status'] == 'halted',
        )
        assert next(t for t in detail['targets'] if t['node_id'] == 'compute-a')['status'] == 'failed-dirty'
        assert next(t for t in detail['targets'] if t['node_id'] == 'compute-b')['status'] == 'pending'
        assert self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id', cached_revision=credential_revision) == boot
        self.record(
            'Agent rejects actual corrupted native bytes before execution; failure halts later wave without rebooting Guest',
            deployment=deployment,
        )
        self.gate(corrupt_artifact=[])
        self.call('POST', f'/deployments/{deployment}/targets/compute-a/retry', {}, expected=(200, 202))
        self.wait(
            'CVM-only retry',
            lambda: self.detail(deployment),
            lambda d: next(t for t in d['targets'] if t['node_id'] == 'compute-a')['status'] == 'succeeded',
        )
        after = self.lab.remote(
            'compute', 'sudo sha256sum /var/lib/bc-' + self.lab.state['task'][:6] + '/wireguard.conf'
        ).stdout.decode()
        assert before == after and self.revision() == revision
        assert self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id') != boot
        self.record('restored artifact plus CVM-only retry applies real restart at same R without ordinary WG replay')
        self.call('POST', f'/deployments/{deployment}/waves/2/confirm', {'actor': None}, expected=(200, 202))
        self.complete(deployment, timeout=240)
        self.record('later wave requires explicit confirmation after genuine artifact failure and retry')
        self.native_guests()
        self.call('PUT', '/deployment-settings', {'unattended': True})

    def retry_integrity(self):
        deployment = self.state['last_deployment']
        detail = self.detail(deployment)
        revision = detail['revision_id']
        assert detail['status'] == 'halted'
        boot = self.state['native']['alpha']['boot']
        assert (
            self.guest(
                'alpha',
                'cat /proc/sys/kernel/random/boot_id',
                cached_revision=self.instance('alpha')['desired']['spec']['login_revision'],
            )
            == boot
        )
        assert self.instance('alpha')['report']['status'] == 'failed'
        self.record(
            'real corrupted artifact rejection halts global release and leaves physical Guest boot unchanged',
            deployment=deployment,
        )
        before = self.lab.remote(
            'compute', 'sudo sha256sum /var/lib/bc-' + self.lab.state['task'][:6] + '/wireguard.conf'
        ).stdout.decode()
        self.gate(corrupt_artifact=[])
        self.call('POST', f'/deployments/{deployment}/targets/compute-a/retry', {}, expected=(200, 202))
        self.wait(
            'CVM-only retry',
            lambda: self.detail(deployment),
            lambda d: next(t for t in d['targets'] if t['node_id'] == 'compute-a')['status'] == 'succeeded',
            timeout=240,
        )
        after = self.lab.remote(
            'compute', 'sudo sha256sum /var/lib/bc-' + self.lab.state['task'][:6] + '/wireguard.conf'
        ).stdout.decode()
        assert before == after and self.revision() == revision
        assert self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id') != boot
        assert next(t for t in self.detail(deployment)['targets'] if t['node_id'] == 'compute-b')['status'] == 'pending'
        self.record('CVM-only retry consumes intact native bytes, preserves ordinary WG and R, and holds later wave')
        self.call('POST', f'/deployments/{deployment}/waves/2/confirm', {'actor': None}, expected=(200, 202))
        self.complete(deployment, timeout=240)
        self.native_guests()
        self.call('PUT', '/deployment-settings', {'unattended': True})

    def verify_retry_fix(self):
        deployment = self.state['last_deployment']
        detail = self.detail(deployment)
        revision = detail['revision_id']
        assert detail['status'] == 'halted'
        boot = self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id')
        generation = self.instance('alpha')['desired']['generation']
        self.record(
            'real corrupted artifact rejection halts global release and leaves physical Guest boot unchanged',
            deployment=deployment,
        )
        before = self.lab.remote(
            'compute', 'sudo sha256sum /var/lib/bc-' + self.lab.state['task'][:6] + '/wireguard.conf'
        ).stdout.decode()
        self.gate(corrupt_artifact=[])
        self.call('POST', f'/deployments/{deployment}/targets/compute-a/retry', {}, expected=(200, 202))
        self.wait(
            'CVM-only retry',
            lambda: self.detail(deployment),
            lambda d: next(t for t in d['targets'] if t['node_id'] == 'compute-a')['status'] == 'succeeded',
            timeout=240,
        )
        after = self.lab.remote(
            'compute', 'sudo sha256sum /var/lib/bc-' + self.lab.state['task'][:6] + '/wireguard.conf'
        ).stdout.decode()
        assert before == after and self.revision() == revision
        assert self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id') == boot
        assert self.instance('alpha')['desired']['generation'] > generation
        assert next(t for t in self.detail(deployment)['targets'] if t['node_id'] == 'compute-b')['status'] == 'pending'
        self.record(
            'fixed retry ignores cached receipts, preserves already-applied boot and ordinary WG, and uses a new execution at the same R'
        )
        self.call('POST', f'/deployments/{deployment}/waves/2/confirm', {'actor': None}, expected=(200, 202))
        self.complete(deployment, timeout=240)
        self.native_guests()
        self.call('PUT', '/deployment-settings', {'unattended': True})

    def native_guests(self):
        for name in ('alpha', 'beta'):
            item = self.instance_ready(name)
            role = self.state['nodes'][item['node_id']]['role']
            proof = json.loads(
                self.lab.python(
                    role,
                    r"""import glob,json,os,sys
wanted=sys.stdin.read();out=[]
for p in glob.glob('/proc/[0-9]*'):
 try:
  cmd=open(p+'/cmdline','rb').read().decode(errors='replace')
  if wanted not in cmd or 'containerd-shim-cube' not in cmd:continue
  fds=[os.readlink(f) for f in glob.glob(p+'/fd/*') if os.path.lexists(f)]
  if any('kvm-vm' in f for f in fds):out.append({'pid':int(p.rsplit('/',1)[1]),'kvm_vm_fd':True,'vcpus':sum('kvm-vcpu' in f for f in fds)})
 except OSError:pass
assert len(out)==1 and out[0]['vcpus']==1
print(json.dumps(out[0]))
""",
                    item['report']['runtime_id'].encode(),
                )
            )
            boot = self.wait(name + ' SSH', lambda: self.guest(name, 'cat /proc/sys/kernel/random/boot_id'))
            assert self.guest(name, 'id -un') == 'cube'
            marker = self.lab.state['task'] + '-' + name
            assert self.guest(name, 'printf %s ' + shlex.quote(marker) + ' > ~/acceptance; cat ~/acceptance') == marker
            bundle = self.call('GET', '/cvm/' + item['desired']['id'] + '/artifacts')['current']
            receipt = item['report']['artifacts']
            assert 'mode' not in receipt and not receipt['mismatches']
            assert receipt['input_sha256'] == bundle['input_sha256']
            assert len(bundle['files']) == len(receipt['files']) == 4
            self.state.setdefault('native', {})[name] = {
                'boot': boot,
                'marker': marker,
                'volume': item['report']['volume_id'],
            }
            self.record(
                'native four-artifact execution, actual KVM FD, SSH and disk write: ' + name,
                boot=boot,
                **proof,
            )

    def change(self, suffix, body, name='alpha', method='PUT'):
        item = self.instance(name)
        revision = self.revision()
        result = self.call(
            method,
            '/cvm/' + item['desired']['id'] + suffix,
            {'request_id': uuid.uuid4().hex, 'expected_generation': item['desired']['generation'], **body},
            expected=202,
        )
        verification = self.call('POST', '/deployments/verify', {'revision_id': self.revision()})
        if verification['summary']['changed_targets'] == 0:
            assert result['desired']['generation'] == item['desired']['generation']
            assert self.revision() == revision
            self.instance_ready(name)
            self.record('actual unchanged configuration keeps its generation and revision without an empty publication')
            return result
        deployment = self.publish()
        self.complete(deployment, timeout=240)
        self.instance_ready(name)
        return result

    def network_ready(self, name='alpha'):
        def ready(network):
            bindings = network['bindings']
            return bool(bindings) and all(
                b['phase'] == 'active'
                and all(
                    b[s]
                    and b[s]['generation'] == b['generation']
                    and b[s]['configured']
                    and b[s]['reachable'] is True
                    and not b[s]['error']
                    for s in ('consumer', 'provider')
                )
                for b in bindings
            )

        return self.wait(
            'actual two-sided network configuration and reachability',
            lambda: self.call('GET', '/cvm/' + self.state['instances'][name] + '/network'),
            ready,
        )

    def echo(self, protocol='tcp4', text='actual-network', name='alpha'):
        client = self.lab.state['roles']['client']
        endpoint = (
            client['ipv4'] + ':9090'
            if protocol == 'tcp4'
            else client['ipv4'] + ':9091'
            if protocol == 'udp4'
            else '[' + client['ipv6'] + ']:' + ('9090' if protocol == 'tcp6' else '9091')
        )
        deadline = time.monotonic() + 15
        while True:
            try:
                value = self.guest(name, '~/probe ' + protocol + ' ' + shlex.quote(endpoint) + ' ' + shlex.quote(text))
                if value == text:
                    return value
            except RuntimeError:
                pass
            if time.monotonic() >= deadline:
                raise RuntimeError('actual ' + protocol + ' traffic did not converge after neighbor/carrier recovery')
            time.sleep(0.3)

    def networks(self):
        # The third KVM is the independent actual network peer, not a mock server.
        echo = r"""import socket,threading
def serve(family,kind,port):
 s=socket.socket(family,kind)
 if family==socket.AF_INET6:s.setsockopt(socket.IPPROTO_IPV6,socket.IPV6_V6ONLY,1)
 s.bind(('::' if family==socket.AF_INET6 else '0.0.0.0',port))
 if kind==socket.SOCK_STREAM:
  s.listen(64)
  def connection(c):
   with c:
    while True:
     data=c.recv(65535)
     if not data:return
     c.sendall(data)
  while True:
   c,_=s.accept();threading.Thread(target=connection,args=(c,),daemon=True).start()
 else:
  while True:
   data,peer=s.recvfrom(65535);s.sendto(data,peer)
for f in [socket.AF_INET,socket.AF_INET6]:
 for k,p in [(socket.SOCK_STREAM,9090),(socket.SOCK_DGRAM,9091)]:threading.Thread(target=serve,args=(f,k,p),daemon=True).start()
threading.Event().wait()
"""
        source = self.output / 'services/inputs/echo.py'
        source.write_text(echo)
        self.lab.upload('client', source, '/home/cvm-lab/echo.py')
        self.lab.remote('client', 'sudo systemd-run --unit=brocade-lab-echo /usr/bin/python3 /home/cvm-lab/echo.py')
        probe = self.output / 'services/inputs/probe'
        for role in ('compute', 'provider', 'client'):
            self.lab.upload(role, probe, self.prefix + '/probe')
        for name in ('alpha', 'beta'):
            item = self.instance(name)
            role = self.state['nodes'][item['node_id']]['role']
            words = self.prepare_guest_password(item, role) + [
                '-o',
                'StrictHostKeyChecking=accept-new',
                '-o',
                'UserKnownHostsFile=' + self.prefix + '/guest.known-' + item['desired']['id'],
                'cube@' + item['report']['access_address'],
                'cat > ~/probe && chmod 700 ~/probe',
            ]
            self.lab.remote(role, 'sudo cat ' + self.prefix + '/probe | sudo ' + shlex.join(words))
            assert self.guest(name, 'sha256sum ~/probe').split()[0] == hashlib.sha256(probe.read_bytes()).hexdigest()
        catalogue = self.wait(
            'both production network providers',
            lambda: self.call('GET', '/cvm/networks'),
            lambda c: len(c['providers']) == 2 and all(p['fresh'] for p in c['providers']),
        )
        self.private('network-catalogue.json', catalogue)
        provider = self.lab.state['roles']['provider']
        spec = {
            'name': 'actual cross-host egress',
            'provider_node': 'compute-b',
            'resource_domain': 'acceptance-' + self.lab.state['task'],
            'ipv4_pool': '10.120.10.0/24',
            'ipv6_pool': 'fd12:3456::/120',
            'public_ipv4_pool': None,
            'public_ipv6_pool': None,
            'shared_ipv4': provider['ipv4'],
            'shared_ipv6': provider['ipv6'],
            'address_mode': 'shared_nat',
            'destinations': ['0.0.0.0/0', '::/0'],
            'route_presets': ['0.0.0.0/0', '::/0'],
            'dns_suggestions': [],
            'transports': [
                {
                    'kind': kind,
                    'listen_side': 'provider',
                    'endpoint': provider['ipv4'],
                    'port_start': start,
                    'port_end': start + 100,
                    'mtu': 1400,
                }
                for kind, start in [('wireguard', 42000), ('xray', 43000)]
            ],
            'grants': [{'tenant_id': 'platform', 'user_id': 'alice'}],
            'limits': {'max_attachments': 4, 'upload_bps': None, 'download_bps': None, 'monthly_bytes': 512 * 1024},
        }
        publication = self.call(
            'POST',
            '/cvm/networks',
            {'request_id': uuid.uuid4().hex, 'expected_revision': 0, 'spec': spec},
            expected=201,
        )
        self.state['publication'] = publication
        self.save()
        self.change('/interfaces', {'publication_id': publication['id'], 'transport': 'wireguard'}, method='POST')
        self.network_ready()
        first = self.instance('alpha')['desired']['spec']['interfaces'][1]
        self.state['nic'] = first
        self.save()
        self.record(
            'Store publication allocation compiles a real business NIC absent from host contact catalogue',
            interface=first['id'],
        )
        routing = self.instance('alpha')['desired']['spec']['routing']
        routing['routes'] = [
            {'destination': '0.0.0.0/0', 'interface_id': first['id']},
            {'destination': '::/0', 'interface_id': first['id']},
        ]
        self.change('/routes', {'expected_revision': routing['revision'], 'routes': routing})
        self.finish_networks()

    def finish_networks(self):
        self.network_ready()
        first = self.state['nic']
        for protocol in ('tcp4', 'udp4', 'tcp6', 'udp6'):
            assert self.echo(protocol, 'wireguard-' + protocol) == 'wireguard-' + protocol
        self.record('actual cross-host WG business NIC carries IPv4 and IPv6 TCP and UDP')
        boot = self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id')
        for kind in ('xray', 'wireguard'):
            self.change('/interfaces/' + first['id'] + '/transport', {'transport': kind})
            self.network_ready()
            for protocol in ('tcp4', 'udp4', 'tcp6', 'udp6'):
                assert self.echo(protocol, kind + '-' + protocol) == kind + '-' + protocol
            current = self.instance('alpha')['desired']['spec']['interfaces'][1]
            for key in ('allocation_id', 'mac', 'ipv4', 'ipv6'):
                assert current['allocated'][key] == first['allocated'][key]
            assert self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id') == boot
            self.record('native artifact hot transport change applies on both hosts without Guest reboot: ' + kind)

    def lifecycle(self):
        original = self.instance('beta')
        marker = self.state['native']['beta']['marker']
        for action in ('stop', 'start', 'restart'):
            self.operation('beta', action)
            deployment = self.publish()
            self.complete(deployment, timeout=240)
            current = self.instance_ready('beta')
            assert current['report']['volume_id'] == original['report']['volume_id']
            assert current['report']['power'] == ('stopped' if action == 'stop' else 'running')
            if action != 'stop':
                assert self.guest('beta', 'cat ~/acceptance') == marker
            self.record('global approval plus actual native lifecycle and retained volume: ' + action)
        self.configure_lifecycle()

    def configure_lifecycle(self):
        original = self.instance('beta')
        marker = self.state['native']['beta']['marker']
        self.call(
            'POST',
            '/cvm/' + original['desired']['id'] + '/operations',
            {
                'request_id': uuid.uuid4().hex,
                'expected_generation': original['desired']['generation'],
                'action': 'configure',
                'name': 'beta-expanded',
                'vcpus': 2,
                'memory_bytes': 384 * 1024**2,
            },
            expected=409,
        )
        self.operation('beta', 'stop')
        self.complete(self.publish(), timeout=240)
        self.operation('beta', 'configure', name='beta-expanded', vcpus=2, memory_bytes=384 * 1024**2)
        self.complete(self.publish(), timeout=240)
        assert self.instance('beta')['report']['power'] == 'stopped'
        self.operation('beta', 'start')
        self.complete(self.publish(), timeout=240)
        assert self.guest('beta', 'getconf _NPROCESSORS_ONLN') == '2'
        assert int(self.guest('beta', "awk '/MemTotal:/{print $2}' /proc/meminfo")) > 300000
        assert self.guest('beta', 'cat ~/acceptance') == marker
        self.record('native resource artifacts change actual KVM vCPU and Guest RAM while retaining data')
        self.operation(
            'beta',
            'reinstall',
            image_id=original['desired']['spec']['image']['id'],
        )
        self.complete(self.publish(), timeout=240)
        self.finish_reinstall()

    def finish_reinstall(self):
        # A verified reinstall intentionally regenerates Guest SSH host keys. Rotate only
        # this owned Guest's private known-hosts file after its matching terminal receipt.
        item = self.instance_ready('beta')
        assert item['report']['reinstall_epoch'] == item['desired']['reinstall_epoch'] > 0
        known = self.prefix + '/guest.known-' + item['desired']['id']
        role = self.state['nodes'][item['node_id']]['role']
        old = self.lab.remote(role, 'sudo ssh-keygen -lf ' + known + ' -E sha256').stdout.decode()
        self.lab.python(
            role,
            'from pathlib import Path;p=Path('
            + repr(known)
            + ');assert p.is_file();p.rename(p.with_suffix(".before-reinstall"))',
        )
        assert self.guest('beta', 'test ! -e ~/acceptance && echo clean') == 'clean'
        assert self.guest('beta', 'id -un') == 'cube'
        new = self.guest('beta', 'ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256')
        assert new.split()[1] not in old
        self.record(
            'reinstall consumes verified native creation and Guest-init artifacts; old data and SSH host identity are reset and authorized SSH works'
        )

    def mixed_mtu_route(self):
        boot = self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id')
        for node in self.state['nodes']:
            self.call('PUT', '/nodes/' + node, {'mtu': 1360, 'dns': {'t': 'servers', 'v': ['1.1.1.1']}})
        item = self.instance('alpha')
        routes = item['desired']['spec']['routing']
        routes['dns'] = ['9.9.9.9']
        self.call(
            'PUT',
            '/cvm/' + item['desired']['id'] + '/routes',
            {
                'request_id': uuid.uuid4().hex,
                'expected_generation': item['desired']['generation'],
                'expected_revision': routes['revision'],
                'routes': routes,
            },
            expected=202,
        )
        revision = self.revision()
        preview = self.call('POST', '/deployments/plan/artifacts', {'revision_id': revision})
        self.private('actual-mixed-config-preview.json', preview)
        assert preview['revision'] == revision
        host = next(t for t in preview['targets'] if t['node_id'] == 'compute-a')
        assert host['desired_structure']['cvm']['revision_id'] == revision
        assert any(i['files'] for i in host['desired_structure']['cvm']['instances'])
        deployment = self.publish()
        self.complete(deployment, timeout=240)
        for role, peer in [('compute', '10.66.0.2'), ('provider', '10.66.0.1')]:
            link = json.loads(self.lab.remote(role, 'sudo ip -j link show wg0').stdout)[0]
            assert link['mtu'] == 1360
            self.lab.remote(role, 'ping -c 2 -W 3 -I wg0 ' + peer)
        assert '9.9.9.9' in self.guest('alpha', 'cat /etc/resolv.conf')
        assert self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id') == boot
        assert self.echo(text='mixed-config-data') == 'mixed-config-data'
        self.state['ui_deployment'] = deployment
        self.record(
            'one global release applies changed ordinary WG/Xray and native route/DNS artifacts; real WG pings and business data survive without Guest reboot',
            deployment=deployment,
            revision=revision,
        )

    def capacity_routes(self):
        publication = self.state['publication']
        boot = self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id')
        self.change('/interfaces', {'publication_id': publication['id'], 'transport': 'wireguard'}, method='POST')
        self.network_ready()
        item = self.instance('alpha')
        first, second = item['desired']['spec']['interfaces'][1:]
        assert first['allocated']['mac'] != second['allocated']['mac']
        before = self.sql('SELECT count(*) FROM cvm_nic_allocations')
        self.call(
            'POST',
            '/cvm/' + item['desired']['id'] + '/interfaces',
            {
                'request_id': uuid.uuid4().hex,
                'expected_generation': item['desired']['generation'],
                'publication_id': publication['id'],
                'transport': 'wireguard',
            },
            expected=(400, 409),
        )
        assert self.sql('SELECT count(*) FROM cvm_nic_allocations') == before
        client = self.lab.state['roles']['client']
        routes = item['desired']['spec']['routing']
        routes['routes'] += [
            {
                'destination': client['ipv4'] + '/32',
                'interface_id': second['id'],
                'service': {'protocol': 'tcp', 'port_start': 9090, 'port_end': 9090},
            },
            {'destination': client['ipv6'] + '/128', 'interface_id': second['id']},
        ]
        self.change('/routes', {'expected_revision': routes['revision'], 'routes': routes})
        devices = {n['id']: n['device'] for n in self.instance('alpha')['report']['interfaces']}
        for protocol in ('tcp4', 'udp4', 'tcp6', 'udp6'):
            assert self.echo(protocol, 'route-' + protocol) == 'route-' + protocol
        assert (
            json.loads(self.guest('alpha', 'ip -j route get ' + client['ipv4'] + ' ipproto 6 dport 9090'))[0]['dev']
            == devices[second['id']]
        )
        assert (
            json.loads(self.guest('alpha', 'ip -j route get ' + client['ipv4'] + ' ipproto 17 dport 9091'))[0]['dev']
            == devices[first['id']]
        )
        self.record(
            'two actual Store NICs occupy independent slots and L3/L4 selection; a third business NIC is rejected without allocation'
        )
        network = self.network_ready()
        binding = next(b['id'] for b in network['bindings'] if b['interface_id'] == second['id'])
        root = '/var/lib/bc-' + self.lab.state['task'][:6]
        anchor = json.loads(
            self.lab.remote('compute', 'sudo cat ' + root + '/cvm-nics/anchor-' + second['id'] + '-c.json').stdout
        )
        links = json.loads(self.lab.remote('compute', 'sudo ip -n ' + anchor['namespace'] + ' -j link show').stdout)
        link = next(l['ifname'] for l in links if l.get('ifalias') == 'brocade-binding:' + binding)
        self.lab.remote('compute', 'sudo systemctl kill --kill-whom=main --signal=SIGSTOP brocade-lab-agent')
        try:
            self.lab.remote('compute', 'sudo ip -n ' + anchor['namespace'] + ' link set ' + link + ' down')
            assert self.echo('udp4', 'healthy-default') == 'healthy-default'
            status = self.guest(
                'alpha', '~/probe tcp4 ' + client['ipv4'] + ':9090 no-fallback >/dev/null 2>&1; echo $?'
            )
            assert status != '0'
        finally:
            self.lab.remote('compute', 'sudo ip -n ' + anchor['namespace'] + ' link set ' + link + ' up', check=False)
            self.lab.remote('compute', 'sudo systemctl kill --kill-whom=main --signal=SIGCONT brocade-lab-agent')
        self.finish_capacity_routes()

    def finish_capacity_routes(self):
        item = self.instance('alpha')
        first, second = item['desired']['spec']['interfaces'][1:]
        boot = self.state['native']['alpha']['boot']
        self.network_ready()
        assert self.echo(text='recovered-selected-nic') == 'recovered-selected-nic'
        self.record(
            'actual selected carrier failure stays unreachable while healthy default still works; recovery preserves Guest boot'
        )
        item = self.instance('alpha')
        self.call(
            'POST',
            '/cvm/' + item['desired']['id'] + '/operations',
            {
                'action': 'detach',
                'interface_id': second['id'],
                'request_id': uuid.uuid4().hex,
                'expected_generation': item['desired']['generation'],
            },
            expected=409,
        )
        routes = item['desired']['spec']['routing']
        routes['routes'] = [r for r in routes['routes'] if r['interface_id'] != second['id']]
        routes['priorities'] = [r for r in routes['priorities'] if r['interface_id'] != second['id']]
        self.change('/routes', {'expected_revision': routes['revision'], 'routes': routes})
        self.operation('alpha', 'detach', interface_id=second['id'])
        self.complete(self.publish(), timeout=240)
        self.network_ready()
        assert len(self.instance('alpha')['report']['interfaces']) == 2
        assert self.guest('alpha', 'cat /proc/sys/kernel/random/boot_id') == boot
        self.record(
            'dependency-aware hot detach removes actual extra NIC after both endpoint receipts and retains original Guest identity'
        )

    def permissions(self):
        for user in ('alice', 'bob'):
            login = self.call('POST', '/users/platform/' + user + '/login', {}, expected=201)
            opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
            self.call('POST', '/auth/login', {'operator_id': user, 'password': login['password']}, opener=opener)
            if user == 'alice':
                self.call(
                    'POST',
                    '/deployments',
                    {'revision_id': self.revision(), 'idempotency_key': uuid.uuid4().hex},
                    expected=403,
                    opener=opener,
                )
                assert self.call('GET', '/cvm/' + self.state['instances']['alpha'] + '/artifacts', opener=opener)[
                    'current'
                ]
            else:
                for suffix in ('', '/artifacts', '/network', '/monitoring'):
                    self.call('GET', '/cvm/' + self.state['instances']['alpha'] + suffix, expected=403, opener=opener)
                assert self.call('GET', '/cvm', opener=opener)['instances'] == []
        self.record(
            'actual authenticated HTTP scopes preserve owner artifact access, deny other-owner access and reserve global publication for administrators'
        )

    def entries(self):
        provider = self.lab.state['roles']['provider']['ipv4']
        whole = '198.51.100.211'
        self.lab.persist_routes('client', [(whole + '/32', provider)])
        spec = {
            'name': 'actual entry publication',
            'provider_node': 'compute-b',
            'addresses': [
                {'address': provider, 'kind': 'shared', 'port_start': 55080, 'port_end': 55081},
                {'address': whole, 'kind': 'dedicated'},
            ],
            'transports': [
                {
                    'kind': 'wireguard',
                    'listen_side': 'provider',
                    'endpoint': provider,
                    'port_start': 44000,
                    'port_end': 44050,
                    'mtu': 1400,
                }
            ],
            'grants': [{'tenant_id': 'platform', 'user_id': 'alice'}],
            'max_entries': 3,
            'wireguard_transport': 'udp',
        }
        publication = self.call(
            'POST',
            '/cvm/entry-publications',
            {'request_id': uuid.uuid4().hex, 'expected_revision': 0, 'spec': spec},
            expected=201,
        )
        entries = []
        for address, mode in [
            (
                provider,
                {
                    'kind': 'l4_range',
                    'protocol': 'tcp',
                    'listen_start': 55080,
                    'listen_end': 55080,
                    'target_start': 22,
                    'target_end': 22,
                },
            ),
            (
                provider,
                {
                    'kind': 'l4_range',
                    'protocol': 'udp',
                    'listen_start': 55081,
                    'listen_end': 55081,
                    'target_start': 18081,
                    'target_end': 18081,
                },
            ),
            (whole, {'kind': 'l3'}),
        ]:
            entries.append(
                self.call(
                    'POST',
                    '/cvm/entries',
                    {
                        'request_id': uuid.uuid4().hex,
                        'instance_id': self.state['instances']['alpha'],
                        'publication_id': publication['id'],
                        'address': address,
                        'mode': mode,
                    },
                    expected=201,
                )
            )
        self.state['entries'] = entries
        self.state['entry_publication'] = publication
        self.save()
        self.gate(hold_entries=['compute-b'])
        deployment = self.publish()
        self.state['entry_deployment'] = deployment
        self.save()
        self.finish_entries()

    def finish_entries(self):
        provider = self.lab.state['roles']['provider']['ipv4']
        whole = '198.51.100.211'
        deployment = self.state.get('entry_deployment', self.state['last_deployment'])
        self.wait(
            'actual provider entry receipt intercepted',
            lambda: (
                self.lab.remote(
                    'compute', 'sudo test -s ' + self.prefix + '/gate/compute-b-entry.json', check=False
                ).returncode
                == 0
            ),
        )
        assert self.detail(deployment)['status'] == 'running'
        self.wait(
            'entry target remains converging behind its real receipt gate',
            lambda: self.detail(deployment),
            lambda d: any(t['status'] == 'converging' for t in d['targets']),
        )
        captured = self.prefix + '/gate/compute-b-entry.json'
        proof = self.wait(
            'actual configured entry provider receipt held',
            lambda: json.loads(
                self.lab.python(
                    'compute',
                    'import json;v=json.load(open('
                    + repr(captured)
                    + '));r=v.get("rules",[]);t=v.get("tunnels",[]);print(json.dumps({"configured":bool(r) and all(x.get("configured") for x in r) and all(x.get("connected") for x in t),"generation":v.get("generation")}))',
                )
            ),
            lambda p: p['configured'],
        )
        self.private('held-configured-entry-proof.json', proof)
        self.record(
            'mixed release waits for actual entry-provider receipt after its runtime was configured',
            deployment=deployment,
        )
        self.gate(hold_entries=[])
        self.complete(deployment, timeout=240)
        self.wait(
            'all three actual entries active',
            lambda: self.call('GET', '/cvm/' + self.state['instances']['alpha'] + '/entries'),
            lambda e: len(e) == 3 and all(i['state'] == 'active' for i in e),
        )
        self.guest('alpha', 'nohup ~/probe udp-server 0.0.0.0:18081 >~/entry.log 2>&1 </dev/null &')
        for address, port in [(provider, 55080), (whole, 22)]:
            item = self.instance('alpha')
            words = self.prepare_guest_password(item, 'client') + [
                '-o',
                'StrictHostKeyChecking=accept-new',
                '-o',
                'UserKnownHostsFile=' + self.prefix + '/entry.known',
                '-o',
                'ConnectTimeout=5',
                '-p',
                str(port),
                'cube@' + address,
                'cat ~/acceptance',
            ]
            assert (
                self.lab.remote('client', 'sudo ' + shlex.join(words)).stdout.decode().strip()
                == self.state['native']['alpha']['marker']
            )
        for endpoint in (provider + ':55081', whole + ':18081'):
            assert (
                self.lab.remote(
                    'client', 'sudo ' + self.prefix + '/probe udp4 ' + endpoint + ' ' + 'u' * 4096
                ).stdout.decode()
                == 'u' * 4096
            )
        self.lab.remote('client', 'ping -c 2 -W 3 ' + whole)
        self.record(
            'actual independent client reaches cross-host L4 SSH, fragmented UDP and whole-IP SSH/UDP/ICMP over entry WG'
        )

    def withdraw_entries(self):
        instance_id = self.state['instances']['alpha']
        owned = {entry['id'] for entry in self.state.get('entries', [])}
        current = {
            entry['id']: entry for entry in self.call('GET', '/cvm/' + instance_id + '/entries') if entry['id'] in owned
        }
        if not current:
            self.state['entries'] = []
            self.save()
            return
        for entry in current.values():
            self.call(
                'DELETE',
                '/cvm/entries/' + entry['id'],
                {'request_id': uuid.uuid4().hex, 'expected_revision': entry['revision']},
                expected=202,
            )
        self.complete(self.publish(), timeout=240)
        self.wait(
            'actual entry cleanup',
            lambda: self.call('GET', '/cvm/' + instance_id + '/entries'),
            lambda entries: not any(entry['id'] in current for entry in entries),
        )
        self.state['entries'] = []
        self.save()
        self.record('entry withdrawal waits for both hosts and releases actual entry claims')

    def used(self):
        return int(self.sql('SELECT coalesce(sum(uploaded+downloaded),0)::bigint FROM cvm_nic_monthly_usage'))

    def burn(self, count=24):
        client = self.lab.state['roles']['client']['ipv4']
        # Real bounded payloads pass through the selected business NIC. Quota rejection is an
        # expected outcome, so a failed request cannot abort the remote shell loop.
        payload = 'q' * 32000
        script = (
            'i=0; while [ "$i" -lt '
            + str(count)
            + ' ]; do ~/probe tcp4 '
            + client
            + ':9090 '
            + payload
            + ' >/dev/null 2>&1 || break; i=$((i+1)); done; echo generated'
        )
        assert self.guest('alpha', script, timeout=150) == 'generated'

    def traffic_quotas(self):
        before = self.used()
        self.burn()
        self.wait(
            'real measured traffic exceeds approved monthly allowance',
            lambda: self.instance('alpha'),
            lambda i: i['desired']['spec']['interfaces'][1]['allocated']['quota_exceeded'],
        )
        self.wait('kernel quota state applied', lambda: self.instance('alpha'), lambda i: i['status'] == 'applied')
        assert self.used() > before
        assert (
            self.guest(
                'alpha',
                '~/probe tcp4 '
                + self.lab.state['roles']['client']['ipv4']
                + ':9090 should-block >/dev/null 2>&1; echo $?',
            )
            != '0'
        )
        self.record(
            'real business traffic enters monthly ledger once and both enforced NIC policies block after allowance'
        )
        publication = self.state['publication']
        spec = publication['spec']
        spec['limits']['monthly_bytes'] = 4 * 1024**2
        publication = self.call(
            'PUT',
            '/cvm/networks/' + publication['id'],
            {'request_id': uuid.uuid4().hex, 'expected_revision': publication['revision'], 'spec': spec},
        )
        self.state['publication'] = publication
        self.save()
        item = self.instance('alpha')
        self.call(
            'PUT',
            '/cvm/' + item['desired']['id'] + '/interface-versions',
            {
                'request_id': uuid.uuid4().hex,
                'expected_generation': item['desired']['generation'],
                'updates': [
                    {
                        'interface_id': self.state['nic']['id'],
                        'publication_revision': publication['revision'],
                        'reallocate': False,
                    }
                ],
            },
            expected=202,
        )
        revision = self.revision()
        serving = next(i for i in self.desired('compute-a')['instances'] if i['id'] == self.state['instances']['alpha'])
        assert serving['spec']['interfaces'][1]['allocated']['limits']['monthly_bytes'] == 512 * 1024
        assert serving['spec']['interfaces'][1]['allocated']['quota_exceeded']
        assert (
            self.instance('alpha')['desired']['spec']['interfaces'][1]['allocated']['limits']['monthly_bytes']
            == 4 * 1024**2
        )
        self.record(
            'unpublished NIC version keeps approved old limit and blocked serving policy despite larger staged limit'
        )
        self.complete(self.publish(), timeout=240)
        self.network_ready()
        assert self.echo(text='approved-new-limit') == 'approved-new-limit'
        assert self.revision() == revision
        self.record('global publication alone activates new NIC version allowance and restores actual traffic')
        self.burn(count=100)
        self.wait(
            'larger real allowance exhausted',
            lambda: self.instance('alpha'),
            lambda i: i['desired']['spec']['interfaces'][1]['allocated']['quota_exceeded'],
        )
        self.instance_ready('alpha')
        self.finish_traffic_quotas()

    def finish_traffic_quotas(self):
        self.operation('alpha', 'stop')
        revision = self.revision()
        before_pending = self.instance('alpha')['desired']['generation']
        # Actual isolated OS/PostgreSQL/Console/Agent clocks cross a UTC month boundary. No SQL
        # ledger rewriting and no physical host clock changes are involved.
        code = 'import datetime,subprocess;d=datetime.datetime.now(datetime.timezone.utc);y=d.year+(d.month==12);m=1 if d.month==12 else d.month+1;t=datetime.datetime(y,m,1,0,1,tzinfo=datetime.timezone.utc);subprocess.run(["date","-u","-s",t.isoformat()],check=True)'
        for role in ('compute', 'provider', 'client'):
            self.lab.python(role, code)
        self.cookies.clear()
        self.call('POST', '/auth/login', {'operator_id': self.creds['operator_id'], 'password': self.creds['password']})
        item = self.wait(
            'real new month refreshes unpublished stop branch',
            lambda: self.instance('alpha'),
            lambda i: not i['desired']['spec']['interfaces'][1]['allocated']['quota_exceeded'],
        )
        serving = next(i for i in self.desired('compute-a')['instances'] if i['id'] == item['desired']['id'])
        assert serving['power'] == 'running' and not serving['spec']['interfaces'][1]['allocated']['quota_exceeded']
        assert item['desired']['power'] == 'stopped'
        assert item['desired']['generation'] > serving['generation'] > before_pending
        assert self.revision() == revision
        self.wait(
            'new month actual traffic restored behind unpublished Stop',
            lambda: self.echo(text='new-month'),
            lambda x: x == 'new-month',
        )
        self.record(
            'actual UTC month boundary unblocks serving traffic without applying staged Stop; pending and serving generations stay monotonic at same R'
        )
        self.complete(self.publish(), timeout=240)
        assert self.instance('alpha')['report']['power'] == 'stopped'
        self.operation('alpha', 'start')
        self.complete(self.publish(), timeout=240)
        assert self.echo(text='start-after-month') == 'start-after-month'
        self.record(
            'later approval executes previously staged Stop, and Start restores the same actual network identity'
        )

    def transport_failure(self):
        self.lab.remote(
            'provider',
            'sudo nft add table inet bckvm_fault && sudo nft "add chain inet bckvm_fault prerouting { type filter hook prerouting priority -300; policy accept; }" && sudo nft add rule inet bckvm_fault prerouting iifname lab0 tcp dport 43000-43100 counter drop',
        )
        item = self.instance('alpha')
        self.call(
            'PUT',
            '/cvm/' + item['desired']['id'] + '/interfaces/' + self.state['nic']['id'] + '/transport',
            {'request_id': uuid.uuid4().hex, 'expected_generation': item['desired']['generation'], 'transport': 'xray'},
            expected=202,
        )
        deployment = self.publish()
        terminal = self.wait(
            'real broken Xray transport triggers fallback and release failure',
            lambda: self.detail(deployment),
            lambda d: d['status'] in ('halted', 'succeeded'),
            timeout=240,
        )
        assert terminal['status'] == 'halted', 'the injected fault must prevent the requested transport from succeeding'
        rules = json.loads(self.lab.remote('provider', 'sudo nft -j list chain inet bckvm_fault prerouting').stdout)
        dropped = sum(
            expression.get('counter', {}).get('packets', 0)
            for item in rules['nftables']
            for expression in item.get('rule', {}).get('expr', [])
        )
        assert dropped > 0, 'the owned firewall must have observed and dropped actual Xray connection attempts'
        self.wait(
            'real WG fallback identity',
            lambda: self.instance('alpha'),
            lambda i: i['desired']['spec']['interfaces'][1]['allocated']['transport'] == 'wireguard',
        )
        self.network_ready()
        assert self.echo(text='actual-fallback') == 'actual-fallback'
        self.record(
            'real Xray port fault restores prior WG data path while original requested release stays failed',
            deployment=deployment,
            dropped_connection_packets=dropped,
        )
        self.burn(count=150)
        self.wait(
            'quota execution after physical transport fallback',
            lambda: self.instance('alpha'),
            lambda i: i['desired']['spec']['interfaces'][1]['allocated']['quota_exceeded'],
        )
        assert self.detail(deployment)['status'] == 'halted'
        self.call('POST', f'/deployments/{deployment}/targets/compute-a/retry', {}, expected=409)
        self.record(
            'later actual traffic-quota execution cannot erase failed transport history or enable generic retry'
        )
        self.call('POST', f'/deployments/{deployment}/cancel', {}, expected=(200, 202))
        self.lab.remote('provider', 'sudo nft delete table inet bckvm_fault')
        publication = self.state['publication']
        spec = publication['spec']
        spec['limits']['monthly_bytes'] = 64 * 1024**2
        publication = self.call(
            'PUT',
            '/cvm/networks/' + publication['id'],
            {'request_id': uuid.uuid4().hex, 'expected_revision': publication['revision'], 'spec': spec},
        )
        self.state['publication'] = publication
        self.save()
        self.change(
            '/interface-versions',
            {
                'updates': [
                    {
                        'interface_id': self.state['nic']['id'],
                        'publication_revision': publication['revision'],
                        'reallocate': False,
                    }
                ]
            },
        )
        self.network_ready()
        assert self.echo(text='compensating-revision') == 'compensating-revision'
        self.record('new compensating global revision converges after fallback and real traffic returns')

    def delete_and_empty(self):
        # Leave a measured two-MiB allowance before staging deletion. The held final sample
        # below must actually cross this limit after the Guest has already been removed.
        used = int(
            self.sql(
                "SELECT coalesce(sum(uploaded+downloaded),0)::bigint FROM cvm_nic_monthly_usage WHERE interface_id='"
                + self.state['nic']['id']
                + "' AND period=date_trunc('month',now() AT TIME ZONE 'UTC')::date"
            )
        )
        publication = self.state['publication']
        spec = publication['spec']
        spec['limits']['monthly_bytes'] = used + 2 * 1024**2
        publication = self.call(
            'PUT',
            '/cvm/networks/' + publication['id'],
            {'request_id': uuid.uuid4().hex, 'expected_revision': publication['revision'], 'spec': spec},
        )
        self.state['publication'] = publication
        self.save()
        self.change(
            '/interface-versions',
            {
                'updates': [
                    {
                        'interface_id': self.state['nic']['id'],
                        'publication_revision': publication['revision'],
                        'reallocate': False,
                    }
                ]
            },
        )
        self.network_ready()
        # Stage deletion only, then prove the original Guest continues serving and metering.
        self.operation('alpha', 'delete', delete_data=True)
        revision = self.revision()
        before = self.used()
        assert self.echo(text='pending-delete-still-serves') == 'pending-delete-still-serves'
        self.burn(count=12)
        self.wait('unpublished Delete still meters serving business binding', self.used, lambda total: total > before)
        assert self.instance('alpha')['desired']['power'] == 'deleted'
        assert (
            next(i for i in self.desired('compute-a')['instances'] if i['id'] == self.state['instances']['alpha'])[
                'power'
            ]
            == 'running'
        )
        self.record(
            'pending Delete preserves actual running Guest, business data and measured usage without publishing intent'
        )
        self.gate(hold_nics=['compute-a'])
        # This data is genuine and not yet accepted by Console. The final withdrawal receipt
        # must carry the cumulative tail without resurrecting the deleted execution.
        self.burn(count=48)
        deployment = self.publish()
        terminal = self.wait(
            'actual Guest is physically deleted before final network receipt',
            lambda: self.instance('alpha'),
            lambda i: bool(i['report']) and i['report']['power'] == 'deleted' and i['report']['volume_deleted'],
            timeout=240,
        )
        generation = terminal['desired']['generation']
        path = self.prefix + '/gate/compute-a-nic-withdrawing.json'
        self.wait(
            'real final cumulative NIC withdrawal receipt captured',
            lambda: self.lab.remote('compute', 'sudo test -s ' + path, check=False).returncode == 0,
        )
        replay = r"""import hashlib,json,pathlib,urllib.request
root=pathlib.Path(ROOT);data=(root/'gate/compute-a-nic-withdrawing.json').read_bytes();token=(root/'node.token').read_text().strip()
request=urllib.request.Request('http://127.0.0.1:8080/agent/v1/cvm/nics/report',method='POST',data=data,headers={'Authorization':'Bearer '+token,'Content-Type':'application/json','x-brocade-protocol-version':CURRENT_PROTOCOL})
with urllib.request.urlopen(request,timeout=30) as response:result=json.load(response)
print(json.dumps({'sha256':hashlib.sha256(data).hexdigest(),'bytes':len(data),'response':result,'traffic_samples':[b['traffic'] for b in json.loads(data)['bindings'] if b.get('traffic')]}))
""".replace('ROOT', repr(self.prefix)).replace('CURRENT_PROTOCOL', repr(AGENT_PROTOCOL_VERSION))
        result = json.loads(self.lab.python('compute', replay))
        assert result['traffic_samples']
        self.private('actual-final-receipt-replay.json', result)
        after = self.instance('alpha')
        assert after['desired']['generation'] == generation and after['status'] == 'applied'
        final_used = int(
            self.sql(
                "SELECT coalesce(sum(uploaded+downloaded),0)::bigint FROM cvm_nic_monthly_usage WHERE interface_id='"
                + self.state['nic']['id']
                + "' AND period=date_trunc('month',now() AT TIME ZONE 'UTC')::date"
            )
        )
        assert final_used >= spec['limits']['monthly_bytes']
        assert self.revision() == revision
        self.record(
            'actual delayed cumulative withdrawal bytes are accepted after physical deletion without reopening generation or pending state',
            receipt_sha256=result['sha256'],
        )
        self.gate(hold_nics=[])
        detail = self.complete(deployment, timeout=240)
        self.wait(
            'instance and both endpoint claims released', lambda: self.instance('alpha'), lambda i: not i['reserved']
        )
        evidence = next(t for t in detail['targets'] if t['node_id'] == 'compute-a')['observed_after']['cvm'][
            'instances'
        ]
        assert any(i['id'] == self.state['instances']['alpha'] and i['status'] == 'applied' for i in evidence)
        self.record('global release retains terminal deleted-instance evidence after actual two-sided network cleanup')
        # The next release begins with an already empty network snapshot; its ACK is still
        # mandatory, and a real Agent must send it even when no binding changed.
        self.create('gamma', 'compute-a')
        next_deployment = self.publish()
        self.complete(next_deployment, timeout=240)
        item = self.instance_ready('gamma')
        assert item['report']['power'] == 'running'
        assert not self.call(
            'GET', '/agent/v1/cvm/nics/desired', authorization=self.state['nodes']['compute-a']['token']
        )['bindings']
        self.record(
            'already-empty real NIC snapshot receives fresh ACK and the next primary-only Guest release completes',
            deployment=next_deployment,
        )

    def terminal_runtime_proof(self, role):
        return json.loads(
            self.lab.python(
                role,
                r"""import glob,json,os,subprocess,urllib.request
from pathlib import Path
with urllib.request.urlopen('http://127.0.0.1:18080/v1/network/taps',timeout=5) as response:
 taps=json.load(response)['taps']
# Ready TAPs are intentionally retained for reuse, with no sandbox owner.
assert all(t['state']=='Ready' and not t.get('sandboxID') and not t.get('ownerSandboxID') and not t.get('networkHandle') and not t.get('portMappings') and not t.get('lastError') for t in taps)
active=[];vmfds=[]
for path in glob.glob('/proc/[0-9]*/cmdline'):
 try:
  executable=open(path,'rb').read().split(b'\0')[0]
  if os.path.basename(executable).startswith(b'containerd-shim-cube'):active.append(path)
 except OSError:pass
for path in glob.glob('/proc/[0-9]*/fd/*'):
 try:
  if 'kvm-vm' in os.readlink(path):vmfds.append(path)
 except OSError:pass
maps={}
for name in ['ifindex_to_mvmmeta','primary_chain_routes']:
 path='/sys/fs/bpf/'+name
 if Path(path).exists():
  maps[name]=json.loads(subprocess.check_output(['bpftool','-j','map','dump','pinned',path]))
  assert not maps[name]
assert 'ifindex_to_mvmmeta' in maps
links=[x['ifname'] for x in json.loads(subprocess.check_output(['ip','-j','link'])) if x['ifname'].startswith('bcg')]
tables=[x['table']['name'] for x in json.loads(subprocess.check_output(['nft','-j','list','tables']))['nftables'] if 'table' in x and x['table']['name'].startswith('brocade_chain_')]
assert not active and not vmfds and not links and not tables
print(json.dumps({'ready_pool_taps':taps,'runtime_processes':active,'kvm_vm_fds':vmfds,'instance_identity_maps':maps,'chain_devices':links,'chain_guards':tables}))
""",
            )
        )

    def cleanup(self):
        if self.state.get('entries'):
            self.withdraw_entries()
        for name in ('beta', 'gamma'):
            self.operation(name, 'delete', delete_data=True)
        deployment = self.publish()
        self.complete(deployment, timeout=300)
        for name in ('alpha', 'beta', 'gamma'):
            item = self.instance(name)
            assert item['report']['power'] == 'deleted' and item['report']['volume_deleted'] and not item['reserved']
        for role in ('compute', 'provider'):
            self.record(
                'actual Guest/runtime identity cleanup with ownerless Ready TAP pool on ' + role,
                **self.terminal_runtime_proof(role),
            )
        assert self.sql('SELECT count(*) FROM cvm_nic_allocations WHERE NOT released') == '0'
        assert self.sql('SELECT count(*) FROM cvm_nic_bindings WHERE NOT cleaned') == '0'
        self.record('all owned business NIC allocation and endpoint reservations are released')


def main():
    acceptance = Acceptance(sys.argv[1])
    getattr(acceptance, sys.argv[2])()


if __name__ == '__main__':
    main()
