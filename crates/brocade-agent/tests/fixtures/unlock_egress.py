"""Actual Agent -> private Xray -> IPv4 relay -> IPv6 exit failures, Docker only."""
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time

assert Path('/.dockerenv').exists() and os.environ.get('BROCADE_PROBE_EGRESS_DOCKER') == '1'
binary, agent, directory, mode = sys.argv[1:]
root = Path(directory)
root.mkdir(mode=0o700, parents=True, exist_ok=True)
assert mode in ['no-address', 'address-without-route']
subprocess.run(['ip', 'link', 'set', 'lo', 'up'], check=True)
if mode == 'address-without-route':
    subprocess.run(['ip', '-6', 'addr', 'add', '2001:db8:1::2/128', 'dev', 'lo'], check=True)
used = set()
def port():
    while True:
        with socket.socket() as connection:
            connection.bind(('127.0.0.1', 0))
            value = connection.getsockname()[1]
        if value not in used:
            used.add(value)
            return value
entry, exit_port = port(), port()
ports = {part: port() for part in ['webpage', 'premium', 'cdn']}
uuid = '00000000-0000-4000-8000-000000000006'
def inbound(listen_port):
    return {'listen': '127.0.0.1', 'port': listen_port, 'protocol': 'vless', 'settings': {'decryption': 'none', 'clients': [{'id': uuid, 'email': 'probe#v6:fixture'}]}}
def forward(tag, listen_port):
    return {'tag': tag, 'protocol': 'vless', 'settings': {'vnext': [{'address': '127.0.0.1', 'port': listen_port, 'users': [{'id': uuid, 'encryption': 'none'}]}]}}
exit_config = {
    'log': {'loglevel': 'debug'},
    'dns': {'hosts': {'www.youtube.com': '2606:4700:4700::1111', 'redirector.googlevideo.com': '2606:4700:4700::1111'}},
    'inbounds': [inbound(exit_port)],
    'outbounds': [{'tag': 'out:egress', 'protocol': 'freedom', 'settings': {'domainStrategy': 'UseIP'}}, {'tag': 'out:egress:probe-v6', 'protocol': 'freedom', 'settings': {'domainStrategy': 'ForceIPv6'}}],
    'routing': {'rules': [{'type': 'field', 'user': ['probe#v6:fixture'], 'outboundTag': 'out:egress:probe-v6'}]},
}
entry_config = {'log': {'loglevel': 'debug'}, 'inbounds': [inbound(entry)], 'outbounds': [forward('remote-ipv4', exit_port)]}
client_config = {'log': {'loglevel': 'debug'}, 'inbounds': [{'tag': 'probe:request:youtube_premium.' + part, 'listen': '127.0.0.1', 'port': value, 'protocol': 'socks', 'settings': {'auth': 'noauth'}} for part, value in ports.items()], 'outbounds': [forward('selected', entry)]}
trace = root / 'connections.jsonl'
trace.write_bytes(b'')
trace.chmod(0o600)
processes, logs = [], []
try:
    for name, config in [('exit', exit_config), ('entry', entry_config), ('client', client_config)]:
        file = root / (name + '.json')
        file.write_text(json.dumps(config))
        environment = os.environ.copy()
        if name == 'client':
            environment['XRAY_BROCADE_PROBE_FILE'] = str(trace)
        else:
            identity = root / (name + '.identity')
            identity.write_text('docker-' + name)
            identity.chmod(0o600)
            environment['XRAY_BROCADE_PROBE_IDENTITY_FILE'] = str(identity)
            freedom = [outbound for outbound in config['outbounds'] if outbound['protocol'] == 'freedom']
            projection = {'dns': config.get('dns'), 'outbounds': freedom}
            fingerprint = hashlib.sha256(json.dumps(projection, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
            environment['XRAY_BROCADE_PROBE_PROFILE'] = json.dumps({'fingerprint': fingerprint, 'strategies': {'out:egress': 'UseIP'}})
        log = (root / (name + '.log')).open('w')
        logs.append(log)
        process = subprocess.Popen([binary, 'run', '-config', str(file)], env=environment, stdout=log, stderr=log)
        processes.append(process)
        limit = time.monotonic() + 5
        while True:
            assert process.poll() is None, name + ' exited'
            try:
                with socket.create_connection(('127.0.0.1', config['inbounds'][0]['port']), timeout=.1):
                    break
            except OSError:
                assert time.monotonic() < limit, name + ' did not listen'
                time.sleep(.02)
    descriptor = root / 'agent-context.json'
    descriptor.write_text(json.dumps({'ports': ports, 'trace': str(trace), 'mode': mode, 'output': str(root / 'result.json')}))
    environment = {**os.environ, 'BROCADE_PROBE_EGRESS_TEST': str(descriptor)}
    run = subprocess.run([agent, '--exact', 'unlock_probe::dial_failure_tests::actual_xray_dial_policy_in_docker', '--ignored', '--nocapture'], env=environment, capture_output=True, text=True, timeout=90)
    (root / 'agent.log').write_text(run.stdout + run.stderr)
    assert run.returncode == 0 and '1 passed' in run.stdout, 'actual Agent test failed; inspect protected fixture logs'
    result = json.loads((root / 'result.json').read_text())
    print(json.dumps({'mode': mode, 'passed': True, 'status': result['status'], 'code': result['code'], 'requests': result['connection_requests'], 'actual_connections': len(result['connection_observations']), 'xray_sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest()}), flush=True)
finally:
    for process in reversed(processes):
        process.terminate()
        process.wait(timeout=5)
    for log in logs:
        log.close()
