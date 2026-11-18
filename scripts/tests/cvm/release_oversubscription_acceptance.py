#!/usr/bin/env python3
"""Real oversubscribed Cube Guests in release_kvm_lab's explicitly owned VMs."""

import json
import sys
import uuid
from release_kvm_acceptance import Acceptance

GIB = 1024**3


class Oversubscription(Acceptance):
    def live_resources(self, name):
        instance = self.instance(name)['desired']['id']
        code = (
            """import json,socket,http.client
from pathlib import Path
record=json.loads(Path('/data/cubelet/network-attachments-v2/'+%r+'.json').read_text())
socket_=socket.socket(socket.AF_UNIX);socket_.settimeout(5);socket_.connect('/run/vc/vm/'+record['runtime']+'/chapi')
client=http.client.HTTPConnection('localhost',timeout=5);client.sock=socket_;client.request('GET','/api/v1/vm.info')
response=client.getresponse();value=json.loads(response.read());assert response.status==200 and value['state']=='Running'
print(json.dumps({'vcpus':value['config']['cpus']['boot_vcpus'],'memory_bytes':value['config']['memory']['size']}))
"""
            % instance
        )
        return json.loads(self.lab.python('compute', code))

    def run(self):
        self.call(
            'PUT',
            '/cvm/quota',
            {
                'tenant_id': 'platform',
                'user_id': 'alice',
                'enabled': True,
                'allowed_nodes': list(self.state['nodes']),
                'max_instances': 4,
                'max_vcpus': 10,
                'max_memory_bytes': 10 * GIB,
                'max_disk_bytes': 28 * GIB,
                'max_interfaces': 8,
            },
            expected=204,
        )
        for name in ['over-a', 'over-b', 'over-c']:
            self.create(name, 'compute-a', {'vcpus': 3, 'memory_bytes': 3 * GIB, 'root_disk_bytes': 8 * GIB})
        self.complete(self.publish(), timeout=300)
        volumes = {}
        keys = {}
        for name in ['over-a', 'over-b', 'over-c']:
            item = self.instance_ready(name)
            assert self.live_resources(name) == {'vcpus': 3, 'memory_bytes': 3 * GIB}
            volumes[name] = item['report']['volume_id']
            assert int(self.guest(name, 'getconf _NPROCESSORS_ONLN')) == 3
            self.guest(name, 'printf owned-oversell > ~/oversell-proof')
            keys[name] = self.guest(name, 'cat /etc/ssh/ssh_host_ed25519_key.pub')
        proof = json.loads(
            self.lab.python(
                'compute',
                """import json,os
from pathlib import Path
root=Path('/data/cubelet/storage/plain/objects/volumes')
files=[p for p in root.glob('*/*') if p.is_file() and p.name==p.parent.name]
stat=os.statvfs(root)
logical=sum(p.stat().st_size for p in files);physical=sum(p.stat().st_blocks*512 for p in files)
total=stat.f_blocks*stat.f_frsize
assert len(files)==3 and logical>total and physical<total
print(json.dumps({'logical_disk_bytes':logical,'physical_file_bytes':physical,'filesystem_bytes':total,'volumes':len(files),'host_cpus':os.cpu_count()}))
""",
            )
        )
        assert proof['host_cpus'] < 9
        host = next(h for h in self.call('GET', '/cvm')['hosts'] if h['node_id'] == 'compute-a')
        assert host['capabilities']['allocatable_memory_bytes'] < 9 * GIB
        self.record(
            'three real 3-vCPU/3GiB/8GiB Guests run on four physical CPUs, below 9GiB memory baseline and a 20GiB filesystem; logical 24GiB disks remain sparse',
            **proof,
        )
        item = self.instance('over-a')
        spec = item['desired']['spec']
        self.call(
            'POST',
            '/cvm',
            {
                'request_id': uuid.uuid4().hex,
                'tenant_id': 'platform',
                'user_id': 'alice',
                'node_id': 'compute-a',
                'name': 'disk-denied',
                'image_id': spec['image']['id'],
                'resources': {'vcpus': 1, 'memory_bytes': 128 * 1024**2, 'root_disk_bytes': 8 * GIB},
                'ssh_public_key': spec['ssh_public_key'],
                'interfaces': [
                    {
                        'source_id': spec['interfaces'][0]['source_id'],
                        'purpose': 'contact',
                        'default_ipv4': False,
                        'default_ipv6': False,
                    }
                ],
            },
            expected=400,
        )
        self.operation('over-a', 'stop')
        self.complete(self.publish(), timeout=240)
        item = self.instance_ready('over-a')
        body = {
            'request_id': uuid.uuid4().hex,
            'expected_generation': item['desired']['generation'],
            'action': 'configure',
            'name': 'over-a',
            'vcpus': 5,
            'memory_bytes': 3 * GIB,
        }
        self.call('POST', '/cvm/' + item['desired']['id'] + '/operations', body, expected=400)
        self.operation('over-a', 'configure', name='over-a-resized', vcpus=4, memory_bytes=4 * GIB)
        self.complete(self.publish(), timeout=240)
        self.operation('over-a', 'start')
        self.complete(self.publish(), timeout=240)
        self.instance_ready('over-a')
        assert int(self.guest('over-a', 'getconf _NPROCESSORS_ONLN')) == 4
        assert self.live_resources('over-a') == {'vcpus': 4, 'memory_bytes': 4 * GIB}
        assert self.guest('over-a', 'cat ~/oversell-proof') == 'owned-oversell'
        assert self.guest('over-a', 'cat /etc/ssh/ssh_host_ed25519_key.pub') == keys['over-a']
        assert self.instance('over-a')['report']['volume_id'] == volumes['over-a']
        self.operation('over-b', 'stop')
        self.complete(self.publish(), timeout=240)
        item = self.instance_ready('over-b')
        body = {
            'request_id': uuid.uuid4().hex,
            'expected_generation': item['desired']['generation'],
            'action': 'configure',
            'name': 'over-b',
            'vcpus': 3,
            'memory_bytes': 4 * GIB,
        }
        self.call('POST', '/cvm/' + item['desired']['id'] + '/operations', body, expected=400)
        self.operation('over-b', 'start')
        self.complete(self.publish(), timeout=240)
        self.instance_ready('over-b')
        self.record(
            'real stopped resize oversells CPU/RAM and preserves original sparse disk, data and SSH identity; user CPU, RAM and disk quotas still reject excess'
        )
        for name in ['over-a', 'over-b', 'over-c']:
            self.operation(name, 'delete', delete_data=True)
        self.complete(self.publish(), timeout=300)
        for name in ['over-a', 'over-b', 'over-c']:
            item = self.instance(name)
            assert not item['reserved'] and item['report']['volume_deleted']
        self.record(
            'actual oversubscribed Guests and thin disks deleted with normal global publication',
            **self.terminal_runtime_proof('compute'),
        )


if __name__ == '__main__':
    Oversubscription(sys.argv[1]).run()
