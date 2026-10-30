#!/usr/bin/env python3
"""Run user-service acceptance in private Docker containers, with real production binaries."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import uuid


def command(*args, timeout=60):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT, timeout=timeout).strip()


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def write_json(path, value):
    temporary = path.with_name(path.name + '.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--console', type=Path, required=True)
    parser.add_argument('--node-fixture', type=Path, default=Path(__file__).with_name('node.py'))
    parser.add_argument('--support-fixture', type=Path)
    parser.add_argument('--node-arg', action='append', default=[])
    parser.add_argument('--geoip', type=Path, required=True)
    parser.add_argument('--geosite', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--image', default='brocade-user-service:local')
    parser.add_argument('--database-image', default='postgres:16-alpine')
    args = parser.parse_args()
    os.umask(0o077)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    inputs, results = output / 'inputs', output / 'results'
    inputs.mkdir()
    results.mkdir()
    for source, name in [(args.console, 'console'), (args.geoip, 'geoip.dat'), (args.geosite, 'geosite.dat'),
                         (args.node_fixture, 'node.py'), (Path(__file__), 'run.py')]:
        shutil.copy2(source.resolve(), inputs / name)
    if args.support_fixture:
        shutil.copy2(args.support_fixture.resolve(), inputs / 'support.py')
    (inputs / 'run-id').write_text(uuid.uuid4().hex)
    manifest = {'runtime': 'docker', 'console_sha256': digest(inputs / 'console'),
                'fixture_sha256': digest(inputs / 'node.py'), 'runner_sha256': digest(inputs / 'run.py'),
                'geoip_sha256': digest(inputs / 'geoip.dat'), 'geosite_sha256': digest(inputs / 'geosite.dat'),
                'image_id': command('docker', 'image', 'inspect', args.image, '--format', '{{.Id}}'),
                'database_image_id': command('docker', 'image', 'inspect', args.database_image, '--format', '{{.Id}}'),
                'git_head': command('git', 'rev-parse', 'HEAD')}
    manifest['node_arguments'] = args.node_arg
    if args.support_fixture:
        manifest['support_sha256'] = digest(inputs / 'support.py')
    write_json(output / 'manifest.json', manifest)
    prefix = 'brocade-service-' + uuid.uuid4().hex[:10]
    network, database, node = prefix + '-network', prefix + '-db', prefix + '-node'
    owned = []
    network_created = False
    try:
        command('docker', 'network', 'create', '--internal', network)
        network_created = True
        command('docker', 'run', '-d', '--name', database, '--network', network, '--network-alias', 'database',
                '--memory', '512m', '--pids-limit', '128', '-e', 'POSTGRES_USER=brocade',
                '-e', 'POSTGRES_DB=brocade_service', '-e', 'POSTGRES_HOST_AUTH_METHOD=trust', args.database_image)
        owned.append(database)
        for _ in range(100):
            if subprocess.run(['docker', 'exec', database, 'pg_isready', '-U', 'brocade'], capture_output=True, timeout=5).returncode == 0:
                break
            time.sleep(.2)
        else:
            raise RuntimeError('private PostgreSQL did not start')
        command('docker', 'run', '-d', '--name', node, '--network', network, '--cap-add', 'NET_ADMIN',
                '--memory', '3g', '--cpus', '2', '--pids-limit', '1024',
                '-e', 'BROCADE_USER_SERVICE_DISPOSABLE=1', '-v', f'{output}:/lab',
                args.image, 'sleep', 'infinity')
        owned.append(node)
        state = json.loads(command('docker', 'inspect', node))[0]
        write_json(output / 'isolation.json', {'privileged': state['HostConfig']['Privileged'],
            'cap_add': state['HostConfig']['CapAdd'], 'network_mode': state['HostConfig']['NetworkMode'],
            'pid_mode': state['HostConfig']['PidMode'], 'published_ports': state['HostConfig']['PortBindings'],
            'network_internal': json.loads(command('docker', 'network', 'inspect', network))[0]['Internal']})
        command('docker', 'exec', '-d', node, 'python3', '/lab/inputs/node.py', *args.node_arg)
        deadline, delivered, sql_id = time.monotonic() + 1200, 0, None
        while time.monotonic() < deadline:
            request = results / 'sql-request.json'
            if request.exists():
                value = json.loads(command('docker', 'exec', node, 'cat', '/lab/results/sql-request.json'))
                if value['id'] != sql_id:
                    query = value['query']
                    if value['data'] is not None:
                        data = json.dumps(value['data']).replace('\\', '\\\\')
                        query = 'CREATE TEMP TABLE lab_seed(data jsonb);\nCOPY lab_seed FROM STDIN;\n' + data + '\n\\.\n' + query
                    completed = subprocess.run(['docker', 'exec', '-i', database, 'psql', '-XqAt', '-U', 'brocade',
                        '-d', 'brocade_service', '-v', 'ON_ERROR_STOP=1'], input=query, text=True,
                        capture_output=True, timeout=30)
                    if completed.returncode:
                        (results / 'sql-error-private.txt').write_text(completed.stderr)
                    write_json(results / 'sql-response.json', {'id': value['id'], 'ok': completed.returncode == 0,
                               'result': completed.stdout if completed.returncode == 0 else ''})
                    sql_id = value['id']
            evidence = results / 'evidence.json'
            if evidence.exists():
                records = json.loads(command('docker', 'exec', node, 'cat', '/lab/results/evidence.json'))
                for record in records[delivered:]:
                    print(json.dumps(record), flush=True)
                delivered = len(records)
            if (results / 'result.json').exists():
                result = json.loads(command('docker', 'exec', node, 'cat', '/lab/results/result.json'))
                if not result['passed']:
                    raise RuntimeError('Docker acceptance failed; inspect private results/failure.txt')
                print(json.dumps({'passed': True, 'cases': len(result['events'])}), flush=True)
                return
            time.sleep(.2)
        raise RuntimeError('Docker acceptance exceeded 20 minutes')
    finally:
        for container in reversed(owned):
            (results / (container.rsplit('-', 1)[1] + '-container.log')).write_text(command('docker', 'logs', container))
            command('docker', 'rm', '-f', container)
        if network_created:
            command('docker', 'network', 'rm', network)
        command('docker', 'run', '--rm', '--network', 'none', '-v', f'{output}:/lab', args.image,
                'chown', '-R', f'{os.getuid()}:{os.getgid()}', '/lab')


if __name__ == '__main__':
    main()
