#!/usr/bin/env python3
"""Measured HY2 throughput and cumulative network coexistence in four real KVMs."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import time
import uuid

BASE_SHA = '6a81c37564db9b1ee84e141922625e1d7c5b389b99bb3c572e0243607d5bb4d2'


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def digest(path):
    with Path(path).open('rb') as f: return hashlib.file_digest(f, 'sha256').hexdigest()


def command(*args, timeout=90):
    return subprocess.check_output([str(x) for x in args], text=True, stderr=subprocess.STDOUT, timeout=timeout).strip()


def save(path, data):
    tmp = path.with_suffix('.tmp'); tmp.write_text(json.dumps(data, indent=2)+'\n'); tmp.replace(path)


def udp_entries(text):
    # Extended conntrack output starts with an address-family prefix on some
    # versions. Match the protocol field, never assume it is the first token.
    return [line for line in text.splitlines() if re.search(r'(?:^|\s)udp\s+\d+\s',line)]


def outer_hy2(line):
    match=re.search(r'src=(\S+) dst=(\S+) sport=(\d+) dport=(\d+)',line)
    if not match:return False
    source,destination,sport,dport=match.groups()
    local={'198.18.2.2','fd42:bc:2::2','169.254.254.2','fd42:b10c:adf0::2'}
    return (destination in local and 30000<=int(dport)<=30199) or (source in local and 30000<=int(sport)<=30199)


class Cluster:
    def __init__(self, root):
        self.root = root
        self.seq = 0
        self.records = []
        self.started = time.monotonic()
        self.last_progress = 0
        self.host_pids = {}

    def host_sample(self):
        result={'monotonic':time.monotonic(), 'clock_ticks':os.sysconf('SC_CLK_TCK'), 'processes':{},
                'loadavg':Path('/proc/loadavg').read_text(), 'stat':Path('/proc/stat').read_text()}
        for role,pid in self.host_pids.items():
            fields=Path(f'/proc/{pid}/stat').read_text().split(') ',1)[1].split()
            result['processes'][role]={'pid':pid,'ticks':int(fields[11])+int(fields[12])}
        return result

    def progress(self, phase):
        print(json.dumps({'utc': now(), 'phase': phase, 'elapsed_seconds': round(time.monotonic()-self.started, 3), 'completed_cases': len(self.records)}), flush=True)
        self.last_progress = time.monotonic()

    def rpc(self, role, op, **args):
        self.seq += 1
        filename = f'{self.seq:05}-{op}.json'
        save(self.root/'commands'/role/filename, {'op':op, 'args':args})
        path = self.root/'results'/role/filename
        deadline = time.monotonic()+max(120, args.get('seconds',0)+60, args.get('timeout',0)+30)
        while not path.exists():
            failures = list((self.root/'results').glob('**/*failure.txt'))
            if failures: raise RuntimeError(failures[0].read_text())
            if time.monotonic()>deadline: raise TimeoutError(f'RPC {role}/{filename}')
            if time.monotonic()-self.last_progress>25: self.progress(f'waiting {role}/{op}')
            time.sleep(.1)
        result = json.loads(path.read_text())
        if not result['passed']: raise RuntimeError(result['error'])
        return result['data']

    def check(self, label, data):
        self.records.append({'case':label, 'passed':True, **data})
        save(self.root/'results'/'cases.json', self.records)
        self.progress(label)

    def bench(self, profile, mode, version, direction='duplex', streams=4, seconds=10, repetition=1, wireguard=False):
        host_before=self.host_sample()
        before = {role:self.rpc(role,'snapshot') for role in ('node','client','edge','target')}
        args = {'seconds':seconds, 'streams':streams, 'direction':direction}
        if mode != 'direct': args['proxy']='127.0.0.1:'+str(1080+(version==6))
        args['target'] = '198.18.3.2:6001' if version==4 else '[fd42:bc:3::2]:6001'
        if wireguard:args['target']='10.200.0.2:6001' if version==4 else '[fd42:bc:200::2]:6001'
        result = self.rpc('client', 'bench', **args)
        after = {role:self.rpc(role,'snapshot') for role in ('node','client','edge','target')}
        host_after=self.host_sample()
        assert result['passed'] and result['verified'] and sum(result['bytes'].values())>0
        cpu = {}
        for role in before:
            delta = after[role]['monotonic']-before[role]['monotonic']
            cpu[role] = {}
            for name, end in after[role]['processes'].items():
                start = before[role]['processes'].get(name)
                if start and start['pid']==end['pid']:
                    cpu[role][name] = round((end['ticks']-start['ticks'])/after[role]['clock_ticks']/delta*100, 2)
        if mode=='namespace':
            root = [line for line in udp_entries(after['node']['root_ct']) if outer_hy2(line)]
            isolated = udp_entries(after['node']['hy2_ct'])
            assert not root, 'HY2 outer UDP leaked into root conntrack'
            assert isolated, 'HY2 namespace positive conntrack control missing'
            assert after['node']['processes']['managed-xray']['pid']==before['node']['processes']['managed-xray']['pid']
        if mode=='root-hy2':
            assert any(20000<=int(re.search(r'dport=(\d+)',line)[1])<=20199 for line in udp_entries(after['node']['root_ct'])), 'root conntrack positive control missing'
        def horizon(value):
            if isinstance(value,dict):
                for key,item in value.items():
                    if key=='horizon_drops': assert item==0, 'FQ horizon timestamp drops'
                    horizon(item)
            elif isinstance(value,list):
                for item in value:horizon(item)
        horizon(after['node']['qdisc'])
        host_cpu={role:round((end['ticks']-host_before['processes'][role]['ticks'])/host_after['clock_ticks']/(host_after['monotonic']-host_before['monotonic'])*100,2)
                  for role,end in host_after['processes'].items()}
        data = {'profile':profile, 'mode':mode, 'ipv':version, 'direction':direction, 'streams':streams,
                'repetition':repetition, 'result':result, 'cpu_percent_one_core_100':cpu,'hypervisor_cpu_percent':host_cpu,
                'client_profile':'minimum-QUIC; disableChromeParrot=true' if version==6 and profile in ('all-plus-mtu1280','all-plus-wan-delay-loss','combined-soak') else 'default ChromeParrot'}
        label = f'{profile}-{mode}-v{version}-{direction}-{streams}-r{repetition}'
        save(self.root/'results'/(label+'.json'), {'before':before,'after':after,'host_before':host_before,'host_after':host_after,**data})
        self.check(label,data)
        return data

    def docker_check(self, label, allow_reload_interruptions=False):
        egress = self.rpc('node','exec',command=['docker','--host','unix:///run/lab-docker.sock','exec','cluster-echo','/echo','egress','http://198.18.3.2:8080/'])
        assert egress=='cluster-target-ok'
        published = self.rpc('client','exec',command=['python3','-c',"import urllib.request,socket; assert urllib.request.urlopen('http://198.18.2.2:28080/',timeout=5).read()==b'container-ok'; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.settimeout(5);s.sendto(b'published-udp-integrity',('198.18.2.2',28081));assert s.recv(128)==b'published-udp-integrity';print('tcp-udp-ok')"])
        assert published=='tcp-udp-ok'
        observation=json.loads(self.rpc('node','exec',command=['python3','-c',"from pathlib import Path; p=Path('/lab/results/node/docker-monitor-observations.json');print(p.read_text() if p.exists() else 'null')"]))
        if observation and not allow_reload_interruptions:assert observation['failed_requests']==0,'steady-state Docker egress failed'
        self.check(label,{'docker_egress':True,'published_tcp_udp':True,'egress_observations':observation})


def experiment(c, seconds, repeats, include_baseline=True, start_profile=0):
    node = c.rpc('node','node-start')
    c.check('production-agent-start',node)
    for version in ((4,6) if include_baseline else ()): c.bench('clean','direct',version,streams=4,seconds=seconds)
    # Same Xray bytes; alternate order each repetition to reduce warm/order bias.
    for repetition in (range(1,repeats+1) if include_baseline else ()):
        for baseline in ((True,False) if repetition%2 else (False,True)):
            c.rpc('client','client',baseline=baseline)
            c.rpc('client','bench',proxy='127.0.0.1:1080',seconds=2,direction='duplex',streams=2)
            for version in (4,6):
                for direction,streams in (('download',1),('upload',1),('duplex',8)):
                    c.bench('clean','root-hy2' if baseline else 'namespace',version,direction,streams,seconds,repetition)
    docker = c.rpc('node','docker')
    c.check('real-docker-forward-drop',{'docker_version':docker['version']})
    c.docker_check('docker-before-combinations')
    profiles = [('docker',None),('docker-firewall-policy-legacy', 'foreign'),
                ('docker-firewall-policy-fq-capture','qos'),('docker-firewall-policy-fq-ifb-capture','ifb'),
                ('all-plus-upstream-nat','nat'),('all-plus-mtu1280','mtu'),('all-plus-wan-delay-loss','impair')]
    external = None
    for index,(name, op) in enumerate(profiles):
        if op=='foreign': external = c.rpc('node','foreign')
        if op=='qos': c.rpc('node','qos',capture=True)
        if op=='ifb':
            c.rpc('node','exec',command=['tc','qdisc','del','dev','eth0','root'])
            c.rpc('node','qos',capture=True,ifb=True)
        if op=='nat': c.rpc('edge','edge-nat')
        if op=='mtu':
            for role in ('client','edge','node'):c.rpc(role,'pmtu-capture')
            c.rpc('node','exec',command=['ip','link','set','eth0','mtu','1280'])
            c.rpc('edge','exec',command=['ip','link','set','eth1','mtu','1280'])
        if op=='impair': c.rpc('edge','impair')
        if index<start_profile:continue
        for offset in (0,37,99):
            c.rpc('client','client',offset=offset,nat=op in ('nat','mtu','impair'),minimum_mtu=index>=5)
            for version in (4,6):
                proxy='127.0.0.1:'+str(1080+(version==6))
                target='198.18.3.2:6001' if version==4 else '[fd42:bc:3::2]:6001'
                response=c.rpc('client','bench',proxy=proxy,target=target,seconds=.3,direction='duplex',streams=1)
                c.check(name+f'-boundary-v{version}-{offset}',{'result':response})
                udp=c.rpc('client','udp',version=version)
                c.check(name+f'-udp-v{version}-{offset}',udp)
        c.rpc('client','client',hopping=True,nat=op in ('nat','mtu','impair'),minimum_mtu=index>=5)
        for version in (4,6):
            for repetition in range(1,3): c.bench(name,'namespace',version,streams=4,seconds=max(seconds,12),repetition=repetition)
        c.docker_check(name+'-docker-control')
        prior = c.rpc('node','snapshot')['processes']['managed-xray']['pid']
        repaired = c.rpc('node','reconcile')
        assert repaired['pid']==prior, 'compatibility reconcile restarted live Xray'
        if external is not None: assert c.rpc('node','foreign-state')==external, 'Agent rewrote third-party rules'
        c.check(name+'-reconcile',{'same_xray_pid':prior,'foreign_rules_preserved':external is not None})
    # Longer mixed-direction multiplexing on the entire cumulative topology.
    c.bench('combined-soak','namespace',4,streams=32,seconds=120)
    c.bench('combined-soak','namespace',6,streams=32,seconds=120)
    c.docker_check('docker-after-soak')
    expected=132-18*start_profile+(2+12*repeats if include_baseline else 0)
    assert len(c.records)==expected, f'incomplete matrix {len(c.records)}/{expected}'


def application_experiment(c,kind,reload_only=False,reload_count=1,observer_interval=.5):
    c.check('production-agent-start',c.rpc('node','node-start'))
    c.check('real-'+kind,c.rpc('node','firewall',kind=kind))
    c.check('real-docker-with-'+kind,{'docker_version':c.rpc('node','docker',interval=observer_interval)['version']})
    c.docker_check(kind+'-docker-before')
    c.rpc('client','client',hopping=True)
    for version in ((4,6) if not reload_only else ()):
        for repetition in (1,2):c.bench(kind+'-docker','namespace',version,streams=8,seconds=12,repetition=repetition)
    node_key=c.rpc('node','wg-key');target_key=c.rpc('target','wg-key')
    c.rpc('node','wireguard',host=1,peer_key=target_key)
    c.rpc('target','wireguard',host=2,peer_key=node_key)
    c.check(kind+'-real-kernel-wireguard',{'peers':2,'root_namespace_egress':True})
    c.check(kind+'-fq-capture',{'qdisc':c.rpc('node','qos',rate=500,capture=True)})
    c.check(kind+'-upstream-nat',{'rules':c.rpc('edge','edge-nat')})
    c.rpc('client','client',hopping=True,nat=True)
    for version in ((4,6) if not reload_only else ()):
        for repetition in (1,2):c.bench(kind+'-docker-wireguard-nat-fq','namespace',version,streams=8,seconds=15,repetition=repetition,wireguard=True)
    if reload_only:
        for version in (4,6):
            c.rpc('client','bench',proxy='127.0.0.1:'+str(1080+(version==6)),target='10.200.0.2:6001' if version==4 else '[fd42:bc:200::2]:6001',streams=2,seconds=2,direction='duplex')
    prior=c.rpc('node','snapshot')['processes']['managed-xray']['pid']
    c.rpc('node','schedule-reload',kind=kind,count=reload_count)
    c.bench(kind+'-live-reload','namespace',4,streams=16,seconds=max(40,10+reload_count*3.5),wireguard=True)
    reload=json.loads(c.rpc('node','exec',command=['cat','/lab/results/node/firewall-reload.json']))
    assert reload['exit_code']==0,'firewall reload failed'
    assert c.rpc('node','snapshot')['processes']['managed-xray']['pid']==prior
    c.check(kind+'-live-reload-result',{'reload':reload,'same_xray_pid':prior})
    c.bench(kind+'-after-reload','namespace',6,streams=16,seconds=30,wireguard=True)
    c.docker_check(kind+'-docker-after-reload',allow_reload_interruptions=True)
    assert c.rpc('node','reconcile')['pid']==prior
    c.check(kind+'-agent-reconcile',{'same_xray_pid':prior})
    transfer=c.rpc('node','exec',command=['wg','show','wg-lab','transfer'])
    counts=transfer.split();assert len(counts)==3 and int(counts[1])>0 and int(counts[2])>0
    c.check(kind+'-wireguard-encrypted-bytes',{'transfer':transfer,'handshakes':c.rpc('node','exec',command=['wg','show','wg-lab','latest-handshakes'])})
    expected=13 if reload_only else 21
    assert len(c.records)==expected,f'incomplete application suite {len(c.records)}/{expected}'


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for key in ('output','vm-image','agent','xray','traffic','docker-rootfs'):
        parser.add_argument('--'+key,type=Path,required=True)
    parser.add_argument('--packages-image',default='brocade-hy2-docker-host:20261031')
    parser.add_argument('--qemu-image',default='brocade-hy2-cluster-qemu:20261101')
    parser.add_argument('--seconds',type=int,default=10)
    parser.add_argument('--repeats',type=int,default=3)
    parser.add_argument('--suite',choices=('network','combinations','firewalld','ufw'),default='network')
    parser.add_argument('--start-profile',type=int,choices=range(7),default=0,
                        help='combinations only: configure earlier layers, measure from this zero-based layer')
    parser.add_argument('--keep-on-failure',action='store_true',help='leave the offline cluster for bounded interactive diagnosis')
    parser.add_argument('--app-reload-only',action='store_true',help='applications only: configure the full topology and retain only reload measurements')
    parser.add_argument('--reload-count',type=int,choices=range(1,11),default=1)
    parser.add_argument('--observer-interval',type=float,default=.5)
    args=parser.parse_args()
    assert .05<=args.observer_interval<=1
    if args.start_profile and args.suite!='combinations':parser.error('--start-profile requires --suite combinations')
    if args.app_reload_only and args.suite not in ('firewalld','ufw'):parser.error('--app-reload-only requires an application suite')
    assert 5<=args.seconds<=60 and 2<=args.repeats<=10
    assert digest(args.vm_image)==BASE_SHA and Path('/dev/kvm').exists()
    root=args.output.resolve();root.mkdir(parents=True,exist_ok=False)
    inputs=root/'inputs';inputs.mkdir();(root/'results').mkdir();(root/'commands').mkdir()
    for role in ('client','edge','node','target'): (root/'commands'/role).mkdir()
    for filename in ('guest.py','hypervisor.py','apps.py','monitor.py','traffic.go','run.py','Dockerfile.qemu','Dockerfile.packages'): shutil.copy2(Path(__file__).with_name(filename),inputs/filename)
    for path,name in ((args.agent,'agent'),(args.xray,'xray'),(args.traffic,'traffic'),(args.docker_rootfs,'docker-rootfs.tar')): shutil.copy2(path,inputs/name)
    (inputs/'DISPOSABLE_KVM_CLUSTER').write_text('offline-four-guests\n')
    command('openssl','req','-x509','-newkey','ec','-pkeyopt','ec_paramgen_curve:P-256','-noenc','-keyout',inputs/'cover.key','-out',inputs/'cover.pem','-days','2','-subj','/CN=cover.test','-addext','subjectAltName=DNS:cover.test')
    (inputs/'cover.key').chmod(0o600)
    name='brocade-hy2-cluster-'+uuid.uuid4().hex[:10]
    manifest={'started_at':now(),'base_sha256':BASE_SHA,'seconds':args.seconds,'repeats':args.repeats,'suite':args.suite,'start_profile':args.start_profile,'app_reload_only':args.app_reload_only,'reload_count':args.reload_count,'observer_interval_seconds':args.observer_interval,'network':'none; three container-private TAP/vhost Ethernet bridges',
              'artifacts':{p.name:digest(p) for p in inputs.iterdir() if p.is_file()},'container':name,
              'images':{image:command('docker','image','inspect','--format','{{.Id}}',image) for image in (args.packages_image,args.qemu_image)}}
    started=time.monotonic();launched=False;c=Cluster(root)
    save(root/'manifest.json',manifest)
    try:
        package=command('docker','create','--network','none',args.packages_image)
        try: command('docker','cp',package+':/packages',inputs/'packages')
        finally: command('docker','rm',package)
        manifest['packages']={p.name:digest(p) for p in (inputs/'packages').glob('*.deb')}
        assert len(manifest['packages'])>=10
        command('docker','run','-d','--name',name,'--network','none','--cap-add','NET_ADMIN','--device','/dev/kvm','--device','/dev/net/tun','--device','/dev/vhost-net','--memory','7g','--pids-limit','1024',
                '-v',str(root)+':/lab','-v',str(args.vm_image.resolve())+':/base.img:ro',args.qemu_image,'python3','-u','/lab/inputs/hypervisor.py')
        launched=True;c.progress('four KVMs booting')
        deadline=time.monotonic()+360
        while not all((root/'results'/r/'ready.json').exists() for r in ('client','edge','node','target')):
            failure=list((root/'results').glob('**/*failure.txt'))
            if failure: raise RuntimeError(failure[0].read_text())
            if time.monotonic()>deadline: raise TimeoutError('cluster boot; inspect packages and serial logs')
            if time.monotonic()-c.last_progress>25:c.progress('waiting for offline guest setup')
            time.sleep(.5)
        proof=json.loads((root/'results'/'kvm.json').read_text());assert len(proof)==4 and all(x=={'enabled':True,'present':True} for x in proof.values())
        guests={r:json.loads((root/'results'/r/'ready.json').read_text()) for r in ('client','edge','node','target')}
        assert len({g['boot_id'] for g in guests.values()})==4
        manifest['guests']=guests;manifest['query_kvm']=proof
        for line in command('docker','top',name,'-eo','pid,args').splitlines()[1:]:
            pid,_,args_text=line.strip().partition(' ')
            for role in ('client','edge','node','target'):
                if f'/lab/vm-{role}/root.qcow2' in args_text:c.host_pids[role]=int(pid)
        assert len(c.host_pids)==4
        c.progress('four distinct kernels and KVM acceleration verified')
        if args.suite in ('network','combinations'):experiment(c,args.seconds,args.repeats,include_baseline=args.suite=='network',start_profile=args.start_profile)
        else:application_experiment(c,args.suite,reload_only=args.app_reload_only,reload_count=args.reload_count,observer_interval=args.observer_interval)
        manifest['passed']=True
    except BaseException as error:
        manifest['passed']=False;manifest['error']=type(error).__name__+': '+str(error);raise
    finally:
        if launched and not manifest.get('passed'):
            for role in ('node','client','edge','target'):
                if (root/'results'/role/'ready.json').exists():
                    try:save(root/'results'/(role+'-failure-evidence.json'),c.rpc(role,'evidence'))
                    except Exception as error:save(root/'results'/(role+'-evidence-error.json'),{'error':str(error)})
        if launched:
            try: (root/'results'/'container.log').write_text(command('docker','logs',name)+'\n')
            finally:
                if not (args.keep_on_failure and not manifest.get('passed')):
                    command('docker','stop','--time','20',name)
                    command('docker','rm',name)
        kept=launched and args.keep_on_failure and not manifest.get('passed')
        manifest.update({'ended_at':now(),'elapsed_seconds':round(time.monotonic()-started,3),'cases':len(c.records),'cleaned':not kept,'kept_for_diagnosis':bool(kept)})
        save(root/'manifest.json',manifest)
        c.progress('complete' if manifest.get('passed') else 'failed; evidence retained')


if __name__=='__main__':main()
