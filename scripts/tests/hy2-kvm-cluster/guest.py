#!/usr/bin/env python3
"""Control plane for explicitly marked, offline, disposable KVM guests only."""
import datetime
import hashlib
import http.server
import json
import os
import re
import shlex
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import time
import threading
import traceback

from apps import configure_firewall, wg_key, wireguard

ROOT = Path('/lab')
INPUTS = ROOT / 'inputs'
STATE = Path('/var/lib/brocade-agent')
ROLE = sys.argv[1]
OUT = ROOT / 'results' / ROLE
PROCESSES = {}
AUTH = 'disposable-kvm-cluster-auth'


def run(*args, timeout=30, env=None, input=None):
    result = subprocess.run([str(x) for x in args], text=True, input=input, env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f'{args}: exit {result.returncode}: {result.stdout[-3000:]} {result.stderr[-3000:]}')
    return result.stdout.strip()


def write(path, data):
    path = Path(path)
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(data, indent=2) + '\n')
    temporary.replace(path)


def stop(name):
    process = PROCESSES.pop(name, None)
    if process and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def start(name, command, env=None):
    stop(name)
    log = (OUT / (name + '.log')).open('ab')
    process = subprocess.Popen([str(x) for x in command], stdout=log, stderr=log, env=env)
    log.close()
    PROCESSES[name] = process
    time.sleep(.05)
    if process.poll() is not None:
        raise RuntimeError(f'{name} exited during startup; inspect {OUT / (name + ".log")}')
    return process.pid


def stream():
    return {'network': 'hysteria', 'security': 'tls', 'hysteriaSettings': {'version': 2},
            'tlsSettings': {'certificates': [{'certificateFile': str(INPUTS / 'cover.pem'),
                                             'keyFile': str(INPUTS / 'cover.key')}]}}


def server_config(base_port, dynamic=False):
    config = {'log': {'loglevel': 'warning'}, 'stats': {},
              'policy': {'levels': {'0': {'statsUserUplink': True, 'statsUserDownlink': True}}},
              'outbounds': [{'tag': 'forward', 'protocol': 'freedom'}], 'inbounds': []}
    if dynamic:
        config['api'] = {'tag': 'api', 'listen': '127.0.0.1:10085',
                         'services': ['HandlerService', 'StatsService']}
    for version, listen, port in ((4, '0.0.0.0', base_port), (6, '::', base_port + 100)):
        config['inbounds'].append({'tag': f'hy{version}', 'listen': listen, 'port': port,
                                  'protocol': 'hysteria', 'settings': {'version': 2, 'users': [] if dynamic else [{'auth': AUTH, 'email': f'hy{version}@lab.test'}]},
                                  'streamSettings': stream()})
    return config


def repair():
    return run(INPUTS / 'agent', 'repair', '--state-dir', STATE, timeout=60,
               env={**os.environ, 'BROCADE_XRAY_BIN': str(INPUTS / 'xray')})


def reconcile():
    class IdleConsole(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(204);self.send_header('Content-Length','0');self.end_headers()
        do_POST=do_GET
        def log_message(self,*_):pass
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),IdleConsole)
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        result=run(INPUTS/'agent','apply-once','--state-dir',STATE,'--apply','linux',
                   '--server',f'http://127.0.0.1:{server.server_port}','--token','broc_node_disposable_lab',timeout=60,
                   env={**os.environ,'BROCADE_XRAY_BIN':str(INPUTS/'xray')})
        return {'output':result,'pid':managed_pid(),'reconcile':json.loads((STATE/'local-reconcile.json').read_text())}
    finally:
        server.shutdown();server.server_close();thread.join(timeout=2)


def managed_pid():
    found = []
    for entry in Path('/proc').glob('[0-9]*'):
        try:
            if str(STATE / 'xray.json').encode() in (entry / 'cmdline').read_bytes() and 'xray' in os.readlink(entry / 'exe'):
                found.append(int(entry.name))
        except (FileNotFoundError, PermissionError):
            pass
    if len(found) != 1:
        raise RuntimeError(f'expected one managed Xray, found {found}')
    return found[0]


def node_start():
    assert ROLE == 'node'
    STATE.mkdir(mode=0o700, parents=True, exist_ok=True)
    STATE.chmod(0o700)
    write(STATE / 'xray.json', server_config(30000, True))
    write(STATE / 'source-access-policy.json', {'revision': 0, 'node_ips': [], 'users': []})
    write(STATE / 'grants.desired.json', [
        {'tag': f'hy{v}', 'clients': [{'email': f'hy{v}@lab.test', 'uuid': AUTH, 'flow': None, 'level': 0}]} for v in (4, 6)])
    write(STATE / 'hy2_port_hop.json', {'redirects': [
        {'start': 30000, 'end': 30099, 'to': 30000}, {'start': 30100, 'end': 30199, 'to': 30100}]})
    (STATE / 'usage-generation').write_text('1')
    repair()
    write('/var/tmp/baseline.json', server_config(20000))
    start('baseline', [INPUTS / 'xray', 'run', '-config', '/var/tmp/baseline.json'])
    run('nft', '-f', '-', input='''table inet lab_ct_control {
 chain pre { type filter hook prerouting priority filter; ct state new,established counter; }
 chain out { type filter hook output priority filter; ct state new,established counter; }
}''')
    time.sleep(.5)
    return {'pid': managed_pid(), 'network': json.loads((STATE / 'hy2-network.json').read_text()),
            'agent_output': (STATE / 'local-reconcile.json').read_text()}


def client_start(args):
    assert ROLE == 'client'
    records = []
    for version in (4, 6):
        address = ('198.18.2.9' if args.get('nat') else '198.18.2.2') if version == 4 else 'fd42:bc:2::2'
        base = 20000 if args.get('baseline') else 30000
        port = base + (100 if version == 6 else 0) + args.get('offset', 0)
        s = {'network': 'hysteria', 'security': 'tls', 'hysteriaSettings': {'version': 2, 'auth': args.get('auth', AUTH)},
             'tlsSettings': {'serverName': 'cover.test', 'certificates': [{'usage': 'verify', 'certificateFile': str(INPUTS / 'cover.pem')}]}}
        params={}
        if args.get('hopping'):
            first = 30000 + (100 if version == 6 else 0)
            params['udpHop']={'ports':f'{first}-{first+99}','interval':5}
        if args.get('minimum_mtu') and version==6:
            params['disableChromeParrot']=True
        if params:s['finalMask']={'quicParams':params}
        config = {'log': {'loglevel': 'warning'},
                  'inbounds': [{'listen': '127.0.0.1', 'port': 1080 + (version == 6), 'protocol': 'socks', 'settings': {'udp': True}}],
                  'outbounds': [{'protocol': 'hysteria', 'settings': {'version': 2, 'address': address, 'port': port}, 'streamSettings': s}]}
        path = Path(f'/var/tmp/client-{version}.json'); write(path, config)
        records.append(start(f'client-{version}', [INPUTS / 'xray', 'run', '-config', path]))
    time.sleep(.3)
    for version in (4, 6):
        if PROCESSES[f'client-{version}'].poll() is not None:
            raise RuntimeError('client Xray exited')
    return records


def docker(*args):
    return run('docker', '--host', 'unix:///run/lab-docker.sock', *args, timeout=45)


def docker_start(args):
    # The direct benchmark needed forwarding. Docker sets its default DROP
    # policy only when it enables forwarding itself; exercise that real path.
    run('sysctl','-w','net.ipv4.ip_forward=0')
    start('dockerd', ['dockerd', '--host', 'unix:///run/lab-docker.sock', '--data-root', '/var/lib/lab-docker',
                      '--exec-root', '/run/lab-docker', '--pidfile', '/run/lab-docker.pid', '--storage-driver', 'vfs'])
    for _ in range(150):
        try:
            version = docker('version', '--format', '{{.Server.Version}}')
            break
        except RuntimeError:
            time.sleep(.2)
    else:
        raise RuntimeError('dockerd did not start')
    docker('import', str(INPUTS / 'docker-rootfs.tar'), 'cluster-echo:local')
    docker('run', '-d', '--name', 'cluster-echo', '--security-opt', 'apparmor=unconfined',
           '-p', '28080:8080/tcp', '-p', '28081:8081/udp', 'cluster-echo:local', '/echo')
    assert '-P FORWARD DROP' in run('iptables', '-S', 'FORWARD')
    start('docker-monitor', ['python3','-u',str(INPUTS/'monitor.py'),str(args.get('interval',.5))])
    return {'version': version, 'rules': run('iptables-save'), 'bridge': json.loads(docker('network', 'inspect', 'bridge'))}


def foreign():
    run('nft', '-f', '-', input='''table inet lab_firewall {
 chain input { type filter hook input priority filter; policy drop;
  iifname "lo" accept
  ct state established,related accept
  meta l4proto { icmp, ipv6-icmp } accept
  udp dport { 20000-20199, 30000-30199, 28081 } counter accept
  tcp dport { 28080, 6001, 8080 } counter accept
 }
 chain mark_inbound { type filter hook prerouting priority mangle; iifname "eth1" meta mark set 0x42 counter; }
}''')
    for family,gateway in (('-4','198.18.2.1'),('-6','fd42:bc:2::1')):
        # Keep connected Docker/private-link routes in the policy table too.
        # Otherwise marking container replies manufactures an unrelated blackhole.
        for line in run('ip',family,'route','show','table','main').splitlines():
            if not line.startswith('default '):
                run('ip',family,'route','replace','table','100',*shlex.split(line))
        run('ip',family,'route','replace','table','100','default','via',gateway,'dev','eth0')
        run('ip', family, 'rule', 'add', 'pref', '100', 'from', '198.18.2.2' if family == '-4' else 'fd42:bc:2::2', 'lookup', '100')
        run('ip', family, 'rule', 'add', 'pref', '101', 'fwmark', '0x42', 'lookup', '100')
    # A genuine separate xtables legacy ruleset, matching unrelated traffic.
    run('iptables-legacy', '-t', 'nat', '-N', 'LAB_THIRD_PARTY')
    run('iptables-legacy', '-t', 'nat', '-A', 'POSTROUTING', '-j', 'LAB_THIRD_PARTY')
    run('iptables-legacy', '-t', 'nat', '-A', 'LAB_THIRD_PARTY', '-s', '192.0.2.0/24', '-j', 'MASQUERADE')
    run('sysctl', '-w', 'net.ipv4.conf.all.rp_filter=1', 'net.ipv4.conf.eth0.rp_filter=1', 'net.ipv4.conf.eth1.rp_filter=1')
    return foreign_state()


def foreign_state():
    firewall = subprocess.run(['nft', '-s', 'list', 'table', 'inet', 'lab_firewall'], capture_output=True, text=True)
    def rules(binary):
        return '\n'.join(re.sub(r'\[\d+:\d+\]$','[0:0]',line) if line.startswith(':') else line
                         for line in run(binary).splitlines() if not line.startswith('#'))
    return {'iptables': rules('iptables-save'), 'legacy': rules('iptables-legacy-save'), 'firewall': firewall.stdout,
            'rule4': run('ip', '-j', 'rule'), 'rule6': run('ip', '-6', '-j', 'rule')}


def qos(args):
    run('tc', 'qdisc', 'replace', 'dev', 'eth0', 'root', 'handle', '1:', 'tbf', 'rate', str(args.get('rate', 500))+'mbit', 'burst', '512k', 'latency', '50ms')
    run('tc', 'qdisc', 'add', 'dev', 'eth0', 'parent', '1:1', 'handle', '2:', 'fq')
    if args.get('capture'):
        start('capture', ['tcpdump', '-i', 'eth0', '-nn', '-s', '96', '-w', '/dev/null', 'udp'])
    if args.get('ifb'):
        run('modprobe', 'ifb')
        run('ip', 'link', 'add', 'ifb-lab', 'type', 'ifb')
        run('ip', 'link', 'set', 'ifb-lab', 'up')
        run('tc', 'qdisc', 'add', 'dev', 'eth0', 'clsact')
        run('tc', 'filter', 'add', 'dev', 'eth0', 'ingress', 'pref', '10', 'matchall', 'action', 'mirred', 'egress', 'redirect', 'dev', 'ifb-lab')
        run('tc', 'qdisc', 'add', 'dev', 'ifb-lab', 'root', 'tbf', 'rate', '300mbit', 'burst', '512k', 'latency', '50ms')
    return run('tc', '-s', '-j', 'qdisc', 'show')


def edge_nat():
    run('ip', 'addr', 'add', '198.18.2.9/24', 'dev', 'eth1')
    run('nft', '-f', '-', input='''table ip lab_edge_nat {
 chain pre { type nat hook prerouting priority dstnat; ip daddr 198.18.2.9 udp dport 30000-30199 dnat to 198.18.2.2; }
 chain post { type nat hook postrouting priority srcnat; ip saddr 198.18.1.0/24 oifname "eth1" masquerade; }
}''')
    return run('nft', 'list', 'table', 'ip', 'lab_edge_nat')


def impair(args):
    for interface in ('eth0', 'eth1'):
        run('tc', 'qdisc', 'replace', 'dev', interface, 'root', 'netem', 'limit', '4000', 'delay', '20ms', '2ms', 'loss', '0.05%', 'rate', '200mbit')
    return run('tc', '-s', '-j', 'qdisc', 'show')


def snapshot():
    pids = {name: p.pid for name, p in PROCESSES.items() if p.poll() is None}
    if ROLE == 'node':
        pids['managed-xray'] = managed_pid()
    cpu = {}
    for name, pid in pids.items():
        fields = Path(f'/proc/{pid}/stat').read_text().split(') ', 1)[1].split()
        cpu[name] = {'pid': pid, 'ticks': int(fields[11])+int(fields[12]), 'rss_pages': int(fields[21])}
    data = {'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'monotonic': time.monotonic(),
            'clock_ticks': os.sysconf('SC_CLK_TCK'), 'processes': cpu,
            'stat': Path('/proc/stat').read_text(), 'softnet': Path('/proc/net/softnet_stat').read_text(),
            'links': json.loads(run('ip', '-s', '-j', 'link')), 'qdisc': json.loads(run('tc', '-s', '-j', 'qdisc', 'show')),
            'softirq': Path('/proc/softirqs').read_text()}
    if ROLE == 'node':
        ns = json.loads((STATE / 'hy2-network.json').read_text())['namespace']
        data['root_ct'] = run('conntrack', '-L', '-o', 'extended')
        data['hy2_ct'] = run('ip', 'netns', 'exec', ns, 'conntrack', '-L', '-o', 'extended')
        data['nft'] = run('nft', '-s', 'list', 'ruleset')
    return data


def client_udp(args):
    version = args['version']
    with socket.create_connection(('127.0.0.1', 1080+(version==6)), timeout=10) as control:
        control.sendall(b'\x05\x01\x00')
        assert control.recv(2)==b'\x05\x00'
        control.sendall(b'\x05\x03\x00\x01'+b'\0'*6)
        data=b''
        while len(data)<10:
            part=control.recv(10-len(data));assert part;data+=part
        assert data[:4]==b'\x05\x00\x00\x01'
        relay_port=int.from_bytes(data[-2:],'big')
        address='198.18.3.2' if version==4 else 'fd42:bc:3::2'
        header=b'\0\0\0'+bytes([1 if version==4 else 4])+socket.inet_pton(socket.AF_INET if version==4 else socket.AF_INET6,address)+struct.pack('!H',6002)
        with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as udp:
            udp.bind(('127.0.0.1',0));udp.settimeout(10)
            for i in range(32):
                payload=bytes([i])*1100
                udp.sendto(header+payload,('127.0.0.1',relay_port))
                response=udp.recv(2048)
                assert response==header+payload, 'UDP payload integrity failed'
        return {'ipv':version,'datagrams':32,'bytes_each_direction':35200,'verified':True}


def dispatch(req):
    op, args = req['op'], req.get('args', {})
    if op == 'exec': return run(*args['command'], timeout=args.get('timeout', 60), input=args.get('input'))
    if op == 'node-start': return node_start()
    if op == 'client': return client_start(args)
    if op == 'docker': return docker_start(args)
    if op == 'foreign': return foreign()
    if op == 'foreign-state': return foreign_state()
    if op == 'qos': return qos(args)
    if op == 'edge-nat': return edge_nat()
    if op == 'impair': return impair(args)
    if op == 'snapshot': return snapshot()
    if op == 'pmtu-capture':
        command=['tcpdump','-Z','root','-U','-i','any','-nn','-s','0','-C','8','-W','2','-w',str(OUT/'pmtu.pcap'),'ip6']
        start('pmtu-capture',command)
        if ROLE=='node':
            ns=json.loads((STATE/'hy2-network.json').read_text())['namespace']
            private=command.copy();private[-2]=str(OUT/'pmtu-private.pcap')
            start('pmtu-private',['ip','netns','exec',ns,*private])
        return True
    if op == 'evidence':
        data={'snapshot':snapshot()}
        if ROLE=='node':
            data['xray_log']=(STATE/'logs/xray.log').read_text(errors='replace')[-40000:] if (STATE/'logs/xray.log').exists() else None
            data['snmp6']=Path('/proc/net/snmp6').read_text()
            ns=json.loads((STATE/'hy2-network.json').read_text())['namespace']
            data['private_snmp6']=run('ip','netns','exec',ns,'cat','/proc/net/snmp6')
            data['routes6']=run('ip','-6','route','show','table','all')
        return data
    if op == 'reconcile': return reconcile()
    if op == 'udp': return client_udp(args)
    if op == 'firewall': return configure_firewall(args['kind'],run)
    if op == 'wg-key': return wg_key(run)
    if op == 'wireguard': return wireguard(args,run)
    if op == 'schedule-reload':
        command=['firewall-cmd','--reload'] if args['kind']=='firewalld' else ['ufw','reload']
        script='''import datetime,json,pathlib,subprocess,time
time.sleep(5)
start=time.monotonic();stamp=datetime.datetime.now(datetime.timezone.utc).isoformat()
attempts=[]
for i in range(RELOAD_COUNT):
 tick=time.monotonic();begin=datetime.datetime.now(datetime.timezone.utc).isoformat()
 r=subprocess.run(COMMAND,capture_output=True,text=True,timeout=30)
 attempts.append({'exit_code':r.returncode,'stdout':r.stdout,'stderr':r.stderr,'started_at':begin,'ended_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'elapsed_seconds':time.monotonic()-tick})
 if i<RELOAD_COUNT-1:time.sleep(2)
code=next((r['exit_code'] for r in attempts if r['exit_code']),0)
pathlib.Path('/lab/results/node/firewall-reload.json').write_text(json.dumps({'exit_code':code,'started_at':stamp,'ended_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'elapsed_seconds':time.monotonic()-start,'attempts':attempts}))
raise SystemExit(code)
'''.replace('COMMAND',repr(command)).replace('RELOAD_COUNT',str(args.get('count',1)))
        return start('firewall-reload',['python3','-u','-c',script])
    if op == 'bench':
        command = [INPUTS / 'traffic', '-target', args.get('target', '198.18.3.2:6001'),
                   '-streams', str(args.get('streams', 1)), '-seconds', str(args.get('seconds', 10)),
                   '-direction', args.get('direction', 'download')]
        if args.get('proxy'): command += ['-proxy', args['proxy']]
        return json.loads(run(*command, timeout=args.get('seconds', 10)+40))
    raise ValueError(op)


def main():
    assert (INPUTS / 'DISPOSABLE_KVM_CLUSTER').read_text().strip() == 'offline-four-guests'
    assert run('systemd-detect-virt', '--vm') == 'kvm'
    assert ROLE in ('client', 'edge', 'node', 'target')
    OUT.mkdir(exist_ok=True)
    run('ip', 'link', 'set', 'lo', 'up')
    for name in ('eth0', 'eth1'):
        if Path('/sys/class/net', name).exists():
            run('ip', 'link', 'set', name, 'up')
    run('sysctl', '-w', 'net.ipv4.ip_forward=1', 'net.ipv6.conf.all.forwarding=1')
    if ROLE == 'edge':
        run('ip', 'route', 'replace', '198.18.3.0/24', 'via', '198.18.2.2')
        run('ip', '-6', 'route', 'replace', 'fd42:bc:3::/64', 'via', 'fd42:bc:2::2')
    if ROLE == 'target':
        start('target', [INPUTS / 'traffic', '-server'])
    write(OUT / 'ready.json', {'role': ROLE, 'kernel': os.uname().release,
                              'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
                              'addresses': json.loads(run('ip', '-j', 'addr')), 'versions': {'nft': run('nft', '--version'), 'ip': run('ip', '-V')}})
    commands = ROOT / 'commands' / ROLE
    deadline=time.monotonic()+14400
    while time.monotonic()<deadline:
        for path in sorted(commands.glob('*.json')):
            reply = OUT / path.name
            if reply.exists(): continue
            started = time.monotonic(); stamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
            try:
                result = {'passed': True, 'data': dispatch(json.loads(path.read_text()))}
            except Exception:
                result = {'passed': False, 'error': traceback.format_exc()}
            result.update({'started_at': stamp, 'ended_at': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'elapsed_seconds': time.monotonic()-started})
            write(reply, result)
        time.sleep(.1)
    raise TimeoutError('guest control lifetime exceeded')


if __name__ == '__main__':
    try:
        main()
    except Exception:
        OUT.mkdir(exist_ok=True)
        (OUT / 'failure.txt').write_text(traceback.format_exc())
        raise
