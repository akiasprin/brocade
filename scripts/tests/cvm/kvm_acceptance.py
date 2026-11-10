#!/usr/bin/env python3
"""Docker Console -> production Agent round -> Rust Cube client -> actual KVM Guest.

Requires an explicitly selected lab host and cached verified Cube template.
Never stops Cubelet or deletes pre-existing objects. Unknown cleanup leaves evidence.
"""
import argparse
import datetime as dt
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import secrets
import shlex
import socket
import subprocess
import time
import urllib.error
import urllib.request
import uuid
import base64
import remote_nics
import lifecycle
import terminal


def run(args, *, data=None, timeout=90, check=True):
    p = subprocess.run(args, input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    if check and p.returncode:
        raise RuntimeError(f"command failed ({p.returncode}): {args[0]} {args[1] if len(args)>1 else ''}")
    return p


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--console-binary', type=Path, required=True)
    parser.add_argument('--agent-test-binary', type=Path, required=True)
    parser.add_argument('--ssh-helper', type=Path, required=True)
    parser.add_argument('--ssh-port', type=int, required=True)
    parser.add_argument('--cube-config', default='/var/lib/nodeweave/config.json')
    parser.add_argument('--cube-socket', default='/data/cubelet/cubelet.sock')
    parser.add_argument('--cubecli', default='/usr/local/services/cubetoolbox/Cubelet/bin/cubecli')
    parser.add_argument('--network-inspection', default='http://127.0.0.1:9998/v1/network/taps')
    parser.add_argument('--source-image-id', required=True)
    parser.add_argument('--guest-visible-console', default='http://10.0.2.2:{port}')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--lifecycle-power-helper', type=Path, help='explicitly selected disposable host power-cycle helper')
    parser.add_argument('--xray-binary', type=Path, help='the Xray embedded in this exact Console build')
    parser.add_argument('--virtualization-mode', choices=('kvm','pvm'), default='kvm')
    parser.add_argument('--storage-backend', choices=('xfs', 'plain'), default='xfs')
    parser.add_argument('--guest-terminal', action='store_true', help='exercise actual Console/Agent/Guest PTY relay and cleanup')
    parser.add_argument('--smoke', action='store_true', help='real Guest boot, SSH, retained-disk stop/start and physical deletion; omits the extended matrix')
    parser.add_argument('--remote-nics',action='store_true',help='exercise Store-allocated real NICs, independent routes and both IP transports')
    parser.add_argument('--network-probe-binary',type=Path)
    args = parser.parse_args()
    if args.remote_nics and (not args.xray_binary or not args.network_probe_binary):
        parser.error('remote NIC acceptance requires exact Xray and static IP probe binaries')
    if args.lifecycle_power_helper and not os.access(args.lifecycle_power_helper, os.X_OK):
        parser.error('lifecycle power helper must be executable before creating a test Guest')
    for binary in [args.console_binary, args.agent_test_binary]:
        if not binary.is_file(): parser.error('all current build inputs must exist')
    output = args.output.resolve(); output.mkdir(parents=True, exist_ok=True); output.chmod(0o700)
    task = uuid.uuid4().hex[:10]
    prefix = f'/var/lib/brocade/acceptance-{task}'
    unit = f'brocade-cvm-acceptance-{task}'
    network, db, console = [f'brocade-cvm-{task}-{name}' for name in ['net', 'db', 'console']]
    started, mono = dt.datetime.now(dt.timezone.utc).isoformat(), time.monotonic()
    checks = []; created = None; cleaned = False; cookie = None; attempted = False
    broker = None
    network_lab = None
    primary_network, primary_container = network + '-primary', console + '-primary'
    primary_nat_endpoint = None

    def record(name, **detail):
        checks.append({'name':name, 'passed':True, 'at':dt.datetime.now(dt.timezone.utc).isoformat(), **detail})
        print('PASS ' + name, flush=True)

    def remote(command, data=None, check=True, timeout=90):
        return run(['sh', str(args.ssh_helper), str(args.ssh_port), command], data=data, check=check, timeout=timeout)

    def py(code, data=None):
        return remote('sudo python3 -c ' + shlex.quote(code), data).stdout

    def runtime_snapshot():
        code = "import subprocess,json; p=subprocess.run("+repr([args.cubecli,'--address',args.cube_socket,'--namespace','default','cubebox','ls','--all','--no-trunc'])+",stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True); print(json.dumps(sorted({line.split()[2] for line in p.stdout.decode().splitlines()[1:] if len(line.split())>=5})))"
        return json.loads(py(code))

    def private(path, data):
        path.write_bytes(data); path.chmod(0o600)

    def upload(source, name, executable=False):
        code = f"import sys,os; p={prefix+'/'+name!r}; f=open(p,'wb');\nwhile True:\n b=sys.stdin.buffer.read(1024*1024)\n if not b:break\n f.write(b)\nf.flush();os.fsync(f.fileno());f.close();os.chmod(p,{0o700 if executable else 0o600})"
        # Stream binary inputs; do not load a large Rust test executable into memory.
        with source.open('rb') as f:
            p = subprocess.run(['sh',str(args.ssh_helper),str(args.ssh_port),'sudo python3 -c '+shlex.quote(code)],stdin=f,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=180)
        if p.returncode: raise RuntimeError('KVM acceptance file upload failed')

    def sql(query):
        return run(['docker','exec','-i',db,'psql','-U','brocade','-d','brocade','-v','ON_ERROR_STOP=1','-At'],data=query.encode()).stdout.decode().strip()

    def call(method, path, body=None, expected=200, authorization=None, opener=None, origin=None, agent_protocol=None):
        headers={'content-type':'application/json'}
        if agent_protocol is not None: headers['x-brocade-protocol-version']=str(agent_protocol)
        if origin: headers['origin']=origin
        if authorization: headers['authorization']='Bearer '+authorization
        request=urllib.request.Request(broker+path,headers=headers,data=json.dumps(body).encode() if body is not None else None,method=method)
        try: response=(opener or admin).open(request,timeout=20)
        except urllib.error.HTTPError as e: response=e
        data=response.read()
        try:value=json.loads(data) if data else None
        except json.JSONDecodeError:
            private(output/'last-nonjson-response.txt',data[:65536])
            raise RuntimeError(f'HTTP {method} {path}: received non-JSON HTTP {response.status}; see private response evidence')
        if response.status not in (expected if isinstance(expected, tuple) else (expected,)):
            # Keep the full private response locally, never echo credentials or download URLs.
            private(output/'last-http-error.json',json.dumps({'path':path,'status':response.status,'body':value},ensure_ascii=False).encode())
            raise RuntimeError(f'HTTP {method} {path}: expected {expected}, received {response.status}; see private evidence')
        return value

    def converge(cvm_only=False, expect_observer_unavailable=False):
        environment = {
            'BROCADE_RUN_CVM_KVM':'1','BROCADE_AGENT_SERVER':args.guest_visible_console.format(port=port),
            'BROCADE_NODE_TOKEN_FILE':prefix+'/node.token','BROCADE_AGENT_STATE_DIR':prefix+'/agent-state',
            'BROCADE_AGENT_APPLY':'linux','BROCADE_CVM_CONFIG':prefix+'/config.json','BROCADE_CVM_STATE_DIR':prefix+'/runtime',
        }
        if expect_observer_unavailable:environment['BROCADE_EXPECT_CVM_OBSERVER_UNAVAILABLE']='1'
        command='sudo env '+' '.join(shlex.quote(k+'='+v) for k,v in environment.items())+' '+shlex.quote(prefix+'/agent-test')+' --exact cvm::tests::kvm_convergence_round --ignored --nocapture'
        p=remote(command,check=False)
        with (output/'agent-rounds.log').open('ab') as f:f.write(p.stdout+p.stderr)
        if p.returncode or b'1 passed' not in p.stdout or b'cvm::tests::kvm_convergence_round ... ok' not in p.stdout:raise RuntimeError('production Agent convergence round failed or did not execute exactly one test; see private Agent log')

    def wait_instance(status='applied', attempts=80):
        nonlocal created
        for _ in range(attempts):
            converge(); item=call('GET','/cvm/'+created['desired']['id'])
            if item['status']==status:
                created=item
                return item
            time.sleep(1)
        private(output/'last-instance.json',json.dumps(item,ensure_ascii=False).encode())
        raise RuntimeError(f'actual instance did not reach {status}; last state {item["status"]}')

    def guest(command):
        ip=created['report']['access_address']
        assert ip and ip != created['report']['interfaces'][0]['addresses'][0].split('/')[0]
        words=['ssh','-i',prefix+'/guest.key','-o','IdentitiesOnly=yes','-o','StrictHostKeyChecking=accept-new','-o','UserKnownHostsFile='+prefix+'/guest.known-'+created['desired']['id'],'-o','ConnectTimeout=5','cube@'+ip,command]
        return remote('sudo '+' '.join(shlex.quote(word) for word in words)).stdout.decode().strip()

    def operation(action, **values):
        nonlocal created
        created=call('POST','/cvm/'+created['desired']['id']+'/operations',{'action':action,'request_id':uuid.uuid4().hex,'expected_generation':created['desired']['generation'],**values},expected=202)
        return created

    admin=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    private(output/'execution-context.json',json.dumps({'remote_prefix':prefix,'unit':unit,'containers':[db,console],'network':network,'primary_container':primary_container,'primary_network':primary_network}).encode())
    try:
        # Read-only snapshot of existing runtime identities, then a private isolated Rust executor.
        py(f'import os;os.makedirs({prefix!r},mode=0o700,exist_ok=False);os.mkdir({prefix+"/agent-state"!r},0o700)')
        config_code = r'''
import json,hashlib,os,sys
params=json.load(sys.stdin);base=params['prefix']
c=json.load(open(params['source'])); t=next(t for t in c['Templates'] if t['ID']==params['template'])
r=t['Request']; root_artifact=r['containers'][0]['image']['annotations']['cube.master.rootfs.artifact.sha256']
# Convert only the system definition. Discard every hardware/snapshot binding.
for container in r['containers']:
 container.pop('resources',None)
for volume in r.get('volumes',[]):
 volume.get('volume_source',{}).get('empty_dir',{}).pop('SizeLimit',None)
for owner in [r,*r['containers'],*[c.get('image',{}) for c in r['containers']]]:
 annotations=owner.get('annotations',{})
 for key in list(annotations):
  if any(part in key for part in ['appsnapshot','template.spec_fingerprint','system_disk_size','writable_layer_size','com.exposed_ports']):del annotations[key]
r.pop('exposed_ports',None)
request=json.dumps(r,separators=(',',':')).encode();path=base+'/system-image.json'
open(path,'wb').write(request);os.chmod(path,0o600)
cap={'create':True,'delete':True,'max_interfaces':3,'allocatable_vcpus':2,'allocatable_memory_bytes':268435456,'allocatable_disk_bytes':6442450944,
'images':[{'id':'debian13','name':'Debian','version':'13','architecture':'x86_64','sha256':hashlib.sha256(request).hexdigest(),'boot_mode':'cube-oci'}],
'network_sources':[{'id':'contact','name':'Mapped contact','kind':'public','description':'actual Cube NIC; public access uses separate authorized forwarding'}]}
cap.update(start=True,stop=True,stable_storage=True,retain_on_delete=False,restart=True,reinstall=True,configure=True,hot_attach=True,hot_detach=True)
cap['allocatable_memory_bytes']=536870912
cap['resource_limits']={'min_vcpus':1,'max_vcpus':2,'min_memory_bytes':134217728,'max_memory_bytes':536870912,'memory_step_bytes':67108864,'min_root_disk_bytes':3221225472,'max_root_disk_bytes':4294967296,'root_disk_step_bytes':1073741824}
cfg={'cubelet_socket':params['socket'],'network_inspection_url':params['inspection'],'storage_backend':params['storage_backend'],'capabilities':cap,'image_files':{'debian13':path},'virtualization_mode':params['virtualization_mode']}
if params.get('lifecycle'):
 cap.update(stable_storage=True,start=True,stop=True,restart=True,reinstall=True,configure=True)
p=base+'/config.json';open(p,'w').write(json.dumps(cfg));os.chmod(p,0o600)
print(json.dumps({'resources':{'vcpus':1,'memory_bytes':134217728,'root_disk_bytes':3221225472},'image':cap['images'][0],'request_sha256':cap['images'][0]['sha256'],'root_artifact_sha256':root_artifact}))
'''
        manifest=json.loads(py(config_code,json.dumps({'virtualization_mode':args.virtualization_mode,'storage_backend':args.storage_backend,'prefix':prefix,'source':args.cube_config,'template':args.source_image_id,'socket':args.cube_socket,'cli':args.cubecli,'inspection':args.network_inspection,'network_lab':network_lab,'lifecycle':bool(args.lifecycle_power_helper)}).encode()))
        private(output/'image-evidence.json',json.dumps(manifest,indent=2).encode())
        upload(args.agent_test_binary,'agent-test',True)
        run(['ssh-keygen','-q','-t','ed25519','-N','','-f',str(output/'guest.key')])
        upload(output/'guest.key','guest.key')
        remote('sudo systemd-run --quiet --collect --unit='+unit+' --property=Type=oneshot --property=RemainAfterExit=yes /bin/true')
        if args.lifecycle_power_helper:
            lifecycle.install_services(py,remote,prefix,unit,network_lab)
        original_runtimes=runtime_snapshot()
        private(output/'preexisting-runtimes.json',json.dumps(original_runtimes).encode())
        record('real KVM host and private Rust Cube client configuration prepared',**manifest)
        run(['docker','network','create',network])
        if args.remote_nics:
            # A benchmark-range address is allowed by the primary NIC's private
            # network fence. It lives outside the VM, reachable through QEMU's
            # real underlay, while the Docker bridge keeps the test isolated.
            for _ in range(4):
                octet = int(uuid.uuid4().hex[:2], 16)
                address = f'198.19.{octet}.2'
                made = run(['docker','network','create','--internal','--subnet',f'198.19.{octet}.0/28',primary_network], check=False)
                if made.returncode == 0: break
            else: raise RuntimeError('cannot allocate the isolated primary NAT fixture network')
            run(['docker','run','-d','--name',primary_container,'--network',primary_network,'--ip',address,
                 '-v',str(args.network_probe_binary.resolve())+':/probe:ro',
                 'ubuntu@sha256:6a06111c3b0a8600feadb270aec771dc239bf407b25625f93f44ab00e11f4d3c',
                 '/probe','tcp-server','0.0.0.0:9090'])
            for _ in range(50):
                try:
                    with socket.create_connection((address,9090),timeout=1): pass
                    break
                except OSError: time.sleep(.1)
            else: raise RuntimeError('isolated primary NAT echo did not become ready')
            primary_nat_endpoint = address + ':9090'
        password,bootstrap=secrets.token_hex(24),secrets.token_hex(32)
        private(output/'db.env',f'POSTGRES_USER=brocade\nPOSTGRES_DB=brocade\nPOSTGRES_PASSWORD={password}\n'.encode())
        run(['docker','run','-d','--name',db,'--network',network,'--network-alias','db','--env-file',str(output/'db.env'),'postgres:16-alpine'])
        for _ in range(60):
            if run(['docker','exec',db,'pg_isready','-h','127.0.0.1','-U','brocade','-d','brocade'],check=False).returncode==0:break
            time.sleep(.5)
        else:raise RuntimeError('isolated PostgreSQL TCP listener did not become ready')
        with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
        broker=f'http://127.0.0.1:{port}'
        private(output/'console.env',f'DATABASE_URL=postgres://brocade:{password}@db:5432/brocade\nBROCADE_BOOTSTRAP_TOKEN={bootstrap}\nBROCADE_SECRET_KEY={base64.b64encode(secrets.token_bytes(32)).decode()}\nBROCADE_ADMIN_BIND=0.0.0.0:8080\nBROCADE_AGENT_PUBLIC_URL={args.guest_visible_console.format(port=port)}\n'.encode())
        run(['docker','run','-d','--name',console,'--network',network,'--env-file',str(output/'console.env'),'-p',f'127.0.0.1:{port}:8080','-v',str(args.console_binary.resolve())+':/app/brocade-console:ro','ubuntu@sha256:6a06111c3b0a8600feadb270aec771dc239bf407b25625f93f44ab00e11f4d3c','/app/brocade-console'])
        for _ in range(100):
            try:urllib.request.urlopen(broker+'/healthz',timeout=2);break
            except (OSError,urllib.error.URLError):time.sleep(.3)
        else:raise RuntimeError('Docker Console did not become healthy')
        if args.remote_nics:
            distribution = call('GET', '/enroll/dist')
            assert distribution['xray_bin_sha256_x86_64'] == hashlib.sha256(args.xray_binary.read_bytes()).hexdigest()
            record('publication testing uses the exact Xray digest advertised by this Console build')
            with urllib.request.urlopen(broker + '/enroll/cvm-stack.py', timeout=10) as response:
                helper = response.read(1024 * 1024)
            assert hashlib.sha256(helper).hexdigest() == distribution['cvm_installer_sha256']
            assert helper == Path(__file__).resolve().parents[2].joinpath('cvm_stack.py').read_bytes()
            with urllib.request.urlopen(broker + '/enroll/install.sh', timeout=10) as response:
                installer = response.read(1024 * 1024).decode()
            assert '__BROCADE_CVM_' not in installer and distribution['cvm_installer_sha256'] in installer
            record('actual public enrollment endpoints serve this exact CVM helper and a rendered digest-pinned installer')
        admin_password=secrets.token_urlsafe(24)
        private(output/'admin.credentials.json',json.dumps({'origin':broker,'operator_id':'acceptance-admin','password':admin_password}).encode())
        call('POST','/auth/init',{'operator_id':'acceptance-admin','display_name':'Acceptance','password':admin_password,'root_tenant':'platform'},expected=201,authorization=bootstrap)
        node='kvm-'+task
        sql("INSERT INTO users(tenant_id,id,uuid) VALUES ('platform','alice','"+str(uuid.uuid4())+"'),('platform','bob','"+str(uuid.uuid4())+"');\n"+f"INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,api_port,overlay,egress_allowed,dns_kind,dns_servers) VALUES ('{node}','platform','KVM acceptance','192.0.2.10','10.66.0.1','fixture-private','fixture-public',51820,10085,TRUE,TRUE,'servers','[\"1.1.1.1\"]'::jsonb);")
        token=call('POST','/nodes/'+node+'/agent-token',{},expected=201)['token']
        tokenfile=output/'node.token';private(tokenfile,token.encode());upload(tokenfile,'node.token')
        converge()
        workspace=call('GET','/cvm')
        private(output/'initial-workspace.json',json.dumps(workspace,ensure_ascii=False).encode())
        assert len(workspace['hosts'])==1 and workspace['hosts'][0]['fresh']
        assert workspace['hosts'][0]['capabilities']['max_interfaces']==3
        record('Docker Console receives capabilities from the production Agent and direct Rust executor')
        quota={'tenant_id':'platform','user_id':'alice','enabled':True,'allowed_nodes':[node],'max_instances':1,'max_vcpus':2,'max_memory_bytes':536870912,'max_disk_bytes':6442450944,'max_interfaces':4}
        if workspace['hosts'][0]['capabilities'].get('network_quota'):
            limits={'max_states':100000,'new_per_second':1000,'burst':2000}
            preview=call('POST','/cvm/quota/network/preview',{'tenant_id':'platform','user_id':'alice','node_id':node,'limits':limits})
            assert preview['fits']
            quota['host_network_quotas']=[{'node_id':node,'expected_revision':preview['revision'],'budget_revision':preview['budget_revision'],'limits':limits}]
        call('PUT','/cvm/quota',quota,expected=204)
        converge()
        if workspace['hosts'][0]['capabilities'].get('network_quota'):
            applied=call('GET','/cvm')['network_quotas'][0]
            assert applied['fresh'] and applied['report']['applied'] and applied['report']['revision']==applied['revision']
            record('Store allocates user-host network capacity and actual kernel acknowledgement precedes Guest admission')
        request={'request_id':uuid.uuid4().hex,'tenant_id':'platform','user_id':'alice','node_id':node,'name':'acceptance-'+task,'image_id':'debian13','resources':{'vcpus':1,'memory_bytes':134217728,'root_disk_bytes':3221225472},'ssh_public_key':(output/'guest.key.pub').read_text().strip(),'interfaces':[{'source_id':'contact','purpose':'contact','default_ipv4':False,'default_ipv6':False}]}
        bad={**request,'request_id':uuid.uuid4().hex,'interfaces':request['interfaces'][:1]*2}
        call('POST','/cvm',bad,expected=400)
        assert sql('SELECT count(*) FROM cvm_instances')=='0'
        record('unsupported or duplicate-source NIC request is refused before reservation or runtime allocation')
        attempted=True
        private(output/'creation-identity.json',json.dumps({'request_id':request['request_id'],'name':request['name']}).encode())
        alice=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        bob=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        for username,opener in [('alice',alice),('bob',bob)]:
            login=call('POST','/users/platform/'+username+'/login',{},expected=201)
            call('POST','/auth/login',{'operator_id':username,'password':login['password']},opener=opener)
        created=call('POST','/cvm',request,expected=202,opener=alice)
        replay=call('POST','/cvm',request,expected=202,opener=alice);assert replay['desired']['id']==created['desired']['id']
        assert sql('SELECT count(*) FROM cvm_volumes')=='1'
        record('duplicate HTTP creation reserves exactly one instance and one disk')
        call('GET','/cvm/'+created['desired']['id'],expected=403,opener=bob)
        call('GET','/cvm/'+created['desired']['id']+'/monitoring',expected=403,opener=bob)
        call('PUT','/cvm/quota',quota,expected=403,opener=alice)
        assert call('GET','/cvm',opener=bob)['instances']==[]
        record('real Docker HTTP users cannot read another owner or grant their own quota')
        created=wait_instance();assert created['report']['power']=='running' and len(created['report']['interfaces'])==1
        runtime=created['report']['runtime_id']
        evidence=json.loads(py(r'''
import glob,json,os,sys
wanted=sys.stdin.read();out=[]
for p in glob.glob('/proc/[0-9]*'):
 try:
  cmd=open(p+'/cmdline','rb').read().decode(errors='replace')
  if wanted not in cmd or 'containerd-shim-cube' not in cmd:continue
  fds=[os.readlink(f) for f in glob.glob(p+'/fd/*') if os.path.lexists(f)]
  if any('kvm-vm' in f for f in fds):out.append({'pid':int(p.rsplit('/',1)[1]),'kvm_vm_fd':True,'vcpus':sum('kvm-vcpu' in f for f in fds)})
 except (OSError,ProcessLookupError):pass
assert len(out)==1 and out[0]['vcpus']==1
print(json.dumps(out[0]))
''',runtime.encode()))
        record('actual independent KVM VM FD and one vCPU exist for the created runtime',**evidence)
        boot=guest('cat /proc/sys/kernel/random/boot_id')
        assert boot and guest('id -un')=='cube'
        small_memory=int(guest("awk '/MemTotal:/ {print $2}' /proc/meminfo"))
        small_disk=int(guest('df -B1 --output=size / | tail -1'))
        host_key=guest('ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256')
        assert 32768 < small_memory <= 131072 and 2*1024**3 < small_disk <= 3*1024**3
        marker='brocade-'+task
        assert guest('printf '+shlex.quote(marker)+' > ~/brocade-acceptance.txt; cat ~/brocade-acceptance.txt')==marker
        record('actual Guest SSH public-key login and filesystem write/read pass',boot_id=boot,actual_interfaces=created['report']['interfaces'])
        if args.remote_nics:
            created=remote_nics.verify(args,created,prefix,unit,call,guest,py,remote,upload,wait_instance,record,primary_nat_endpoint)
        if args.smoke:
            original_volume = created['report']['volume_id']
            operation('stop'); created = wait_instance()
            assert created['report']['power'] == 'stopped' and created['report']['volume_id'] == original_volume
            operation('start'); created = wait_instance()
            assert created['report']['power'] == 'running' and created['report']['volume_id'] == original_volume
            assert guest('cat ~/brocade-acceptance.txt') == marker
            assert guest('ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256') == host_key
            record('smoke stop/start retains the physical root identity, written data and SSH host identity')
            created = operation('delete', delete_data=True); created = wait_instance()
            assert created['report']['power'] == 'deleted' and created['report']['volume_deleted'] and not created['reserved']
            assert runtime_snapshot() == original_runtimes
            cleaned = True
            record('smoke deletion removes the actual runtime, TAP and root disk before releasing reservations')
            return
        if args.guest_terminal:
            terminal.verify(args, broker, admin, node, prefix, created, call, py, record)
        for _ in range(40):
            converge()
            monitor=call('GET','/cvm/'+created['desired']['id']+'/monitoring',opener=alice)
            if monitor['state']=='live' and monitor['samples']: break
            time.sleep(1)
        assert monitor['state']=='live' and monitor['samples'], 'real Guest monitoring has no sample'
        first=monitor.get('latest') or monitor['samples'][-1]
        actual_memory=int(guest("awk '/^MemTotal:/ {print $2}' /proc/meminfo"))*1024
        actual_disk=int(guest("df -Pk / | awk 'NR==2 {print $(NF-4)}'"))*1024
        assert first['memory_total_bytes']==actual_memory and first['root_total_bytes']==actual_disk
        assert {n['id'] for n in first['interfaces']}=={n['id'] for n in created['report']['interfaces']}
        # Only this disposable Guest receives the short CPU load. A later real
        # production-Agent report must show a measured, nonzero interval.
        guest("nohup timeout 18 sh -c 'while :; do :; done' >/tmp/brocade-monitor-load.log 2>&1 </dev/null &")
        for _ in range(30):
            time.sleep(1);converge()
            monitor=call('GET','/cvm/'+created['desired']['id']+'/monitoring',opener=alice)
            point=monitor.get('latest') or monitor['samples'][-1]
            if point['observed_at_unix_millis']-first['observed_at_unix_millis']>=10000 and (point['cpu_usage_basis_points'] or 0)>500 and all(n['rx_bps'] is not None and n['tx_bps'] is not None for n in point['interfaces']):break
        else:raise RuntimeError('real CPU load and per-NIC monitoring rates were not observed')
        record('real Guest CPU load, available memory, root filesystem and each NIC monitoring reach owner-scoped Docker HTTP',cpu_usage_basis_points=point['cpu_usage_basis_points'],interface_count=len(point['interfaces']))
        if args.lifecycle_power_helper:
            created=lifecycle.verify(guest,py,remote,prefix,created,record,operation,wait_instance,converge,lambda:run([str(args.lifecycle_power_helper.resolve())],timeout=90),network_lab)
            runtime=created['report']['runtime_id']
            boot=guest('cat /proc/sys/kernel/random/boot_id')
            marker=guest('cat ~/brocade-acceptance.txt')
        run(['docker','restart',console]);time.sleep(1)
        for _ in range(60):
            try:call('GET','/cvm');break
            except (OSError,urllib.error.URLError):time.sleep(.3)
        assert guest('cat /proc/sys/kernel/random/boot_id')==boot
        assert guest('cat ~/brocade-acceptance.txt')==marker
        record('Docker Console restart preserves durable state and the same Guest boot and data')
        remote('sudo mv '+prefix+'/config.json '+prefix+'/config.unavailable')
        try:
            converge(expect_observer_unavailable=True);unknown=call('GET','/cvm/'+created['desired']['id']);assert unknown['status']=='unknown' and unknown['reserved']
            assert guest('cat /proc/sys/kernel/random/boot_id')==boot
        finally:
            remote('sudo mv '+prefix+'/config.unavailable '+prefix+'/config.json')
        created=wait_instance();assert created['report']['runtime_id']==runtime
        assert guest('cat ~/brocade-acceptance.txt')==marker
        record('Rust executor initialization failure holds reservation; a new Agent process reconciles original runtime and data')
        created=operation('delete',delete_data=True);created=wait_instance()
        assert created['report']['power']=='deleted' and not created['reserved']
        assert created['report']['network_absent'] and created['report']['volume_deleted']
        assert sql("SELECT count(*) FROM cvm_volumes WHERE state<>'released'")=='0'
        # A second instance uses the identical system manifest and a different
        # capacity profile. Its disk and memory must reflect the selected resources.
        first_image=created['desired']['spec']['image']
        request={**request,'request_id':uuid.uuid4().hex,'name':'larger-'+task,'resources':{'vcpus':2,'memory_bytes':335544320,'root_disk_bytes':4294967296}}
        created=call('POST','/cvm',request,expected=202,opener=alice)
        created=wait_instance()
        assert created['desired']['spec']['image']==first_image
        assert created['desired']['spec']['resources']['memory_bytes']==335544320
        assert int(guest("awk '/^processor/ {n++} END {print n}' /proc/cpuinfo"))==2
        large_memory=int(guest("awk '/MemTotal:/ {print $2}' /proc/meminfo"))
        large_disk=int(guest('df -B1 --output=size / | tail -1'))
        assert large_memory>small_memory+65536 and large_memory<=327680
        assert large_disk>small_disk and 3*1024**3<large_disk<=4*1024**3
        assert guest('ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256')!=host_key
        record('one unchanged system image boots different CPU, memory and disk resources with unique SSH identity',small_vcpus=1,large_vcpus=2,small_memory_kib=small_memory,large_memory_kib=large_memory,small_filesystem_bytes=small_disk,large_filesystem_bytes=large_disk)
        created=operation('delete',delete_data=True);created=wait_instance()
        assert created['report']['power']=='deleted' and created['report']['volume_deleted'] and not created['reserved']
        cleaned=True
        assert runtime_snapshot()==original_runtimes
        record('pre-existing Cube runtime identities are unchanged')
        record('actual runtime, TAP and physical disk removal confirmed before reservation release')
    finally:
        private(output/'runtime.log',remote('sudo journalctl --no-pager -u '+unit+' -o cat',check=False).stdout)
        console_logs=run(['docker','logs',console],check=False)
        private(output/'console.log',console_logs.stdout+console_logs.stderr)
        # Never tear down uncertain live state or erase its diagnostic identity.
        if attempted and not cleaned:
            private(output/'pending-cleanup.json',json.dumps({'console':broker,'instance_id':created['desired']['id'] if created else None,'remote_prefix':prefix,'unit':unit,'containers':[db,console],'network':network,'primary_container':primary_container,'primary_network':primary_network}).encode())
            print('Acceptance incomplete: retained isolated services and execution state for safe cleanup.',flush=True)
        else:
            if args.lifecycle_power_helper:
                lifecycle.cleanup_services(py,remote,unit,network_lab)
            remote('sudo systemctl stop '+unit,check=False)
            run(['docker','rm','-f','-v',console,db],check=False)
            run(['docker','network','rm',network],check=False)
            if args.remote_nics:
                run(['docker','rm','-f',primary_container],check=False)
                run(['docker','network','rm',primary_network],check=False)
            # Remove only the private directory created by this exact run.
            py(f'import shutil;shutil.rmtree({prefix!r},ignore_errors=True)')
        result={'acceptanceScope':'smoke' if args.smoke else 'full','virtualizationMode':args.virtualization_mode,'startedAt':started,'endedAt':dt.datetime.now(dt.timezone.utc).isoformat(),'durationSeconds':round(time.monotonic()-mono,3),'checks':checks,'guestCleanupConfirmed':cleaned,'artifacts':{str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in [args.console_binary,args.agent_test_binary]}}
        private(output/'result.json',json.dumps(result,ensure_ascii=False,indent=2).encode())

if __name__=='__main__':main()
