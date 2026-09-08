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
    r = sp.run(prefix + list(args), capture_output=True, text=True, check=check)
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
        if os.environ.get('LAB_KEEPALIVE'):
            idle, interval = map(int, os.environ['LAB_KEEPALIVE'].split(','))
            bridge['outbounds'][1]['streamSettings'] = {
                'sockopt': {'tcpKeepAliveIdle': idle, 'tcpKeepAliveInterval': interval}}
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
        time.sleep(float(os.environ.get('LAB_WARMUP', '10')))
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
    with open(out + '/summary.json', 'w') as f:
        json.dump(summary, f, indent=2)
    print(json.dumps(summary), flush=True)


if __name__ == '__main__':
    if sys.argv[1] == 'echo':
        echo()
    elif sys.argv[1] == 'probe':
        probe()
    else:
        main()
