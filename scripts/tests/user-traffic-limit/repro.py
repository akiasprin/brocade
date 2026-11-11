#!/usr/bin/env python3
"""Disposable Docker reproduction using unmodified Brocade binaries and real PG."""
import argparse
import concurrent.futures
import datetime
import hashlib
import http.client
import http.server
import json
import math
import os
from pathlib import Path
import secrets
import socket
import socketserver
import subprocess
import sys
import threading
import time
import traceback
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / 'results'
IMAGE = ''


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def event(kind, **kw):
    row = dict(at=now(), mono_ns=time.monotonic_ns(), kind=kind, **kw)
    with (RESULTS / 'events.jsonl').open('a') as f:
        f.write(json.dumps(row) + '\n')
    print(json.dumps(row), flush=True)
    return row


def cmd(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs).stdout.strip()


def sql(text):
    db = (ROOT / 'db-name').read_text().strip()
    return cmd(['docker', 'exec', '-i', db, 'psql', '-X', '-q', '-v', 'ON_ERROR_STOP=1', '-U', 'lab', '-d', 'lab'], input=text)


def host():
    names = json.loads((ROOT / 'names.json').read_text()) if (ROOT / 'names.json').exists() else None
    if names:
        raise RuntimeError('experiment already has container identities; use a separate output directory')
    suffix = secrets.token_hex(5)
    names = {k: f'brocade-ul-latency-{suffix}-{k}' for k in ('net', 'db', 'lab')}
    (ROOT / 'names.json').write_text(json.dumps(names))
    (ROOT / 'db-name').write_text(names['db'])
    password = secrets.token_hex(24)
    tokens = {f'n{i}': 'broc_node_lab_' + secrets.token_hex(24) for i in range(1, 5)}
    private = ROOT / 'private.json'
    private.write_text(json.dumps(dict(password=password, tokens=tokens)))
    private.chmod(0o600)
    started = event('setup_start')
    try:
        cmd(['docker', 'network', 'create', '--internal', names['net']])
        cmd(['docker', 'run', '-d', '--name', names['db'], '--network', names['net'], '--memory', '512m',
             '-e', 'POSTGRES_USER=lab', '-e', 'POSTGRES_DB=lab', '-e', f'POSTGRES_PASSWORD={password}', 'postgres:16-bookworm'])
        for _ in range(150):
            p = subprocess.run(['docker', 'exec', names['db'], 'pg_isready', '-U', 'lab'], capture_output=True)
            if p.returncode == 0:
                break
            time.sleep(.2)
        else:
            raise RuntimeError('PostgreSQL did not become ready')
        cmd(['docker', 'run', '-d', '--name', names['lab'], '--network', 'container:' + names['db'],
             '--memory', '3g', '--pids-limit', '2048', '-v', f'{ROOT}:/lab', '-w', '/lab',
             '--entrypoint', 'python3', IMAGE, '/lab/repro.py', 'guest'])
        deadline = time.monotonic() + 90
        while not (RESULTS / 'console-ready').exists():
            if (RESULTS / 'failure.txt').exists():
                raise RuntimeError((RESULTS / 'failure.txt').read_text())
            if time.monotonic() > deadline:
                raise RuntimeError('Console startup timed out')
            time.sleep(.2)
        seed = """BEGIN;
INSERT INTO tenants(id,name) VALUES('platform.acme','User limit latency lab');
INSERT INTO users(tenant_id,id,uuid) VALUES('platform.acme','alice','01010101-0101-0101-0101-010101010101');
INSERT INTO apps(id,label,position) VALUES('app-lab','Latency lab',10);
INSERT INTO chains(id,app_id,tenant_id,name,position) VALUES('chain-lab','app-lab','platform.acme','Lab',0);
"""
        for i in range(1, 5):
            token_hash = hashlib.sha256(tokens[f'n{i}'].encode()).hexdigest()
            seed += f"""
INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,api_port,overlay,egress_allowed,dns_kind,dns_servers)
VALUES('n{i}','platform.acme','Lab {i}','192.0.2.{i}','10.66.0.{i}','synthetic-private-{i}','synthetic-public-{i}',51820,{11000+i},TRUE,TRUE,'servers','["1.1.1.1"]');
INSERT INTO node_lifecycle_state(node_id) VALUES('n{i}') ON CONFLICT DO NOTHING;
INSERT INTO node_agent_state(node_id,token_hash,token_prefix,token_created_at,agent_protocol_version) VALUES('n{i}','{token_hash}','broc_node_lab',now(),49) ON CONFLICT(node_id) DO UPDATE SET token_hash=excluded.token_hash,token_prefix=excluded.token_prefix,token_created_at=now(),agent_protocol_version=49;
INSERT INTO ingresses(id,app_id,chain_id,node_id,bind,port,transport_kind,reality_private_key,reality_public_key,reality_short_ids,reality_dest,reality_server_names,reality_flow,reality_fallback_mode)
VALUES('ing-n{i}','app-lab','chain-lab','n{i}','0.0.0.0',{24440+i},'vless-reality','synthetic-private','synthetic-public','["8337a0bf"]','www.example.com:443','["www.example.com"]','xtls-rprx-vision','custom-site');
INSERT INTO grants(app_id,tenant_id,user_id,ingress_id) VALUES('app-lab','platform.acme','alice','ing-n{i}');
"""
        for extra in (5,6,7):
            seed += f"""
INSERT INTO ingresses SELECT (jsonb_populate_record(NULL::ingresses,to_jsonb(i)||'{{"id":"ing-extra{extra}","port":{24500+extra}}}'::jsonb)).* FROM ingresses i WHERE id='ing-n1';
INSERT INTO grants(app_id,tenant_id,user_id,ingress_id) VALUES('app-lab','platform.acme','alice','ing-extra{extra}');
"""
        seed += """
INSERT INTO users(tenant_id,id,uuid) VALUES('platform.acme','bob','02020202-0202-0202-0202-020202020202');
INSERT INTO grants(app_id,tenant_id,user_id,ingress_id) VALUES('app-lab','platform.acme','bob','ing-n1');
INSERT INTO apps(id,label,position) VALUES('app-other','Independent app',11);
INSERT INTO chains(id,app_id,tenant_id,name,position) VALUES('chain-other','app-other','platform.acme','Other',0);
INSERT INTO ingresses SELECT (jsonb_populate_record(NULL::ingresses,to_jsonb(i)||'{"id":"ing-other","app_id":"app-other","chain_id":"chain-other","port":25009}'::jsonb)).* FROM ingresses i WHERE id='ing-n1';
INSERT INTO grants(app_id,tenant_id,user_id,ingress_id) VALUES('app-other','platform.acme','alice','ing-other');
COMMIT;"""
        sql(seed)
        (RESULTS / 'seeded').touch()
        event('setup_end', elapsed_s=(time.monotonic_ns()-started['mono_ns'])/1e9)
        last_report = time.monotonic()
        seen_case = ''
        workload_deadline = time.monotonic() + 900
        while not (RESULTS / 'complete.json').exists():
            if time.monotonic() > workload_deadline:
                raise RuntimeError('acceptance workload exceeded its 900-second deadline')
            if (RESULTS / 'failure.txt').exists():
                raise RuntimeError((RESULTS / 'failure.txt').read_text())
            request_path = RESULTS / 'policy-request.json'
            if request_path.exists():
                request = json.loads(request_path.read_text())
                rate = request['rate']
                statement = (f"INSERT INTO user_app_rate_limits(tenant_id,user_id,app_id,base_rate_bps) VALUES('platform.acme','alice','app-lab',{rate}) ON CONFLICT(tenant_id,user_id,app_id) DO UPDATE SET base_rate_bps=excluded.base_rate_bps,revision=user_app_rate_limits.revision+1;" if rate else "DELETE FROM user_app_rate_limits WHERE app_id='app-lab' AND user_id='alice';")
                if 'available' in request:
                    statement += "UPDATE users SET service_suspended="+('false' if request['available'] else 'true')+" WHERE tenant_id='platform.acme' AND id='alice';"
                sql(statement)
                request_path.unlink()
                (RESULTS / 'policy-ack').write_text(request['case'])
            status = RESULTS / 'status.json'
            if status.exists():
                current = json.loads(status.read_text())
                if current['case'] != seen_case:
                    print(json.dumps(current), flush=True)
                    seen_case = current['case']
            if time.monotonic()-last_report >= 25:
                print(json.dumps(dict(at=now(), phase='running', latest_case=seen_case)), flush=True)
                last_report = time.monotonic()
            if cmd(['docker', 'inspect', '-f', '{{.State.Running}}', names['lab']]) != 'true':
                raise RuntimeError('Docker guest exited before completion')
            time.sleep(.2)
        print((RESULTS / 'complete.json').read_text(), flush=True)
    finally:
        for label in ('lab', 'db'):
            p = subprocess.run(['docker', 'logs', names[label]], capture_output=True, text=True)
            (RESULTS / (label + '-docker.log')).write_text(p.stdout+p.stderr)
            subprocess.run(['docker', 'rm', '-f', names[label]], capture_output=True)
        subprocess.run(['docker', 'network', 'rm', names['net']], capture_output=True)
        event('cleanup_end')


class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        while True:
            b = self.request.recv(65536)
            if not b:
                return
            self.request.sendall(b)


class ThreadServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def exact(s, n):
    out = b''
    while len(out) < n:
        b = s.recv(n-len(out))
        if not b:
            raise RuntimeError('unexpected EOF')
        out += b
    return out


def vless(node,udp=False):
    s = socket.create_connection(('127.0.0.1', 24000+node), 10)
    s.settimeout(10)
    s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    s.sendall(b'\x00' + bytes([2 if node==8 else 1])*16 + bytes([0,2 if udp else 1]) + (45001).to_bytes(2,'big') + b'\x01\x7f\x00\x00\x01')
    return s


def stats(samples):
    a = sorted(x['latency_ms'] for x in samples if x.get('ok'))
    def q(p):
        return round(a[min(len(a)-1, math.ceil(len(a)*p)-1)],3) if a else None
    return dict(count=len(samples), errors=sum(not x.get('ok') for x in samples), p50_ms=q(.5), p95_ms=q(.95), p99_ms=q(.99), max_ms=round(max(a),3) if a else None,
                over100ms=sum(x>100 for x in a), over500ms=sum(x>500 for x in a), over1000ms=sum(x>1000 for x in a))


def guest():
    processes = []
    budget_records = []
    record_lock = threading.Lock()
    current_case = ['setup']
    budget_delay = [0]
    offline = [False]
    try:
        private = json.loads((ROOT / 'private.json').read_text())
        env = dict(os.environ, DATABASE_URL=f"postgres://lab:{private['password']}@127.0.0.1:5432/lab", BROCADE_ADMIN_BIND='127.0.0.1:18080', BROCADE_CACHE_DIR='/lab/results/cache', BROCADE_AGENT_PUBLIC_URL='http://127.0.0.1:18080', BROCADE_PROBE_RUNTIME_DIR='/lab/results/probes')
        def start(binary, args, log, env=None):
            f = (RESULTS / log).open('w')
            p = subprocess.Popen([str(ROOT/'inputs'/binary), *args], stdout=f, stderr=subprocess.STDOUT, env=env)
            processes.append(p)
            return p
        start('console', [], 'console.log', env)
        for _ in range(300):
            try:
                urllib.request.urlopen('http://127.0.0.1:18080/auth/state', timeout=.2).read()
                break
            except urllib.error.HTTPError:
                break
            except (OSError, urllib.error.URLError):
                time.sleep(.1)
        else:
            raise RuntimeError('Console not listening')
        (RESULTS / 'console-ready').touch()
        while not (RESULTS / 'seeded').exists():
            time.sleep(.1)

        class Proxy(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def log_message(self, *_):
                pass
            def do_any(self):
                b = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                started = time.monotonic_ns()
                if self.path == '/agent/v1/user-traffic/budget':
                    time.sleep(budget_delay[0])
                if offline[0] and self.path.startswith('/agent/v1/user-traffic/'):
                    self.send_response(503);self.send_header('Content-Length','0');self.end_headers();return
                c = http.client.HTTPConnection('127.0.0.1',18080,timeout=10)
                headers = {k:v for k,v in self.headers.items() if k.lower() not in ('host','connection','upgrade')}
                if 'live' in self.path or 'realtime' in self.path or self.headers.get('Upgrade'):
                    self.send_response(404);self.send_header('Content-Length','0');self.end_headers();return
                try:
                    c.request(self.command,self.path,body=b,headers=headers)
                    r=c.getresponse(); out=r.read(); status=r.status
                    self.send_response(status)
                    self.send_header('Content-Type','application/json')
                    self.send_header('Content-Length',str(len(out)))
                    self.end_headers();self.wfile.write(out)
                    if self.path == '/agent/v1/user-traffic/budget':
                        row=dict(at=now(),mono_ns=started,case=current_case[0],http_ms=(time.monotonic_ns()-started)/1e6,status=status,source=json.loads(b).get('source'),request_bytes=json.loads(b).get('bytes'),grant=json.loads(out))
                        with record_lock:
                            budget_records.append(row)
                            with (RESULTS/'budgets.jsonl').open('a') as f: f.write(json.dumps(row)+'\n')
                finally:
                    c.close()
            do_GET=do_POST=do_PUT=do_DELETE=do_any
        proxy=http.server.ThreadingHTTPServer(('127.0.0.1',18081),Proxy)
        threading.Thread(target=proxy.serve_forever,daemon=True).start()
        echo=ThreadServer(('127.0.0.1',45001),Echo)
        threading.Thread(target=echo.serve_forever,daemon=True).start()
        udp_echo=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);udp_echo.bind(('127.0.0.1',45001))
        def udp_loop():
            while True:
                data,peer=udp_echo.recvfrom(65536);udp_echo.sendto(data,peer)
        threading.Thread(target=udp_loop,daemon=True).start()
        for i in range(1,5):
            state=RESULTS/f'node{i}'
            state.mkdir()
            token_file=state/'node.token';token_file.write_text(private['tokens'][f'n{i}']);token_file.chmod(0o600)
            start('agent',['run','--server','http://127.0.0.1:18081','--token-file',str(token_file),'--state-dir',str(state),'--apply','state-dir'],f'agent{i}.log')
            control=state/'user-traffic-control.json'
            for _ in range(150):
                if control.exists(): break
                time.sleep(.1)
            else: raise RuntimeError(f'Agent {i} did not publish user control')
            cfg={'log':{'loglevel':'warning'},'inbounds':[{'tag':'entry','listen':'127.0.0.1','port':24000+i,'protocol':'vless','settings':{'decryption':'none','clients':[{'id':'01010101-0101-0101-0101-010101010101','email':f'alice@platform.acme#ing-n{i}'}]}}],'outbounds':[{'protocol':'freedom','tag':'direct'}]}
            if i==1:
                for extra in (5,6,7):
                    cfg['inbounds'].append({'tag':f'entry-extra{extra}','listen':'127.0.0.1','port':24000+extra,'protocol':'vless','settings':{'decryption':'none','clients':[{'id':'01010101-0101-0101-0101-010101010101','email':f'alice@platform.acme#ing-extra{extra}'}]}})
            if i==1:
                for worker,user,identity in [(8,'bob',2),(9,'alice',1)]:
                    email=f"{user}@platform.acme#"+('ing-n1' if worker==8 else 'ing-other')
                    cfg['inbounds'].append({'tag':f'isolated-{worker}','listen':'127.0.0.1','port':24000+worker,'protocol':'vless','settings':{'decryption':'none','clients':[{'id':f'{identity:02x}'*4+'-'+f'{identity:02x}'*2+'-'+f'{identity:02x}'*2+'-'+f'{identity:02x}'*2+'-'+f'{identity:02x}'*6,'email':email}]}})
            config=state/'manual-xray.json';config.write_text(json.dumps(cfg))
            xenv=dict(os.environ,XRAY_BROCADE_USER_LIMIT_FILE=str(control))
            start('xray',['run','-config',str(config)],f'xray{i}.log',xenv)
            for _ in range(100):
                try:
                    socket.create_connection(('127.0.0.1',24000+i),.1).close();break
                except OSError:time.sleep(.05)
            else:raise RuntimeError(f'Xray {i} did not listen')
        event('services_ready')
        summaries=[]

        def run_case(name,rate,nodes,seconds=20,period=.001,reconnect=False,stagger=0,payload=16,delay=0,outage=False,udp=False,hot=False):
            current_case[0]=name
            budget_delay[0]=delay
            (RESULTS/'policy-ack').unlink(missing_ok=True)
            temp=RESULTS/'policy-request.tmp';temp.write_text(json.dumps(dict(case=name,rate=rate)));temp.replace(RESULTS/'policy-request.json')
            while not (RESULTS/'policy-ack').exists():time.sleep(.05)
            time.sleep(2.3)
            case_start=event('case_start',case=name,rate_bps=rate,nodes=nodes,seconds=seconds,period_s=period,reconnect=reconnect,payload_bytes=payload)
            (RESULTS/'status.json').write_text(json.dumps(dict(case=name,at=now())))
            phase_threads=[]
            def faults():
                time.sleep(3);offline[0]=True;event('outage_start',case=name)
                time.sleep(3);offline[0]=False;event('outage_end',case=name)
            def changes():
                previous=0
                for at,rate,available in [(3,1_000_000,True),(6,None,True),(9,100_000_000,False),(12,100_000_000,True)]:
                    time.sleep(at-previous);previous=at
                    (RESULTS/'policy-ack').unlink(missing_ok=True)
                    request=dict(case=name,rate=rate,available=available)
                    temp=RESULTS/'policy-request.tmp';temp.write_text(json.dumps(request));temp.replace(RESULTS/'policy-request.json')
                    while not (RESULTS/'policy-ack').exists():time.sleep(.02)
                    event('policy_change',case=name,rate_bps=rate,available=available)
            for enabled,fn in [(outage,faults),(hot,changes)]:
                if enabled:
                    t=threading.Thread(target=fn);t.start();phase_threads.append(t)
            barrier=threading.Barrier(len(nodes))
            rows=[]
            def worker(node):
                barrier.wait();time.sleep(stagger*(node-1))
                size=payload[node] if isinstance(payload,dict) else payload
                spacing=period[node] if isinstance(period,dict) else period
                deadline=time.monotonic()+seconds
                s=None;first=True;idx=0
                while time.monotonic()<deadline:
                    started=time.monotonic_ns()
                    row=dict(case=name,node=node,index=idx,at=now(),mono_ns=started,bytes=size)
                    try:
                        if s is None:s=vless(node,udp);first=True
                        data=bytes([idx%256])*size
                        s.sendall((size.to_bytes(2,'big') if udp else b'')+data)
                        if first:
                            assert exact(s,2)==b'\x00\x00'
                            first=False
                        if udp:assert int.from_bytes(exact(s,2),'big')==size
                        assert exact(s,size)==data
                        row['ok']=True
                    except Exception as e:
                        row.update(ok=False,error=str(e))
                        if s:s.close()
                        s=None
                    row['latency_ms']=(time.monotonic_ns()-started)/1e6
                    with record_lock:
                        rows.append(row)
                        with (RESULTS/'samples.jsonl').open('a') as f:f.write(json.dumps(row)+'\n')
                    if reconnect and s:s.close();s=None
                    idx+=1
                    if spacing: time.sleep(max(0,spacing-(time.monotonic_ns()-started)/1e9))
                if s:s.close()
            with concurrent.futures.ThreadPoolExecutor(max_workers=len(nodes)) as pool:
                list(pool.map(worker,nodes))
            for t in phase_threads:t.join()
            summary=dict(case=name,rate_bps=rate,nodes=nodes,elapsed_s=(time.monotonic_ns()-case_start['mono_ns'])/1e9,**stats(rows))
            summary['per_node']={str(n):stats([r for r in rows if r['node']==n]) for n in nodes}
            with record_lock: br=[r for r in budget_records if r['case']==name]
            warm=[r for r in rows if r['index']>10]
            summary['warm']=stats(warm)
            summary['warm_per_stream']={str(n):stats([r for r in warm if r['node']==n]) for n in nodes}
            summary['payload_bytes']=payload
            summary['period_s']=period
            summary['actual_payload_bps']=sum(r['bytes']*2*8 for r in rows if r['ok'])/summary['elapsed_s']
            summary['budget_requests']=len(br)
            summary['budget_retries']=sum(r['grant'].get('mode')=='retry' for r in br)
            summary['max_budget_http_ms']=max((r['http_ms'] for r in br),default=0)
            summaries.append(summary)
            (RESULTS/'summary.json').write_text(json.dumps(summaries,indent=2))
            event('case_end',**summary)

        run_case('unlimited_shared_1ms',None,[1,5,6,7],8)
        run_case('limited_single_1ms',100_000_000,[1],10)
        run_case('limited_shared_1ms',100_000_000,[1,5,6,7],20)
        run_case('limited_four_nodes_1ms',100_000_000,[1,2,3,4],15)
        run_case('limited_shared_no_sleep',100_000_000,[1,5,6,7],20,period=0,payload=8)
        run_case('limited_shared_rtt80',100_000_000,[1,5,6,7],20,delay=.08)
        run_case('mixed_bulk_small',100_000_000,[1,5],20,period={1:0,5:.001},payload={1:16384,5:16})
        run_case('isolated_user_app',1000,[1,8,9],12)
        run_case('full_single_tcp',10_000_000,[1],20,period=0,payload=16384)
        run_case('full_four_tcp',10_000_000,[1,2,3,4],20,period=0,payload=16384)
        run_case('full_four_udp',1_000_000,[1,2,3,4],20,period=0,payload=1024,udp=True)
        run_case('outage_recovery',100_000_000,[1,5,6,7],12,period=.01,outage=True)
        run_case('hot_policy_existing_connections',100_000_000,[1,5],16,period=0,payload=4096,hot=True)
        run_case('unlimited_after',None,[1,5,6,7],8)
        (RESULTS/'complete.json').write_text(json.dumps(dict(at=now(),summaries=summaries),indent=2))
    except Exception:
        (RESULTS/'failure.txt').write_text(traceback.format_exc())
        raise
    finally:
        for p in reversed(processes):
            p.terminate()
        for p in reversed(processes):
            try:p.wait(timeout=5)
            except subprocess.TimeoutExpired:p.kill();p.wait()


if __name__=='__main__':
    if len(sys.argv)>1 and sys.argv[1]=='guest':guest()
    else:
        parser=argparse.ArgumentParser(description=__doc__)
        parser.add_argument('--output',type=Path,required=True)
        parser.add_argument('--agent',type=Path,required=True)
        parser.add_argument('--console',type=Path,required=True)
        parser.add_argument('--xray',type=Path,required=True)
        parser.add_argument('--image',required=True,help='Linux image with Python 3 and GNU libc, e.g. prepared Ubuntu 24.04 lab image')
        args=parser.parse_args();ROOT=args.output.resolve();RESULTS=ROOT/'results';IMAGE=args.image
        ROOT.mkdir(parents=True,exist_ok=False);RESULTS.mkdir();(ROOT/'inputs').mkdir()
        import shutil
        shutil.copy2(__file__,ROOT/'repro.py')
        inputs={}
        for name in ('agent','console','xray'):
            source=getattr(args,name).resolve();target=ROOT/'inputs'/name;shutil.copy2(source,target);target.chmod(0o755)
            inputs[name]=dict(source=str(source),sha256=hashlib.file_digest(target.open('rb'),'sha256').hexdigest())
        (ROOT/'inputs.json').write_text(json.dumps(inputs,indent=2))
        host()
