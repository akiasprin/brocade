#!/usr/bin/env python3
"""Run INSIDE a disposable privileged Linux container, never in the host netns.

Requires /lab/xray, iproute2, iptables, conntrack and Python 3.
Usage: python3 /lab/reverse-nat-lab.py CASE [observation_seconds]
CASE: flush_same, change_ip, change_ip_silent, downstream_silent.
All addresses are documentation/private addresses in three fresh netns.
"""
import json
import os
import socket
import subprocess as sp
import sys
import threading
import time


def cmd(*args, ns=None, check=True):
    prefix = ['ip', 'netns', 'exec', ns] if ns else []
    r = sp.run(prefix + list(args), capture_output=True, text=True)
    if check and r.returncode:
        raise RuntimeError(f"command {prefix+list(args)!r} failed: {r.stdout}{r.stderr}")
    return r.stdout + r.stderr


def echo():
    s = socket.create_server(('10.77.1.2', 8080))
    def handle(c):
        with c:
            while data := c.recv(4096):
                c.sendall(data)
    while True:
        c, _ = s.accept()
        threading.Thread(target=handle, args=(c,), daemon=True).start()


def udp_echo():
    def serve(port):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.bind(('10.77.1.2', port))
            while True:
                payload, address = sock.recvfrom(65535)
                sock.sendto(bytes([port - 8080]) + payload, address)
    threading.Thread(target=serve, args=(8081,), daemon=True).start()
    serve(8080)


def udp_probe():
    start=time.monotonic()
    # Keep the same client socket/source port across the fault; fresh sockets
    # would hide a stale reverse UDP association that never rebinds.
    with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as sock:
        sock.settimeout(0.75)
        while True:
            t=time.monotonic();ok=False;error=None
            try:
                for target in range(2):
                    payload=os.urandom(1024)
                    sock.sendto(payload,('127.0.0.1',18081+target))
                    result,_=sock.recvfrom(65535)
                    if result!=bytes([target])+payload:
                        raise OSError('UDP destination or generation mismatch')
                ok=True
            except OSError as exc:
                error=str(exc)
            print(json.dumps(dict(t=t,elapsed=t-start,ok=ok,latency=time.monotonic()-t,error=error)),flush=True)
            time.sleep(max(0,0.5-(time.monotonic()-t)))


def long_tcp():
    while True:
        try:
            sock=socket.create_connection(('127.0.0.1',18080),timeout=1)
            sock.sendall(b'initial')
            if sock.recv(7)!=b'initial':
                sock.close();time.sleep(.1);continue
            break
        except OSError:
            time.sleep(.1)
    sock.settimeout(5)
    print(json.dumps({'event':'established','t':time.monotonic()}),flush=True)
    with sock:
        while True:
            try:
                payload=os.urandom(32);sock.sendall(payload)
                reply=b''
                while len(reply)<len(payload):
                    data=sock.recv(len(payload)-len(reply))
                    if not data:raise OSError('EOF')
                    reply+=data
                if reply!=payload:raise OSError('long stream payload mismatch')
            except OSError as exc:
                print(json.dumps({'event':'ended','t':time.monotonic(),'error':str(exc)}),flush=True);return
            time.sleep(.1)


def http_canary():
    with socket.create_server(('10.77.1.2',8082)) as server:
        while True:
            client,_=server.accept()
            with client:
                client.settimeout(1)
                try:
                    client.recv(4096)
                    client.sendall(b'HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n')
                except OSError:
                    pass


def tls_target():
    import ssl
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_3
    context.set_alpn_protocols(['h2', 'http/1.1'])
    context.load_cert_chain(sys.argv[2], sys.argv[3])
    with socket.create_server(('127.0.0.1', 8443)) as server:
        while True:
            client, _ = server.accept()
            try:
                with context.wrap_socket(client, server_side=True) as conn:
                    conn.recv(4096)
            except (OSError, ssl.SSLError):
                client.close()


def probe():
    start = time.monotonic()
    while True:
        t = time.monotonic()
        ok = False
        error = None
        try:
            with socket.create_connection(('127.0.0.1', 18080), timeout=1) as c:
                c.settimeout(1)
                payload = os.urandom(32)
                c.sendall(payload)
                result = b''
                while len(result) < len(payload):
                    data = c.recv(len(payload) - len(result))
                    if not data:
                        break
                    result += data
                ok = result == payload
        except OSError as e:
            error = str(e)
        print(json.dumps(dict(t=t, elapsed=t-start, ok=ok,
                              latency=time.monotonic()-t, error=error)), flush=True)
        time.sleep(max(0, 0.5-(time.monotonic()-t)))


def main():
    if not os.path.exists('/.dockerenv'):
        raise SystemExit('This lab must run in a disposable Docker container.')
    case = sys.argv[1]
    assert case in ('flush_same', 'change_ip', 'change_ip_silent', 'downstream_silent')
    window = float(sys.argv[2]) if len(sys.argv) > 2 else 180
    out = '/lab/results/' + case + os.environ.get('LAB_RUN_SUFFIX', '')
    os.makedirs(out, exist_ok=True)
    processes = []
    try:
        for ns in ('b', 'n', 'p'):
            cmd('ip', 'netns', 'add', ns)
            cmd('ip', 'link', 'set', 'lo', 'up', ns=ns)
        for left, right, lip, rip, name in [
            ('b', 'n', '10.77.1.2/24', '10.77.1.1/24', 'bn'),
            ('n', 'p', '192.0.2.1/24', '192.0.2.2/24', 'np')]:
            cmd('ip', 'link', 'add', name+'0', 'type', 'veth', 'peer', 'name', name+'1')
            for ns, iface, addr in [(left, name+'0', lip), (right, name+'1', rip)]:
                cmd('ip', 'link', 'set', iface, 'netns', ns)
                cmd('ip', 'addr', 'add', addr, 'dev', iface, ns=ns)
                cmd('ip', 'link', 'set', iface, 'up', ns=ns)
        cmd('ip', 'route', 'add', 'default', 'via', '10.77.1.1', ns='b')
        cmd('ip', 'addr', 'add', '192.0.2.3/24', 'dev', 'np0', ns='n')
        cmd('sysctl', '-w', 'net.ipv4.ip_forward=1', ns='n')
        def snat(ip):
            cmd('iptables', '-t', 'nat', '-F', 'POSTROUTING', ns='n')
            cmd('iptables', '-t', 'nat', '-A', 'POSTROUTING', '-s', '10.77.1.0/24',
                '-o', 'np0', '-j', 'SNAT', '--to-source', ip, ns='n')
        snat('192.0.2.1')
        uid = '11111111-1111-4111-8111-111111111111'
        portal = {'log': {'loglevel': 'info'}, 'inbounds': [
            {'tag': 'rvs', 'listen': '192.0.2.2', 'port': 10000, 'protocol': 'vless',
             'settings': {'decryption': 'none', 'clients': [{'id': uid, 'reverse': {'tag': 'portal'}}]}},
            {'tag': 'external', 'listen': '127.0.0.1', 'port': 18080, 'protocol': 'dokodemo-door',
             'settings': {'address': '10.77.1.2', 'port': 8080, 'network': 'tcp'}}],
            'outbounds': [{'protocol': 'blackhole', 'tag': 'block'}],
            'routing': {'rules': [{'type': 'field', 'inboundTag': ['external'], 'outboundTag': 'portal'}]}}
        bridge = {'log': {'loglevel': 'info'}, 'outbounds': [
            {'protocol': 'freedom', 'tag': 'direct', 'settings': {'ipsBlocked': []}},
            {'protocol': 'vless', 'tag': 'tunnel', 'settings': {'address': '192.0.2.2', 'port': 10000,
             'id': uid, 'encryption': 'none', 'reverse': {'tag': 'bridge'}}}],
            'routing': {'rules': [{'type': 'field', 'inboundTag': ['bridge'], 'outboundTag': 'direct'}]}}
        for target in range(2):
            portal['inbounds'].append({'tag': 'udp'+str(target), 'listen': '127.0.0.1', 'port': 18081+target,
                'protocol': 'dokodemo-door', 'settings': {'address': '10.77.1.2', 'port': 8080+target, 'network': 'udp'}})
            portal['routing']['rules'].append({'type': 'field', 'inboundTag': ['udp'+str(target)], 'outboundTag': 'portal'})
        transport = os.environ.get('LAB_TRANSPORT', 'raw')
        if transport in ('tls', 'reality'):
            cert, key = out+'/cert.pem', out+'/key.pem'
            cmd('openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', key,
                '-out', cert, '-days', '1', '-subj', '/CN=portal.test', '-addext', 'subjectAltName=DNS:portal.test')
            if transport == 'tls':
                portal['inbounds'][0]['streamSettings'] = {'network':'tcp', 'security':'tls',
                    'tlsSettings':{'certificates':[{'certificateFile':cert, 'keyFile':key}]}}
                bridge['outbounds'][1]['streamSettings'] = {'network':'tcp', 'security':'tls',
                    'tlsSettings':{'serverName':'portal.test', 'pinnedPeerCertSha256':__import__('hashlib').sha256(__import__('ssl').PEM_cert_to_DER_cert(open(cert).read())).hexdigest()}}
            else:
                keys = dict(line.split(': ',1) for line in cmd('/lab/xray','x25519').strip().splitlines())
                private = keys.get('PrivateKey') or keys.get('Private key')
                public = keys.get('Password (PublicKey)') or keys.get('Password') or keys.get('Public key')
                assert private and public, keys
                processes.append(sp.Popen(['ip','netns','exec','p',sys.executable,__file__,'tls_target',cert,key]))
                portal['inbounds'][0]['streamSettings'] = {'network':'tcp','security':'reality',
                    'realitySettings':{'dest':'127.0.0.1:8443','serverNames':['portal.test'], 'privateKey':private,'shortIds':['0123456789abcdef']}}
                bridge['outbounds'][1]['streamSettings'] = {'network':'tcp','security':'reality',
                    'realitySettings':{'serverName':'portal.test','fingerprint':'chrome','publicKey':public,'shortId':'0123456789abcdef'}}
        delay = int(os.environ.get('LAB_RTT_MS','0'))
        if delay:
            for ns, interface in [('b','bn0'), ('p','np1')]:
                cmd('tc','qdisc','add','dev',interface,'root','netem','delay',str(delay//2)+'ms',ns=ns)
        if os.environ.get('LAB_KEEPALIVE'):
            idle, interval = map(int, os.environ['LAB_KEEPALIVE'].split(','))
            bridge['outbounds'][1]['streamSettings'] = {
                'sockopt': {'tcpKeepAliveIdle': idle, 'tcpKeepAliveInterval': interval}}
        portal['inbounds'][0]['settings']['clients'][0]['reverse']['canary_url']='http://10.77.1.2:8082/health'
        processes.append(sp.Popen(['ip','netns','exec','b',sys.executable,__file__,'http_canary']))
        for config in [portal,bridge]:
            config['stats']={}
            config['api']={'tag':'api','services':['StatsService']}
            config.setdefault('inbounds',[]).append({'tag':'api','listen':'127.0.0.1','port':10085,'protocol':'dokodemo-door','settings':{'address':'127.0.0.1'}})
            config['routing']['rules'].insert(0,{'type':'field','inboundTag':['api'],'outboundTag':'api'})
        for ns, config in [('p', portal), ('b', bridge)]:
            path = out + '/' + ns + '.json'
            with open(path, 'w') as f:
                json.dump(config, f, indent=2)
            f = open(out + '/' + ns + '.log', 'w')
            processes.append(sp.Popen(['ip', 'netns', 'exec', ns, '/lab/xray', 'run', '-c', path], stdout=f, stderr=f))
            f.close()
        processes.append(sp.Popen(['ip', 'netns', 'exec', 'b', sys.executable, __file__, 'echo']))
        f = open(out + '/probes.jsonl', 'w')
        processes.append(sp.Popen(['ip', 'netns', 'exec', 'p', sys.executable, __file__, 'probe'], stdout=f))
        f.close()
        processes.append(sp.Popen(['ip','netns','exec','b',sys.executable,__file__,'udp_echo']))
        with open(out+'/udp-probes.jsonl','w') as udp_log:
            processes.append(sp.Popen(['ip','netns','exec','p',sys.executable,__file__,'udp_probe'],stdout=udp_log))
        with open(out+'/long-tcp.jsonl','w') as f:
            processes.append(sp.Popen(['ip','netns','exec','p',sys.executable,__file__,'long_tcp'],stdout=f))
        time.sleep(float(os.environ.get('LAB_WARMUP', '10')))
        with open(out+'/udp-probes.jsonl') as f:
            udp_baseline = [json.loads(line) for line in f]
        assert len(udp_baseline)>=5 and all(row['ok'] for row in udp_baseline[-5:]), 'UDP baseline failed'
        with open(out + '/probes.jsonl') as f:
            baseline = [json.loads(line) for line in f]
        assert len(baseline) >= 5 and all(x['ok'] for x in baseline[-5:]), 'baseline failed'
        before = cmd('conntrack', '-L', '-p', 'tcp', ns='n')
        with open(out + '/conntrack-before.txt', 'w') as f:
            f.write(before)
        fault = time.monotonic()
        if case in ('change_ip', 'change_ip_silent'):
            snat('192.0.2.3')
        if case in ('change_ip_silent', 'downstream_silent'):
            # Blackhole only old TCP tuples. A fresh bridge SYN remains usable.
            import re
            ports = re.findall(r'src=10\.77\.1\.2 dst=192\.0\.2\.2 sport=(\d+) dport=10000', before)
            assert ports, before
            for port in set(ports):
                # After deleting conntrack, replies to the old SNAT address are
                # locally delivered to the NAT router. Drop these too, otherwise
                # its kernel sends RST and the experiment is not silent loss.
                cmd('iptables', '-A', 'INPUT', '-p', 'tcp', '--sport', '10000',
                    '--dport', port, '-j', 'DROP', ns='n')
                cmd('iptables', '-A', 'FORWARD', '-p', 'tcp', '-d', '10.77.1.2',
                    '--dport', port, '-j', 'DROP', ns='n')
                if case == 'change_ip_silent':
                    cmd('iptables', '-A', 'FORWARD', '-p', 'tcp', '-s', '10.77.1.2',
                        '--sport', port, '-j', 'DROP', ns='n')
        deleted = cmd('conntrack', '-D', '-p', 'tcp', '-s', '10.77.1.2', '--dport', '10000', ns='n')
        fault_completed = time.monotonic()
        with open(out + '/fault.json', 'w') as f:
            json.dump({'case': case, 't': fault, 'completed': fault_completed,
                       'wall_completed': time.time(), 'deleted': deleted,
                       'kernel': cmd('uname', '-a'), 'tcp': cmd('sysctl', 'net.netfilter.nf_conntrack_tcp_loose',
                       'net.ipv4.tcp_keepalive_time', 'net.ipv4.tcp_keepalive_intvl', 'net.ipv4.tcp_keepalive_probes', ns='n')}, f, indent=2)
        print(case, 'fault injected', flush=True)
        time.sleep(window)
        for ns in ['p','b']:
            report=json.loads(cmd('/lab/xray','api','reversehealth','--server=127.0.0.1:10085',ns=ns))
            with open(out+'/'+ns+'-health.json','w') as f: json.dump(report,f,indent=2)
            assert any(w['state']=='READY' for w in report['workers']), 'health API has no ready worker'
            if ns=='p':
                assert report['canaries'] and report['canaries'][0]['state']=='AVAILABLE', 'normal-path canary failed'
        with open(out + '/conntrack-after.txt', 'w') as f:
            f.write(cmd('conntrack', '-L', '-p', 'tcp', ns='n'))
    finally:
        for p in reversed(processes):
            p.terminate()
        for p in processes:
            try:
                p.wait(timeout=3)
            except sp.TimeoutExpired:
                p.kill()
                p.wait()
        for ns in ('b', 'n', 'p'):
            cmd('ip', 'netns', 'del', ns, check=False)
    with open(out + '/probes.jsonl') as f:
        rows = [json.loads(line) for line in f]
    # Exclude requests started before the mutation finished (including in-flight
    # successes), and measure both first success and the final uninterrupted run.
    fault = fault_completed
    rows = [x for x in rows if x['t'] >= fault]
    successes = [x for x in rows if x['ok']]
    last_failure = max((x['t']+x['latency']-fault for x in rows if not x['ok']), default=None)
    summary = {'case': case, 'window': window, 'attempts': len(rows), 'successes': len(successes),
               'first_success_s': successes[0]['t']+successes[0]['latency']-fault if successes else None,
               'last_failure_s': last_failure,
               'tail_20_all_success': len(rows) >= 20 and all(x['ok'] for x in rows[-20:])}
    stable = [x for x in successes if last_failure is None or x['t']-fault > last_failure]
    summary['stable_recovery_s'] = (stable[0]['t']+stable[0]['latency']-fault
                                    if len(stable) >= 20 else None)
    with open(out+'/udp-probes.jsonl') as f:
        udp_rows = [json.loads(line) for line in f]
    udp_rows = [row for row in udp_rows if row['t']>=fault]
    last_bad = max((row['t']+row['latency'] for row in udp_rows if not row['ok']),default=fault)
    udp_stable = [row for row in udp_rows if row['ok'] and row['t']>last_bad]
    summary['udp'] = {'attempts':len(udp_rows),'successes':sum(row['ok'] for row in udp_rows),
        'stable_recovery_s':udp_stable[0]['t']+udp_stable[0]['latency']-fault if len(udp_stable)>=20 else None}
    with open(out+'/long-tcp.jsonl') as f:
        stream=[json.loads(line) for line in f]
    ended=next((row['t'] for row in stream if row['event']=='ended' and row['t']>=fault),None)
    summary['old_tcp_ended_s']=ended-fault if ended is not None else None
    summary['transport'] = transport
    summary['rtt_ms'] = delay
    with open(out + '/summary.json', 'w') as f:
        json.dump(summary, f, indent=2)
    print(json.dumps(summary), flush=True)
    assert summary["stable_recovery_s"] is not None and summary["udp"]["stable_recovery_s"] is not None, "TCP/UDP recovery not observed"


if __name__ == '__main__':
    if sys.argv[1] == 'long_tcp':
        long_tcp()
    elif sys.argv[1] == 'http_canary':
        http_canary()
    elif sys.argv[1] == 'udp_echo':
        udp_echo()
    elif sys.argv[1] == 'udp_probe':
        udp_probe()
    elif sys.argv[1] == 'tls_target':
        tls_target()
    elif sys.argv[1] == 'echo':
        echo()
    elif sys.argv[1] == 'probe':
        probe()
    else:
        main()
