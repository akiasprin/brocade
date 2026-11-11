#!/usr/bin/env python3
"""Regress many independent user pools against a delayed, healthy mock Console in Docker."""
import argparse
import concurrent.futures
import datetime
import hashlib
import http.server
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import sys
import threading
import time


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def metrics(pid):
    proc = Path('/proc') / str(pid)
    fields = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
    status = dict(line.split(':', 1) for line in (proc / 'status').read_text().splitlines() if ':' in line)
    rollup = dict(line.split(':', 1) for line in (proc / 'smaps_rollup').read_text().splitlines() if ':' in line)
    return dict(cpu_seconds=(int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK'),
                pss_mib=int(rollup['Pss'].split()[0])/1024,
                rss_mib=int(status['VmRSS'].split()[0])/1024,
                threads=int(status['Threads']))


def guest():
    root = Path('/lab')
    config = json.loads((root / 'config.json').read_text())
    users = config['users']
    rows = [dict(source=f'user{i}@tenant#entry', budget_key=hashlib.sha256(str(i).encode()).hexdigest(),
                 effect=dict(mode='limited', rate_bps=100_000_000), revision=1) for i in range(users)]
    snapshot = dict(sources=rows, sha256=hashlib.sha256(json.dumps(rows).encode()).hexdigest())
    budget_calls = [0]
    call_lock = threading.Lock()

    class Server(http.server.ThreadingHTTPServer):
        request_queue_size = 512
        daemon_threads = True

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'
        def log_message(self, *args):
            pass
        def answer(self, status, value=None):
            body = json.dumps(value).encode() if value is not None else b''
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        def do_GET(self):
            self.answer(200, snapshot) if self.path == '/agent/v1/user-traffic/policies' else self.answer(404)
        def do_POST(self):
            body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
            if self.path == '/agent/v1/user-traffic/budget':
                request = json.loads(body)
                time.sleep(config['delay_ms']/1000)
                with call_lock:
                    budget_calls[0] += 1
                self.answer(200, dict(mode='limited', bytes=min(request['bytes'], 12_500_000), rate_bps=100_000_000, valid_ms=2000))
            elif self.path.endswith('/policies/ack'):
                self.answer(204)
            elif self.path.endswith('/support'):
                self.answer(200, True)
            else:
                self.answer(404)

    server = Server(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    reports = []
    for version in config['order']:
        state = root / (version + '-state')
        state.mkdir()
        token = state / 'node.token'
        token.write_text('synthetic-isolated-many-users-token')
        token.chmod(0o600)
        with (root / (version + '-agent.log')).open('w') as log:
            agent = subprocess.Popen([str(root/'inputs'/version), 'run', '--server', f'http://127.0.0.1:{server.server_port}',
                                      '--token-file', str(token), '--state-dir', str(state), '--apply', 'state-dir'],
                                     stdout=log, stderr=subprocess.STDOUT)
            try:
                deadline = time.monotonic() + 15
                path = state / 'user-traffic-control.json'
                while not path.exists():
                    if agent.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError('Agent did not publish its admission endpoint')
                    time.sleep(.05)
                time.sleep(1.5)
                control = json.loads(path.read_text())
                host, port = control['endpoint'].rsplit(':', 1)
                barrier = threading.Barrier(users, timeout=10)
                started = time.monotonic()
                started_at = now()
                before = metrics(agent.pid)
                calls_before = budget_calls[0]
                resource_rows = []
                stopped = threading.Event()
                def sample_resources():
                    while not stopped.wait(.25):
                        resource_rows.append(metrics(agent.pid))
                sampler = threading.Thread(target=sample_resources)
                sampler.start()
                def client(i):
                    latencies = []
                    errors = 0
                    completed = 0
                    with socket.create_connection((host, int(port)), 10) as connection:
                        connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                        connection.settimeout(10)
                        with connection.makefile('rb') as reader:
                            barrier.wait()
                            end = time.monotonic() + config['seconds']
                            request = (json.dumps(dict(token=control['token'], source=rows[i]['source'], bytes=16))+'\n').encode()
                            while time.monotonic() < end:
                                begin = time.monotonic()
                                connection.sendall(request)
                                response = reader.readline()
                                while response == b'WAIT\n':
                                    response = reader.readline()
                                elapsed = time.monotonic() - begin
                                completed += 1
                                if response != b'OK\n':
                                    errors += 1
                                if begin - started >= 5:
                                    latencies.append(elapsed*1000)
                                time.sleep(max(0, .001-elapsed))
                    return dict(user=i, completed=completed, errors=errors, warm_count=len(latencies),
                                warm_max_ms=max(latencies, default=None),
                                over100ms=sum(x>100 for x in latencies), over200ms=sum(x>200 for x in latencies),
                                over500ms=sum(x>500 for x in latencies))
                try:
                    with concurrent.futures.ThreadPoolExecutor(max_workers=users) as executor:
                        samples = list(executor.map(client, range(users)))
                finally:
                    stopped.set()
                    sampler.join()
                after = metrics(agent.pid)
                elapsed = time.monotonic()-started
                report = dict(version=version, started_at=started_at, ended_at=now(), elapsed_s=elapsed,
                              users=samples, warm_max_ms=max(r['warm_max_ms'] for r in samples),
                              errors=sum(r['errors'] for r in samples), over500ms=sum(r['over500ms'] for r in samples),
                              budget_requests=budget_calls[0]-calls_before,
                              resources=dict(cpu_percent_one_core=(after['cpu_seconds']-before['cpu_seconds'])/elapsed*100,
                                             cpu_seconds_per_100k_requests=(after['cpu_seconds']-before['cpu_seconds'])*100_000/sum(r['completed'] for r in samples),
                                             peak_pss_mib=max(r['pss_mib'] for r in resource_rows),
                                             peak_rss_mib=max(r['rss_mib'] for r in resource_rows),
                                             peak_threads=max(r['threads'] for r in resource_rows)))
                reports.append(report)
                print(json.dumps({k:v for k,v in report.items() if k!='users'}), flush=True)
            finally:
                agent.terminate()
                try:
                    agent.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    agent.kill()
                    agent.wait()
    candidate = next(r for r in reports if r['version'] == 'candidate')
    checks = dict(all_users_sampled=len(candidate['users'])==users and all(r['warm_count']>=100 for r in candidate['users']),
                  no_errors=candidate['errors']==0, no_remote_budget_requests=candidate['budget_requests']==0, warm_tail_under200ms=candidate['warm_max_ms']<200,
                  no_half_second_stalls=candidate['over500ms']==0)
    (root/'report.json').write_text(json.dumps(dict(config=config, runs=reports, checks=checks, passed=all(checks.values())), indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--agent', type=Path, required=True)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--image', required=True)
    parser.add_argument('--candidate-first', action='store_true')
    parser.add_argument('--users', type=int, default=96, choices=range(16, 129))
    parser.add_argument('--seconds', type=int, default=18, choices=range(12, 31))
    parser.add_argument('--delay-ms', type=int, default=80, choices=range(20, 201))
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    (root/'inputs').mkdir()
    inputs = {}
    for name, path in [('candidate', args.agent), ('baseline', args.baseline)]:
        if path is None:
            continue
        target = root/'inputs'/name
        shutil.copy2(path, target)
        target.chmod(0o755)
        with target.open('rb') as source:
            inputs[name] = dict(source=str(path.resolve()), sha256=hashlib.file_digest(source,'sha256').hexdigest())
    order = ['baseline','candidate'] if args.baseline else ['candidate']
    if args.candidate_first:
        order.reverse()
    (root/'config.json').write_text(json.dumps(dict(order=order, users=args.users, seconds=args.seconds, delay_ms=args.delay_ms, inputs=inputs), indent=2))
    shutil.copy2(__file__, root/'many_users.py')
    name = 'brocade-user-refills-'+secrets.token_hex(6)
    began = time.monotonic()
    timing = dict(started_at=now(), container=name)
    result = None
    try:
        result = subprocess.run(['docker','run','--rm','--name',name,'--network','none','--memory','2g','--pids-limit','4096',
                                 '-v',str(root)+':/lab','--entrypoint','python3',args.image,'/lab/many_users.py','guest'], timeout=180)
        result.check_returncode()
    finally:
        subprocess.run(['docker','rm','-f',name], capture_output=True)
        timing.update(ended_at=now(), elapsed_s=time.monotonic()-began, exit_code=result.returncode if result else None)
        (root/'timing.json').write_text(json.dumps(timing, indent=2))
    report = json.loads((root/'report.json').read_text())
    for label, info in inputs.items():
        with (root/'inputs'/label).open('rb') as source:
            assert hashlib.file_digest(source,'sha256').hexdigest()==info['sha256']
    print(json.dumps(dict(passed=report['passed'], checks=report['checks'])), flush=True)
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    if sys.argv[1:] == ['guest']:
        guest()
    else:
        raise SystemExit(main())
