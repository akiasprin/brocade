#!/usr/bin/env python3
"""Accept remote Cube first install and cold Stack B using the owned KVM release lab."""

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
import uuid

from release_kvm_acceptance import Acceptance
from release_proxy import STACK_BUNDLE

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
import cvm_stack


API = '/cvm/stack-releases'
NODE = 'compute-a'
BASE = '/opt/brocade/cvm'
STATE = '/var/lib/brocade/cvm-cube'
STORAGE = '/data/cubelet/storage'
OWNER = 'stack-lab-owner'
CUBELET_UNIT = 'brocade-cvm-cubelet.service'
AGENT_UNIT = 'brocade-agent.service'
AGENT_BIN = '/usr/local/bin/brocade-agent'
MANAGER = '/usr/local/lib/brocade/cvm-stack.py'
COLD_PAIR = 'brocade-cvm-stack-cold-pair-v1'
MAINTENANCE_WINDOW_SECONDS = 5 * 60 * 60
DROPIN = '/etc/systemd/system/brocade-cvm-cubelet.service.d/99-brocade-stack-acceptance.conf'
SCENARIOS = ('bad-hash', 'bad-size', 'cancel', 'activation-failure', 'manager-death', 'success', 'rollback')
TERMINAL = {'failed', 'recovered', 'blocked', 'succeeded', 'cancelled'}
DIGEST = re.compile(r'[a-f0-9]{64}')
IDENTITY = re.compile(r'[a-zA-Z0-9_.-]{1,96}')


class AcceptanceError(RuntimeError):
    pass


def require(condition, message):
    if not condition:
        raise AcceptanceError(message)


def private_directory(path):
    info = path.lstat()
    require(
        stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077,
        'expected a private, owned directory',
    )


def atom_json(path, value):
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False, mode='w') as stream:
        temporary = Path(stream.name)
        json.dump(value, stream, indent=2)
    temporary.replace(path)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def openssl(*args):
    return subprocess.run(['openssl', *args], check=True, capture_output=True, timeout=30).stdout


def sign_pair(directory, key, payload):
    wire = json.dumps(payload, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode('utf-8')
    pair_id = hashlib.sha256(wire).hexdigest()
    with tempfile.NamedTemporaryFile(dir=directory) as stream:
        stream.write(wire)
        stream.flush()
        sig = openssl('pkeyutl', '-sign', '-rawin', '-inkey', str(key), '-in', stream.name)
    require(len(sig) == 64, 'OpenSSL did not produce an Ed25519 signature')
    atom_json(directory / (pair_id + '.json'), {'payload': wire.decode('utf-8'), 'signature': sig.hex()})
    return pair_id


def empty_observation(view):
    for host in view['hosts']:
        observed_at = host.get('observed_at')
        if (
            set(host) == {'node_id', 'observed_at', 'installable'}
            and host['node_id'] == NODE
            and host['installable'] == 'plain'
            and type(observed_at) is int
            and observed_at > 0
        ):
            return host
    return None


def installed(manifest, sha):
    return {
        'bundle_sha256': sha,
        'cubelet_sha256': manifest['files'][cvm_stack.CUBELET]['sha256'],
        'shim_sha256': manifest['files'][cvm_stack.SHIM]['sha256'],
        'storage_backend': 'plain',
    }


@contextmanager
def step(name):
    start = datetime.now(timezone.utc)
    mono = time.monotonic()
    print(f'START {name}: {start.isoformat(timespec="seconds")}', flush=True)
    finished = threading.Event()

    def progress():
        while not finished.wait(30):
            print(f'WAIT {name}: {time.monotonic() - mono:.1f}s', flush=True)

    reporter = threading.Thread(target=progress, daemon=True)
    reporter.start()
    result = 'FAIL'
    try:
        yield
        result = 'PASS'
    finally:
        finished.set()
        reporter.join()
        end = datetime.now(timezone.utc)
        print(
            f'{result} {name}: {start.isoformat(timespec="seconds")} to {end.isoformat(timespec="seconds")}; '
            f'{time.monotonic() - mono:.1f}s',
            flush=True,
        )


def make_catalog(args):
    pairs = {}
    manifests = []
    executables = []
    shims = []
    for path, sha in ((args.source, args.source_sha256), (args.target, args.target_sha256)):
        require(DIGEST.fullmatch(sha), 'each bundle needs an explicit lowercase SHA-256 pin')
        require(0 < path.stat().st_size <= cvm_stack.STACK_BUNDLE_MAX_BYTES, 'bundle outside the Stack B size limit')
        # The production verifier checks every archive member without extracting/executing input code.
        manifest = cvm_stack.inspect_archive(path, sha)
        require(manifest['architecture'] == 'x86_64' and 'plain' in manifest['storage_backends'], 'not x86_64/plain')
        manifests.append(manifest)
        executables.append(installed(manifest, sha))
        shims.append(manifest['files']['runtime/cube-shim/version']['sha256'])
    a, b = executables
    require(manifests[0]['state_abi'] == manifests[1]['state_abi'], 'bundle persistent-state ABI differs')
    require(shims[0] != shims[1], 'distinct immutable Shim versions are required')
    require(
        all(a[field] != b[field] for field in ('bundle_sha256', 'cubelet_sha256', 'shim_sha256')),
        'use two genuine bundles with distinct Cubelet and Shim bytes',
    )
    out = args.output.resolve()
    out.mkdir(mode=0o700, exist_ok=False)
    with tempfile.TemporaryDirectory(dir=out) as temporary:
        key = Path(temporary) / 'signing.key'
        openssl('genpkey', '-algorithm', 'ED25519', '-out', str(key))
        der = openssl('pkey', '-in', str(key), '-pubout', '-outform', 'DER')
        require(der[:12] == bytes.fromhex('302a300506032b6570032100') and len(der) == 44, 'invalid Ed25519 SPKI')
        public = der[12:].hex()
        for label, source, target, archive in (('forward', a, b, args.target), ('reverse', b, a, args.source)):
            bundle = out / (target['bundle_sha256'] + '.tar.gz')
            shutil.copyfile(archive, bundle)
            bundle.chmod(0o600)
            require(digest(bundle) == target['bundle_sha256'], 'bundle changed while copying')
            payload = {
                'contract': COLD_PAIR,
                'architecture': 'x86_64',
                'storage_backend': 'plain',
                'target_bytes': bundle.stat().st_size,
                **{
                    prefix + '_' + key: value[key]
                    for prefix, value in (('source', source), ('target', target))
                    for key in ('cubelet_sha256', 'shim_sha256')
                },
                'source_sha256': source['bundle_sha256'],
                'target_sha256': target['bundle_sha256'],
            }
            pairs[label] = sign_pair(out, key, payload)
            if label == 'reverse':
                # First install targets the same verified A bytes, but authorizes no source.
                fresh = {key: value for key, value in payload.items() if not key.startswith('source_')}
                pairs['first-install'] = sign_pair(out, key, {**fresh, 'contract': cvm_stack.FRESH_INSTALL_CONTRACT})
    atom_json(out / 'lab-catalog.json', {'public': public, 'source': a, 'target': b, 'pairs': pairs})


# All mutations occur in the verified compute VM. Preserve the existing ext4
# loop image, do not reformat or mount over a different storage object.
PREPARE = r"""
import json,os,re,stat,subprocess,sys
from pathlib import Path
p,task,pub,fresh = json.load(sys.stdin)
base=Path(p)
def run(*args):
 return subprocess.check_output(args,text=True,stderr=subprocess.DEVNULL,timeout=60).strip()
def put(path,text,mode=0o644):
 with Path(path).open('x') as stream:
  os.fchmod(stream.fileno(),mode)
  stream.write(text)
assert Path('/var/lib/cloud/data/instance-id').read_text().strip() == task+'compute'
units=Path('/etc/systemd/system')
old=units/'brocade-lab-agent.service'; new=units/'brocade-agent.service'
old_dir=base/'runtime'; new_dir=Path('/var/lib/brocade/cvm-cube')
key=Path('/etc/brocade/cvm-stack-update.pub')
owner=base/'stack-lab-owner'
mount=units/'data-cubelet-storage.mount'
assert not any(os.path.lexists(x) for x in (new_dir,key,owner,mount))
selected=new if fresh else old
other=old if fresh else new
assert not os.path.lexists(other)
assert stat.S_ISREG(selected.lstat().st_mode) and selected.stat().st_uid==0
config=selected.read_text()
assert 'ExecStart=/usr/local/bin/brocade-agent run\n' in config
assert f'Environment="BROCADE_NODE_TOKEN_FILE={p}/node.token"\n' in config
assert run('systemctl','show','-p','FragmentPath','--value',selected.name) == str(selected)
assert run('systemctl','show','-p','FragmentPath','--value',other.name) == ''
if fresh:
 assert 'BROCADE_CVM_' not in config and not os.path.lexists(old_dir)
else:
 assert config.count('Environment="BROCADE_CVM_STATE_DIR='+str(old_dir)+'"\n') == 1
assert run('findmnt','-n','-o','FSTYPE','-M','/data/cubelet/storage') == 'ext4'
loop=run('findmnt','-n','-o','SOURCE','-M','/data/cubelet/storage')
assert re.fullmatch('/dev/loop[0-9]+',loop)
image=base/'storage.img'
assert stat.S_ISREG(image.lstat().st_mode) and image.stat().st_uid==0
assert run('losetup','--list','--noheadings','--output','BACK-FILE',loop) == str(image)
put(mount,'[Unit]\nDescription=Owned CVM release lab ext4 image\n[Mount]\nWhat='+str(image)+'\nWhere=/data/cubelet/storage\nType=ext4\nOptions=loop\n[Install]\nWantedBy=local-fs.target\n')
if not fresh:
 run('systemctl','stop','brocade-lab-agent.service')
 if old_dir.exists():
  assert old_dir.is_dir() and not old_dir.is_symlink() and old_dir.stat().st_uid==0
  old_dir.rename(new_dir)
 config=config.replace('Environment="BROCADE_CVM_STATE_DIR='+str(old_dir)+'"\n','')
 put(new, config+'\n[Install]\nWantedBy=multi-user.target\n')
 old.rename(base/'original-lab-agent.service')
key.parent.mkdir(mode=0o755,exist_ok=True)
put(key,pub+'\n')
console=units/'brocade-lab-console.service.d'; console.mkdir(exist_ok=True)
put(console/'99-brocade-stack-acceptance.conf','[Service]\nEnvironment=BROCADE_CVM_STACK_CATALOG_DIR='+p+'/stack-catalog\nEnvironment=BROCADE_CVM_STACK_PUBLIC_KEY='+pub+'\n')
run('systemctl','daemon-reload')
run('systemctl','enable','data-cubelet-storage.mount','brocade-agent.service','brocade-lab-console.service')
run('systemctl','add-wants','multi-user.target','brocade-lab-gate.service')
run('systemctl','restart','brocade-lab-gate.service','brocade-lab-console.service')
run('systemctl','start','brocade-agent.service')
put(owner,task+'\n',0o600)
"""

# Check the actual storage and the running canonical Agent, not just an API shape.
EMPTY_HOST_PROOF = r"""
import json,os,stat,subprocess,sys
from pathlib import Path
prefix,base,state,storage,manager,agent,unit,initial,claim_id=json.load(sys.stdin)
def run(*args):
 return subprocess.check_output(args,text=True,stderr=subprocess.DEVNULL,timeout=10).strip()
absent=[Path(base)/name for name in ('current','installation.json','stack-install.json')]
marker=Path(base)/'stack-maintenance'
if claim_id is None:
 absent.append(marker)
else:
 job=json.loads(marker.read_text())
 assert job['offer']['id']==claim_id and job['offer']['status']=='ready' and job['phase']=='claiming'
absent += [Path(state),Path('/etc/brocade/cvm-cube.json'),Path('/etc/systemd/system/brocade-cvm-cubelet.service')]
if initial:
 absent += [Path(manager),Path('/etc/brocade/cvm-stack-update.pub')]
assert not any(os.path.lexists(path) for path in absent)
assert not run('systemctl','show','-p','FragmentPath','--value','brocade-cvm-cubelet.service')
assert run('systemctl','show','-p','FragmentPath','--value',unit)=='/etc/systemd/system/'+unit
pid=int(run('systemctl','show','-p','MainPID','--value',unit))
assert pid>1
assert os.readlink(f'/proc/{pid}/exe')==agent
assert Path(f'/proc/{pid}/cmdline').read_bytes().split(b'\0')==[agent.encode(),b'run',b'']
assert run('findmnt','-n','-o','FSTYPE','-M',storage)=='ext4'
loop=run('findmnt','-n','-o','SOURCE','-M',storage)
assert run('losetup','--list','--noheadings','--output','BACK-FILE',loop)==prefix+'/storage.img'
filesystem=run('findmnt','-n','-o','UUID','-M',storage)
assert filesystem
if not initial:
 assert run('systemctl','is-enabled','data-cubelet-storage.mount')=='enabled'
for path in Path(storage).iterdir():
 assert path.name=='lost+found' and stat.S_ISDIR(path.lstat().st_mode) and not any(path.iterdir())
print(json.dumps({'filesystem_uuid':filesystem,'agent_pid':pid}))
"""


# Resolve the physical object through the Agent's durable record for the
# specific instance (not a glob over unrelated disks or a reported volume ID).
ROOT_PROOF = r"""
import hashlib,json,re,stat,sys
from pathlib import Path
state,storage,instance,reported_runtime,volume=json.load(sys.stdin)
record=json.loads((Path(state)/(instance+'.json')).read_text())
runtime=record['runtime']
assert record['instance']==instance and record['volume']==volume
assert reported_runtime is None or runtime==reported_runtime
assert re.fullmatch(r'[a-zA-Z0-9_.-]{1,96}',runtime)
obj,=record['objects']; name=obj['name']
assert re.fullmatch(r'[a-zA-Z0-9_.-]{1,96}',obj['role'])
assert obj['kind']=='volume' and name=='sb-'+runtime+'-'+obj['role']
assert volume=='cube-'+hashlib.sha256(name.encode()).hexdigest()
path=Path(storage)/'plain/objects/volumes'/name/name
st=path.lstat()
assert stat.S_ISREG(st.st_mode) and path.resolve(strict=True)==path and st.st_size>0
print(json.dumps({'path':str(path),'device':st.st_dev,'inode':st.st_ino,'size':st.st_size,'volume':volume,'runtime':runtime}))
"""

RUNTIME_PROOF = r"""
import hashlib,json,os,subprocess,sys
from pathlib import Path
base,unit,runtime=json.load(sys.stdin)
def binary(pid):
 exe=Path('/proc')/str(pid)/'exe'
 with exe.open('rb') as stream:
  return {'pid':pid,'exe':os.readlink(exe),'sha256':hashlib.file_digest(stream,'sha256').hexdigest()}
pid=int(subprocess.check_output(['systemctl','show','-p','MainPID','--value',unit],timeout=5))
assert pid>1
current=(Path(base)/'current').resolve(strict=True)
assert current.parent==Path(base)/'releases'
value={'bundle_sha256':current.name,'cubelet':binary(pid)}
if runtime:
 matches=[]
 for proc in Path('/proc').glob('[0-9]*'):
  try:
   args=(proc/'cmdline').read_bytes().split(b'\0')
   if not any(arg==runtime.encode() or arg.endswith(b'='+runtime.encode()) for arg in args):continue
   if not os.readlink(proc/'exe').endswith('/containerd-shim-cube-rs'):continue
   if not any('kvm-vm' in os.readlink(fd) for fd in (proc/'fd').iterdir() if os.path.lexists(fd)):continue
   matches.append(binary(int(proc.name)))
  except (FileNotFoundError,ProcessLookupError):pass
 value['shim'],=matches
print(json.dumps(value))
"""

CANDIDATE_HOOK = r"""
import json,sys,time
from pathlib import Path
root=Path(sys.argv[1]); base=Path('/opt/brocade/cvm')
control=json.loads((root/'candidate.json').read_text())
if (base/'current').resolve().name != control['target']:sys.exit(0)
assert (root/'stack-lab-owner').read_text().strip()==control['task']
checkpoint=json.loads((base/'stack-upgrade.json').read_text())
job=json.loads((base/'stack-maintenance').read_text())
assert job['offer']['id']==control['id'] and job['phase']=='started'
assert checkpoint['phase']=='switching' and checkpoint['target']['bundle_sha256']==control['target']
(root/'candidate-entered').write_text(str(control['id'])+'\n')
if control['mode']=='activation-failure':sys.exit(1)
if control['mode']!='manager-death':sys.exit(2)
end=time.monotonic()+45
while time.monotonic()<end:
 if (root/'candidate-resume').exists():sys.exit(0)
 time.sleep(0.1)
sys.exit(3)
"""


class StackAcceptance:
    def __init__(self, output):
        self.output = output.resolve()
        private_directory(self.output)
        private_directory(self.output / 'lab-3')
        self.acceptance = Acceptance(self.output)
        self.lab = self.acceptance.lab
        self.task = self.lab.state['task']
        require(re.fullmatch(r'[a-f0-9]{10}', self.task), 'invalid KVM lab identity')
        self.prefix = '/var/lib/brocade/acceptance-' + self.task
        require(self.acceptance.prefix == self.prefix, 'KVM lab deployment ownership mismatch')
        self.evidence = self.output / 'stack-acceptance'
        self.evidence.mkdir(mode=0o700, exist_ok=True)
        private_directory(self.evidence)
        self.report_path = self.evidence / 'state.json'
        self.state = json.loads(self.report_path.read_text()) if self.report_path.exists() else None

    def check_lab(self):
        self.lab.verify_owned()
        nodes = self.acceptance.state['nodes']
        require(nodes.get(NODE, {}).get('role') == 'compute', 'compute node is not enrolled in this lab')

    def remote(self, *words, data=None, timeout=90, check=True):
        result = self.lab.remote('compute', 'sudo ' + shlex.join(words), data=data, timeout=timeout, check=False)
        if check:
            require(result.returncode == 0, 'owned VM command failed: ' + ' '.join(words[:2]))
        return result

    def remote_json(self, code, value):
        return json.loads(self.remote('/usr/bin/python3', '-I', '-c', code, data=json.dumps(value).encode()).stdout)

    def remote_write(self, path, content):
        code = (
            'import os,sys;from pathlib import Path;p=Path(sys.argv[1]);'
            'p.parent.mkdir(mode=0o700,exist_ok=True);f=p.open("xb");'
            'os.fchmod(f.fileno(),0o600);f.write(sys.stdin.buffer.read());f.close()'
        )
        self.remote('/usr/bin/python3', '-I', '-c', code, path, data=content)

    def save(self):
        atom_json(self.report_path, self.state)

    def view(self):
        return self.acceptance.call('GET', API)

    def login(self):
        self.acceptance.call(
            'POST',
            '/auth/login',
            {key: self.acceptance.creds[key] for key in ('operator_id', 'password')},
        )

    def wait(self, description, probe, predicate=bool, timeout=300):
        return self.acceptance.wait(description, probe, predicate, timeout)

    def host(self, expected):
        def match(view):
            return next(
                (
                    host
                    for host in view['hosts']
                    if host['node_id'] == NODE and all(host.get(key) == value for key, value in expected.items())
                ),
                None,
            )

        return self.wait('real Agent reports installed stack identity', lambda: match(self.view()))

    def guest_host_inventory(self):
        return next((h for h in self.acceptance.call('GET', '/cvm')['hosts'] if h['node_id'] == NODE), None)

    def guest_host_ready(self):
        # The maintenance receipt can arrive before the separate CVM inventory refresh.
        return self.wait(
            'fresh Cube host inventory before starting a Guest',
            self.guest_host_inventory,
            lambda h: h and h['fresh'] and not h.get('error') and h['capabilities'].get('artifact_compiler'),
        )

    def verify_inventory_while_claiming(self, release_id):
        host = self.guest_host_inventory()
        require(host and host['observed_at'], 'missing Cube inventory before claim')
        # Force a real inventory round while the local mark fences ordinary CVM work.
        updated = self.wait(
            'new Cube inventory while the claim is held',
            self.guest_host_inventory,
            lambda observed: observed and observed['observed_at'] != host['observed_at'],
            timeout=150,
        )
        require(
            updated['fresh'] and not updated.get('error') and updated['capabilities'].get('backend') == 'cube-kvm-v1',
            'pending claim replaced healthy Cube inventory with a maintenance error',
        )
        require(self.release(release_id)['status'] == 'ready', 'claim moved during the held inventory check')

    def owned_guests(self, active, idle, allow_missing=False):
        instances = self.acceptance.state['instances']
        names = {active, idle}
        expected = {instances[name] for name in names if name in instances}
        require(allow_missing or len(expected) == 2, 'both owned guest IDs must exist')
        actual = {
            item['desired']['id']
            for item in self.acceptance.call('GET', '/cvm')['instances']
            if item['node_id'] == NODE and item['desired']['power'] != 'deleted'
        }
        require(expected == actual, 'compute has other retained guests, or requested guests belong elsewhere')

    def read_catalog(self, catalog_path):
        private_directory(catalog_path)
        catalog = json.loads((catalog_path / 'lab-catalog.json').read_text())
        require(set(catalog) == {'public', 'source', 'target', 'pairs'}, 'unexpected fixture catalogue fields')
        require(set(catalog['pairs']) == {'forward', 'reverse', 'first-install'}, 'unexpected fixture catalogue pairs')
        ids = [catalog['public'], *catalog['pairs'].values()]
        for label in ('source', 'target'):
            identity = catalog[label]
            ids.extend(identity[key] for key in ('bundle_sha256', 'cubelet_sha256', 'shim_sha256'))
            require(identity['storage_backend'] == 'plain', 'catalogue requires plain storage')
        require(all(DIGEST.fullmatch(i) for i in ids), 'invalid catalogue file identities')
        require(self.remote('uname', '-m').stdout.strip() == b'x86_64', 'stack acceptance requires x86_64')
        return catalog

    def require_input(self, name, path):
        expected = json.loads((self.output / 'services/input-hashes.json').read_text())[name]
        actual = self.remote('sha256sum', path).stdout.split()[0]
        require(actual == expected.encode(), f'deployed {name} differs from the frozen lab input')

    def prepare_catalog(self, catalog_path, catalog, *, fresh=False):
        # The first-install lab exposes only the source-free entry and A bytes.
        labels = ('first-install',) if fresh else ('forward', 'reverse')
        pairs = [catalog['pairs'][label] for label in labels]
        archives = ('source',) if fresh else ('source', 'target')
        destination = self.prefix + '/stack-catalog'
        self.remote('mkdir', '-m', '0700', '--', destination)
        for label in archives:
            sha = catalog[label]['bundle_sha256']
            name = sha + '.tar.gz'
            path = catalog_path / name
            require(digest(path) == sha, 'catalogued bundle differs from its pin')
            self.lab.upload('compute', path, destination + '/' + name, 0o600)
            require(
                self.remote('sha256sum', destination + '/' + name).stdout.split()[0] == sha.encode(),
                'remote catalogue bundle differs from its pin',
            )
        for pair in pairs:
            name = pair + '.json'
            self.remote_write(destination + '/' + name, (catalog_path / name).read_bytes())
        self.lab.upload('compute', Path(__file__).with_name('release_proxy.py'), self.prefix + '/proxy.py')
        self.remote(
            '/usr/bin/python3',
            '-I',
            '-c',
            PREPARE,
            data=json.dumps([self.prefix, self.task, catalog['public'], fresh]).encode(),
            timeout=240,
        )
        self.wait(
            'real Console restarted',
            lambda: self.remote('curl', '-fsS', 'http://127.0.0.1:8080/healthz', check=False).returncode == 0,
        )
        overview = self.view()
        require({p['id'] for p in overview['pairs']} == set(pairs), 'Console did not verify the signed catalogue')
        require(all('expires_at' not in p for p in overview['pairs']), 'Console advertised a catalogue expiration')
        return overview

    def prepare(self, catalog_path, active, idle, seed_missing):
        require(self.state is None, 'stack acceptance is already prepared')
        require(not self.lab.state.get('agent_only_compute'), 'use a separate preinstalled lab for upgrade acceptance')
        require(active != idle and all(IDENTITY.fullmatch(n) for n in (active, idle)), 'invalid guest names')
        catalog = self.read_catalog(catalog_path)
        source = catalog['source']
        self.login()
        self.owned_guests(active, idle, allow_missing=seed_missing)
        actual = self.remote('readlink', BASE + '/current').stdout.decode().strip()
        require(actual == f'{BASE}/releases/{source["bundle_sha256"]}', 'deployed source is not pinned bundle A')
        self.require_input('cvm_stack.py', MANAGER)
        self.prepare_catalog(catalog_path, catalog)
        self.host(source)
        if seed_missing:
            missing = [name for name in (active, idle) if name not in self.acceptance.state['instances']]
            if missing:
                for name in missing:
                    self.acceptance.create(name, NODE)
                self.acceptance.complete(self.acceptance.publish(), timeout=300)
        self.owned_guests(active, idle)
        self.state = {'catalog': catalog, 'active': active, 'idle': idle, 'cases': {}, 'guests': {}}
        for name in (active, idle):
            item = self.acceptance.instance_ready(name)
            # A pre-existing stopped guest only needs physical root and never boots here.
            if name == idle and item['report']['power'] == 'stopped':
                guest = {'before': self.root(item)}
            else:
                require(item['report']['power'] == 'running', 'active guest must begin running')
                guest = self.seed_guest(name, item)
                if name == idle:
                    item = self.power(name, 'stop')
            if name == idle:
                guest['generation'] = item['desired']['generation']
            self.state['guests'][name] = guest
        self.assert_idle()
        self.save()

    def empty_host(self, *, initial=False, claim_id=None):
        return self.remote_json(
            EMPTY_HOST_PROOF, [self.prefix, BASE, STATE, STORAGE, MANAGER, AGENT_BIN, AGENT_UNIT, initial, claim_id]
        )

    def first_install(self, catalog_path, guest_name):
        evidence_path = self.evidence / 'first-install.json'
        require(self.lab.state.get('agent_only_compute') is True, 'deploy a new Agent-only compute lab first')
        require(
            self.state is None and not os.path.lexists(evidence_path), 'preserved earlier stack acceptance evidence'
        )
        require(IDENTITY.fullmatch(guest_name), 'invalid guest name')
        catalog = self.read_catalog(catalog_path)
        target = catalog['source']
        self.login()
        workspace = self.acceptance.call('GET', '/cvm')
        require(
            not self.acceptance.state['instances']
            and not any(item['node_id'] == NODE for item in workspace['instances']),
            'first install requires a lab without any preexisting compute instances',
        )
        self.require_input('agent', AGENT_BIN)
        initial = self.empty_host(initial=True)
        gate = self.acceptance.state.get('gate', {})
        require(not any(value for name, value in gate.items() if name != 'nodes'), 'unexpected preexisting lab fault')
        evidence = {'initial': initial}
        atom_json(evidence_path, evidence)

        with step('enable signed first-install catalogue and verify Agent-only compute'):
            overview = self.prepare_catalog(catalog_path, catalog, fresh=True)
            self.ensure_owner()
            require(not any(r['node_id'] == NODE for r in overview['releases']), 'compute already has a stack release')
            (pair,) = overview['pairs']
            require(
                pair['id'] == catalog['pairs']['first-install']
                and pair['contract'] == cvm_stack.FRESH_INSTALL_CONTRACT
                and pair['source_sha256'] is None
                and pair['target_sha256'] == target['bundle_sha256']
                and pair['storage_backend'] == 'plain',
                'unexpected first-install pair overview',
            )
            evidence['empty_observation'] = self.wait(
                'real Agent reports a positively verified empty plain host', lambda: empty_observation(self.view())
            )
            self.require_input('cvm_stack.py', MANAGER)
            atom_json(evidence_path, evidence)

        with step('reject corrupt first-install download without installing Cube'):
            self.ensure_owner()
            sha = pair['target_sha256']
            target_path = BASE + '/releases/' + sha
            download_path = self.prefix + '/gate/' + NODE + '-stack-download.summary.json'
            self.remote('test', '!', '-e', download_path)
            self.remote('test', '!', '-e', target_path)
            self.acceptance.gate(restart=False, stack_download={'node': NODE, 'sha256': sha, 'mode': 'hash'})
            try:
                entry = self.create_release(pair)
                release_id = entry['id']
                evidence['failed_release_id'] = release_id
                atom_json(evidence_path, evidence)
                terminal = self.wait_release(release_id, 'failed')
                proof = self.read_remote_json(download_path)
                route = '/agent/v1/cvm/stack-bundles/' + sha
                require(proof and proof['mode'] == 'hash' and proof['path'] == route, 'no corrupted download observed')
                require(terminal['roots_sha256'] is None and terminal['error'], 'fault did not fail during preflight')
                self.remote('test', '!', '-e', target_path)
                require(self.empty_host() == initial, 'failed download changed empty storage or restarted Agent')
                evidence['download_fault'] = proof
                atom_json(evidence_path, evidence)
            finally:
                self.acceptance.gate(restart=False, stack_download=None)

        with step('preflight and automatically claim first install of Cube A'):
            self.hold_claim()
            entry = self.create_release(pair)
            release_id = entry['id']
            evidence.update(release_id=release_id, start_deadline_at=entry['start_deadline_at'])
            atom_json(evidence_path, evidence)
            empty_roots = cvm_stack.empty_roots_digest('plain')
            evidence['claim'] = self.wait_claim(release_id, empty_roots)
            require(self.empty_host(claim_id=release_id) == initial, 'first install changed the host before claim')
            atom_json(evidence_path, evidence)
            self.allow_claim()
            terminal = self.wait_release(release_id, 'succeeded')
            require(terminal['roots_sha256'] == empty_roots, 'terminal receipt lacks canonical empty roots')
            self.wait(
                'first-install fence cleared after receipt',
                lambda: self.remote('test', '!', '-e', BASE + '/stack-maintenance', check=False).returncode == 0,
            )
            self.remote('test', '!', '-e', BASE + '/stack-install.json')
            self.host(target)
            require(self.cold() == empty_roots, 'installed Cubelet does not prove empty roots')
            filesystem = self.remote('findmnt', '-n', '-o', 'UUID', '-M', STORAGE).stdout.decode().strip()
            require(filesystem == initial['filesystem_uuid'], 'first install replaced the owned persistent storage')
            installation = self.read_remote_json(BASE + '/installation.json')
            require(
                installation
                and installation['current'] == target['bundle_sha256']
                and not installation.get('pending')
                and installation['action'] == 'stack-install-activate'
                and installation['previous'] is None
                and installation['stack_install_job'] == release_id,
                'first install did not durably commit its authorized bundle',
            )
            evidence.update(roots_sha256=empty_roots, installation=installation, installed_runtime=self.runtime(target))
            atom_json(evidence_path, evidence)

        with step('provision and confirm the first native KVM guest on installed Cube A'):
            self.guest_host_ready()
            self.acceptance.call('PUT', '/deployment-settings', {'unattended': True})
            self.acceptance.complete(self.acceptance.publish(), timeout=300)
            self.acceptance.create(guest_name, NODE)
            self.acceptance.complete(self.acceptance.publish(), timeout=300)
            guest = self.acceptance.instance_ready(guest_name)
            require(guest['report']['power'] == 'running', 'first guest did not boot')
            require(self.guest_ready(guest_name, 'id -un', trust=True) == 'cube', 'first guest did not answer SSH')
            evidence['guest'] = {'root': self.root(guest), 'runtime': self.runtime(target, guest)}
            atom_json(evidence_path, evidence)

    def root(self, item):
        runtime, volume = item['report']['runtime_id'], item['report']['volume_id']
        ident = item['desired']['id']
        require(
            all(isinstance(v, str) and IDENTITY.fullmatch(v) for v in (ident, volume))
            and (runtime is None or IDENTITY.fullmatch(runtime)),
            'missing stable root identity',
        )
        return self.remote_json(ROOT_PROOF, [STATE, STORAGE, ident, runtime, volume])

    def guest(self, name, code, *, trust=False):
        item, role, address = self.acceptance.guest_endpoint(name)
        require(role == 'compute', 'wrong guest execution host')
        endpoint = str(ipaddress.ip_address(address))
        ident = item['desired']['id']
        words = self.acceptance.prepare_guest_password(item, role)
        return (
            self.remote(
                *words,
                '-o',
                'StrictHostKeyChecking=' + ('accept-new' if trust else 'yes'),
                '-o',
                'HostKeyAlias=' + ident,
                '-o',
                'UserKnownHostsFile=' + self.prefix + '/stack-ssh-' + ident,
                '-o',
                'ConnectTimeout=5',
                '-o',
                'NumberOfPasswordPrompts=1',
                'cube@' + endpoint,
                code,
                timeout=25,
            )
            .stdout.decode()
            .strip()
        )

    def guest_ready(self, name, code, trust=False):
        def run():
            try:
                return self.guest(name, code, trust=trust)
            except (AcceptanceError, RuntimeError, OSError):
                return None

        return self.wait(name + ' actual SSH/application probe', run, timeout=120)

    def seed_guest(self, name, item):
        marker = f'{self.task}-{uuid.uuid4().hex}'
        path = '~/stack-' + self.task
        self.guest_ready(name, 'id -un', trust=True)
        value = self.guest(name, f'printf "%s\\n" {marker} > {path} && sync {path} && cat {path}')
        require(value == marker, 'guest did not commit the baseline application write')
        identity = self.guest(name, 'ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256').split()[1]
        boot = self.guest(name, 'cat /proc/sys/kernel/random/boot_id')
        return {'before': self.root(item), 'writes': [marker], 'ssh_host': identity, 'boot': boot}

    def power(self, name, action):
        self.acceptance.operation(name, action)
        self.acceptance.complete(self.acceptance.publish(), timeout=300)
        desired = 'running' if action == 'start' else 'stopped'
        return self.wait(
            name + ' actual ' + desired + ' receipt',
            lambda: self.acceptance.instance(name),
            lambda item: (
                item['status'] == 'applied'
                and item['desired']['power'] == desired
                and item['report']
                and item['report']['power'] == desired
            ),
        )

    def assert_idle(self):
        item = self.acceptance.instance(self.state['idle'])
        saved = self.state['guests'][self.state['idle']]['before']
        require(
            item['desired']['power'] == 'stopped'
            and item['desired']['generation'] == self.state['guests'][self.state['idle']]['generation']
            and item['report']['power'] == 'stopped'
            and item['report']['volume_retained']
            and item['report']['network_absent'],
            'idle guest is not in its original stopped state',
        )
        require(self.root(item) == saved, 'idle guest root object changed')
        taps = self.remote('curl', '-fsS', 'http://127.0.0.1:18080/v1/network/taps').stdout
        runtime = saved['runtime']
        require(
            all(runtime not in (tap.get('sandboxID'), tap.get('ownerSandboxID')) for tap in json.loads(taps)['taps']),
            'idle guest has a live TAP owner',
        )

    def cold(self):
        value = (
            self.remote(
                'env',
                'BROCADE_CVM_STATE_DIR=' + STATE,
                '/usr/local/bin/brocade-agent',
                'cvm-check',
                '--stack-cold-read-only',
                '--config',
                BASE + '/current/generated/cube.json',
            )
            .stdout.decode()
            .strip()
        )
        require(DIGEST.fullmatch(value), 'host did not prove its actual cold roots')
        return value

    def runtime(self, expected, item=None):
        proof = self.remote_json(RUNTIME_PROOF, [BASE, CUBELET_UNIT, item['report']['runtime_id'] if item else None])
        identity = expected['bundle_sha256']
        require(proof['bundle_sha256'] == identity, 'wrong installed bundle selected')
        require(
            proof['cubelet']['sha256'] == expected['cubelet_sha256']
            and proof['cubelet']['exe'] == f'{BASE}/releases/{identity}/{cvm_stack.CUBELET}',
            'wrong running Cubelet bytes',
        )
        if item:
            require(proof['shim']['sha256'] == expected['shim_sha256'], 'wrong running KVM Shim bytes')
        return proof

    def read_remote_json(self, path):
        result = self.remote('cat', path, check=False)
        return json.loads(result.stdout) if result.returncode == 0 else None

    def create_release(self, pair):
        # PostgreSQL and Console run on the compute VM; bracket creation with that VM's clock.
        earliest = int(self.remote('date', '+%s').stdout)
        entry = self.acceptance.call('POST', API, {'node_id': NODE, 'pair_id': pair['id']}, expected=(200, 201, 202))
        latest = int(self.remote('date', '+%s').stdout)
        require(
            type(entry['id']) is int
            and entry['id'] > 0
            and entry['status'] == 'preparing'
            and entry['node_id'] == NODE
            and entry['source_sha256'] == pair['source_sha256']
            and entry['target_sha256'] == pair['target_sha256'],
            'unexpected created stack release',
        )
        deadline = entry.get('start_deadline_at')
        require(
            type(deadline) is int
            and earliest + MAINTENANCE_WINDOW_SECONDS <= deadline <= latest + MAINTENANCE_WINDOW_SECONDS,
            'release does not have a five-hour server-assigned start deadline in Unix seconds',
        )
        return entry

    def claim_proof_path(self):
        return self.prefix + '/gate/' + NODE + '-stack-claim.summary.json'

    def hold_claim(self):
        self.remote('rm', '-f', '--', self.claim_proof_path())
        self.acceptance.gate(restart=False, hold_stack_claim=[NODE])

    def allow_claim(self):
        self.acceptance.gate(restart=False, hold_stack_claim=[])

    def wait_claim(self, release_id, roots):
        ready = self.wait_release(release_id, 'ready')
        require(ready['roots_sha256'] == roots, 'ready receipt does not match independent cold proof')
        proof = self.wait('real Agent claim delayed', lambda: self.read_remote_json(self.claim_proof_path()))
        require(
            proof['id'] == release_id
            and proof['roots_sha256'] == roots
            and self.release(release_id)['status'] == 'ready',
            'proxy did not hold the matching Agent claim before activation',
        )
        return proof

    def release(self, release_id):
        require(type(release_id) is int and release_id > 0, 'invalid stack release ID')
        return next(r for r in self.view()['releases'] if r['id'] == release_id)

    def wait_release(self, release_id, status):
        entry = self.wait(
            f'stack release {release_id} {status}',
            lambda: self.release(release_id),
            lambda r: r['status'] in TERMINAL | {status},
            timeout=900,
        )
        require(entry['status'] == status, f'stack release {release_id} reached {entry["status"]}, wanted {status}')
        return entry

    @contextmanager
    def fault(self, scenario, release_id, target):
        if scenario not in ('activation-failure', 'manager-death'):
            yield
            return
        self.remote_write(self.prefix + '/candidate.py', CANDIDATE_HOOK.encode())
        self.remote_write(
            self.prefix + '/candidate.json',
            json.dumps(
                {
                    'task': self.task,
                    'id': release_id,
                    'target': target,
                    'mode': scenario,
                }
            ).encode(),
        )
        hook = 'ExecStartPre' if scenario == 'activation-failure' else 'ExecStartPost'
        self.remote_write(
            DROPIN,
            f'[Service]\n{hook}=/usr/bin/python3 -I {self.prefix}/candidate.py {self.prefix}\nRestart=no\n'.encode(),
        )
        self.remote('systemctl', 'daemon-reload')
        try:
            yield
        finally:
            self.remote(
                'rm',
                '-f',
                '--',
                DROPIN,
                self.prefix + '/candidate.py',
                self.prefix + '/candidate.json',
                self.prefix + '/candidate-entered',
                self.prefix + '/candidate-resume',
            )
            self.remote('systemctl', 'daemon-reload')

    def injection_entered(self, release_id):
        proof = self.remote('cat', self.prefix + '/candidate-entered', check=False)
        return proof.returncode == 0 and proof.stdout.strip() == str(release_id).encode()

    def interrupt_manager(self, release_id, target, roots):
        self.wait('candidate start is held by owned fault hook', lambda: self.injection_entered(release_id))
        unit = f'brocade-cvm-stack-{release_id}.service'
        job = self.read_remote_json(BASE + '/stack-maintenance')
        install = self.read_remote_json(BASE + '/installation.json')
        checkpoint = self.read_remote_json(BASE + '/stack-upgrade.json')
        require(
            job['offer']['id'] == release_id
            and job['phase'] == 'started'
            and job['roots_sha256'] == roots
            and install.get('pending') == target['bundle_sha256']
            and install.get('action') == 'stack-activate'
            and checkpoint['phase'] == 'switching'
            and checkpoint['roots'] == roots,
            'no matching durable pending checkpoint at the manager fault boundary',
        )
        require(
            self.remote('systemctl', 'show', '-p', 'MainPID', '--value', AGENT_UNIT).stdout.strip() == b'0',
            'ordinary Agent must be stopped while independent manager runs',
        )
        manager_pid = int(self.remote('systemctl', 'show', '-p', 'MainPID', '--value', unit).stdout)
        require(manager_pid > 1, 'independent manager not running')
        words = self.remote('cat', f'/proc/{manager_pid}/cmdline').stdout.rstrip(b'\0').split(b'\0')
        require(
            words == [b'/usr/local/bin/brocade-agent', b'cvm-stack-execute', str(release_id).encode()],
            'transient unit PID does not own this release',
        )
        live_candidate = self.runtime(target)
        self.remote('systemctl', 'kill', '--signal=SIGKILL', unit)
        self.wait(
            'independent manager process died',
            lambda: self.remote('test', '!', '-e', f'/proc/{manager_pid}', check=False).returncode == 0,
            timeout=15,
        )
        require(
            self.read_remote_json(BASE + '/installation.json') == install,
            'pending installation record did not survive manager death',
        )
        # Let the already-running candidate finish its systemd start job.
        # Do not start Agent or invoke recovery ourselves.
        self.remote_write(self.prefix + '/candidate-resume', b'continue\n')
        return {
            'killed_main_pid': manager_pid,
            'checkpoint_phase': checkpoint['phase'],
            'pending': install['pending'],
            'live_candidate': live_candidate,
        }

    def verify_active(self, scenario, expected):
        self.guest_host_ready()
        name = self.state['active']
        guest = self.state['guests'][name]
        # A restarted Agent reconciles the stopped generation independently of
        # its host inventory. Starting a Guest requires both to be current.
        self.wait(
            name + ' applied stopped receipt before reboot',
            lambda: self.acceptance.instance(name),
            lambda item: (
                item['status'] == 'applied'
                and item['desired']['power'] == 'stopped'
                and (report := item['report'])
                and report['power'] == 'stopped'
                and report['generation'] == item['desired']['generation']
            ),
        )
        item = self.power(name, 'start')
        require(self.root(item) == guest['before'], 'active guest root object changed')
        boot = self.guest_ready(name, 'cat /proc/sys/kernel/random/boot_id')
        require(boot != guest['boot'], 'start did not boot a new guest kernel')
        ssh_host = self.guest(name, 'ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256').split()[1]
        require(ssh_host == guest['ssh_host'], 'actual guest SSH host key changed')
        path = '~/stack-' + self.task
        require(
            self.guest(name, 'cat ' + path).splitlines() == guest['writes'],
            'application contents changed across cold switch',
        )
        marker = f'{scenario}-{uuid.uuid4().hex}'
        expected_writes = [*guest['writes'], marker]
        require(
            self.guest(name, f'printf "%s\\n" {marker} >> {path} && sync {path} && cat {path}').splitlines()
            == expected_writes,
            'application could not commit a fresh write after boot',
        )
        runtime = self.runtime(expected, item)
        guest.update(boot=boot, writes=expected_writes)
        self.assert_idle()
        return runtime

    def smoke(self, use_provider_echo):
        for src, dst in (('provider', 'client'), ('client', 'provider'), ('client', 'compute')):
            peer = str(ipaddress.ip_address(self.lab.state['roles'][dst]['ipv4']))
            require(
                self.lab.remote(
                    src, shlex.join(['ping', '-c', '2', '-W', '2', peer]), timeout=8, check=False
                ).returncode
                == 0,
                'owned provider/client LAN stopped forwarding test traffic',
            )
        if use_provider_echo:
            name = self.state['active']
            self.acceptance.network_ready(name)
            value = 'cold-stack-' + uuid.uuid4().hex
            require(
                self.acceptance.echo('tcp4', value, name=name) == value, 'provider business path did not carry real TCP'
            )
        return 'business-tcp-and-lab-lan' if use_provider_echo else 'lab-lan-only'

    def scenario(self, name, late_receipt=False, provider_echo=False):
        catalog = self.state['catalog']
        reverse = name == 'rollback'
        source, target = (catalog['target'], catalog['source']) if reverse else (catalog['source'], catalog['target'])
        self.owned_guests(self.state['active'], self.state['idle'])
        self.host(source)
        active = self.state['active']
        active_state = self.state['guests'][active]
        first = self.runtime(source, self.acceptance.instance_ready(active))
        item = self.power(active, 'stop')
        require(self.root(item) == active_state['before'], 'stop changed physical root')
        self.assert_idle()
        roots = self.cold()
        pair_id = catalog['pairs']['reverse' if reverse else 'forward']
        pair = next(p for p in self.view()['pairs'] if p['id'] == pair_id)
        require(
            pair['contract'] == COLD_PAIR
            and pair['source_sha256'] == source['bundle_sha256']
            and pair['target_sha256'] == target['bundle_sha256']
            and pair['storage_backend'] == 'plain',
            'Console advertised unexpected signed pair',
        )
        download_path = self.prefix + '/gate/' + NODE + '-stack-download.summary.json'
        report_path = self.prefix + '/gate/' + NODE + '-stack-report.summary.json'
        self.remote('rm', '-f', '--', download_path, report_path)
        corrupt = name.removeprefix('bad-') if name.startswith('bad-') else None
        if corrupt:
            target_path = BASE + '/releases/' + target['bundle_sha256']
            self.remote('test', '!', '-e', target_path)
            self.acceptance.gate(
                restart=False, stack_download={'node': NODE, 'sha256': target['bundle_sha256'], 'mode': corrupt}
            )
        else:
            self.hold_claim()
        hold_report = name == 'success'
        try:
            entry = self.create_release(pair)
            release_id = entry['id']
            evidence = {
                'release_id': release_id,
                'start_deadline_at': entry['start_deadline_at'],
                'roots_sha256': roots,
                'before': first,
            }
            self.state['cases'][name] = evidence
            self.save()
            if corrupt:
                terminal = self.wait_release(release_id, 'failed')
                proof = self.read_remote_json(download_path)
                route = '/agent/v1/cvm/stack-bundles/' + target['bundle_sha256']
                require(
                    proof and proof['mode'] == corrupt and proof['path'] == route and STACK_BUNDLE.fullmatch(route),
                    'no actual target download fault was observed',
                )
                require(
                    not terminal.get('roots_sha256') and terminal.get('error'),
                    'download failure did not remain in preflight',
                )
                self.remote('test', '!', '-e', target_path)
                evidence['download_fault'] = proof
            else:
                evidence['claim'] = self.wait_claim(release_id, roots)
                self.save()
                if name == 'cancel':
                    self.acceptance.call('POST', f'{API}/{release_id}/cancel', expected=(200, 202))
                    terminal = self.wait_release(release_id, 'cancelled')
                    self.allow_claim()
                else:
                    if name == 'activation-failure':
                        self.verify_inventory_while_claiming(release_id)
                    with self.fault(name, release_id, target['bundle_sha256']):
                        if hold_report:
                            self.acceptance.gate(
                                restart=False, hold_stack_report={'node': NODE, 'id': release_id, 'status': 'succeeded'}
                            )
                        self.allow_claim()
                        if name == 'manager-death':
                            evidence['manager_death'] = self.interrupt_manager(release_id, target, roots)
                        if hold_report:
                            held = self.wait(
                                'real terminal report delayed', lambda: self.read_remote_json(report_path), timeout=900
                            )
                            require(
                                held['roots_sha256'] == roots and self.release(release_id)['status'] == 'applying',
                                'Console was not waiting for an actual terminal receipt',
                            )
                            self.acceptance.call('POST', f'{API}/{release_id}/cancel', expected=409)
                            evidence['cancel_after_claim_rejected'] = True
                            if late_receipt:
                                job = self.read_remote_json(BASE + '/stack-maintenance')
                                require(
                                    job['phase']['done']['id'] == release_id,
                                    'terminal receipt was not persisted offline',
                                )
                                self.remote('systemctl', 'restart', AGENT_UNIT)
                                require(
                                    self.read_remote_json(BASE + '/stack-maintenance') == job,
                                    'late receipt did not survive Agent restart',
                                )
                                evidence['late_receipt'] = held
                            self.acceptance.gate(restart=False, hold_stack_report=None)
                        status = 'recovered' if name in ('activation-failure', 'manager-death') else 'succeeded'
                        terminal = self.wait_release(release_id, status)
                        if name == 'activation-failure':
                            require(self.injection_entered(release_id), 'candidate failure hook did not execute')
                        require(terminal['roots_sha256'] == roots, 'terminal release receipt does not match cold roots')
            evidence['status'] = terminal['status']
            self.wait(
                'local stack fence cleared after receipt',
                lambda: self.remote('test', '!', '-e', BASE + '/stack-maintenance', check=False).returncode == 0,
            )
            self.remote('test', '!', '-e', BASE + '/stack-upgrade.json')
            expected = target if terminal['status'] == 'succeeded' else source
            self.host(expected)
            require(self.cold() == roots, 'cold roots digest changed across release')
            cold_runtime = self.runtime(expected)
            installation = self.read_remote_json(BASE + '/installation.json')
            require(
                installation['current'] == expected['bundle_sha256'] and not installation.get('pending'),
                'installer did not commit a settled stack identity',
            )
            if corrupt or name == 'cancel':
                require(first['cubelet'] == cold_runtime['cubelet'], 'operation before claim restarted Cubelet')
            elif terminal['status'] == 'recovered':
                require(installation['action'] == 'stack-recover', 'no durable rollback commit')
            else:
                require(installation['action'] == 'stack-activate', 'no durable switch commit')
            evidence['cold_installation'] = installation
            evidence['after'] = self.verify_active(name, expected)
            evidence['smoke'] = self.smoke(provider_echo)
            evidence['verified'] = True
            self.save()
        finally:
            if corrupt or hold_report:
                self.acceptance.gate(restart=False, stack_download=None, hold_stack_report=None)

    def ensure_owner(self):
        require(
            self.remote('cat', self.prefix + '/' + OWNER).stdout.strip() == self.task.encode(),
            'missing owned stack acceptance marker',
        )

    def ensure_prepared(self):
        require(self.state is not None, 'prepare the dedicated stack acceptance first')
        self.ensure_owner()
        self.login()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    catalogue = sub.add_parser('catalog')
    catalogue.add_argument('--output', required=True, type=Path)
    for role in ('source', 'target'):
        catalogue.add_argument('--' + role, required=True, type=Path)
        catalogue.add_argument('--' + role + '-sha256', required=True)
    prepare = sub.add_parser('prepare')
    prepare.add_argument('--lab-output', required=True, type=Path)
    prepare.add_argument('--catalog', required=True, type=Path)
    prepare.add_argument('--active', default='alpha')
    prepare.add_argument('--idle', default='stack-idle')
    prepare.add_argument('--seed-missing', action='store_true')
    first = sub.add_parser('first-install', help='install Cube A on a separate Agent-only lab and boot one guest')
    first.add_argument('--lab-output', required=True, type=Path)
    first.add_argument('--catalog', required=True, type=Path)
    first.add_argument('--guest', default='alpha')
    run = sub.add_parser('run')
    run.add_argument('--lab-output', required=True, type=Path)
    run.add_argument('--case', choices=SCENARIOS, action='append')
    run.add_argument('--late-receipt', action='store_true', help='hold the genuine success report until Agent restart')
    run.add_argument(
        '--provider-echo', action='store_true', help='use an existing release KVM provider TCP publication'
    )
    args = parser.parse_args()
    if args.action == 'catalog':
        with step('verify and sign offline stack catalogue'):
            make_catalog(args)
        return
    harness = StackAcceptance(args.lab_output)
    lock = harness.evidence / 'acceptance.lock'
    fd = os.open(lock, os.O_CREAT | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as stream:
        fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with step('verify private KVM ownership'):
            harness.check_lab()
        if args.action == 'prepare':
            with step('prepare real Agent service, durable storage and lab guests'):
                harness.prepare(args.catalog.resolve(), args.active, args.idle, args.seed_missing)
        elif args.action == 'first-install':
            harness.first_install(args.catalog.resolve(), args.guest)
        else:
            harness.ensure_prepared()
            for name in args.case or SCENARIOS:
                require(
                    name not in harness.state['cases'], 'case already exists; preserve its evidence and investigate'
                )
                with step(name):
                    harness.scenario(name, args.late_receipt and name == 'success', args.provider_echo)
    print(f'Private stack evidence: {harness.evidence}', flush=True)


if __name__ == '__main__':
    try:
        main()
    except AcceptanceError as error:
        sys.exit(f'Stack acceptance stopped: {error}')
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        sys.exit(
            f'Stack acceptance stopped ({type(error).__name__}); inspect private lab evidence (no resources destroyed).'
        )
