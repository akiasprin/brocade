#!/usr/bin/env python3
"""Deploy frozen current production artifacts only into the owned KVM lab."""

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import secrets
import shlex
import shutil
import tempfile
import time
import urllib.request

from release_kvm_lab import Lab


AGENT_ONLY_LAYOUT = """set -eu
prefix=$1
for path in "$prefix" /data/cubelet/storage /opt/brocade/cvm /usr/local/bin/brocade-agent \\
  /usr/local/lib/brocade/cvm-stack.py /etc/brocade/cvm-stack-update.pub \\
  /etc/systemd/system/brocade-agent.service /etc/systemd/system/brocade-lab-agent.service; do
  test ! -e "$path" && test ! -L "$path" || exit 1
done
test ! -L /data && test ! -L /data/cubelet && test ! -L /var/lib/brocade || exit 1
mkdir -p /var/lib/brocade
mkdir -m 0700 "$prefix"
mkdir -p /data/cubelet/storage
"""


def copy_pinned_input(source, expected, target):
    if source.is_symlink() or not source.is_file():
        raise RuntimeError(f'pinned input is not a regular file: {source}')
    with source.open('rb') as stream:
        if hashlib.file_digest(stream, 'sha256').hexdigest() != expected:
            raise RuntimeError(f'pinned input differs from its SHA-256: {source}')
    if target.is_symlink():
        raise RuntimeError(f'deployment input must not be a symlink: {target}')
    if target.exists():
        with target.open('rb') as stream:
            if hashlib.file_digest(stream, 'sha256').hexdigest() != expected:
                raise RuntimeError(f'deployment input differs from its SHA-256: {target}')
        return
    with tempfile.TemporaryDirectory(dir=target.parent) as directory:
        staged = Path(directory) / target.name
        shutil.copyfile(source, staged)
        staged.chmod(0o600)
        with staged.open('rb') as stream:
            if hashlib.file_digest(stream, 'sha256').hexdigest() != expected:
                raise RuntimeError(f'deployment input differs from its SHA-256: {target}')
        staged.replace(target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--agent-only-compute', action='store_true', help='prepare compute without a Cube installation')
    pinned_options = ('geoip', 'geosite', 'probe')
    for name in pinned_options:
        parser.add_argument('--' + name, type=Path)
        parser.add_argument('--' + name + '-sha256')
    for name in ('lab_path', 'build_path', 'output_path'):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    for name in pinned_options:
        if (getattr(args, name) is None) != (getattr(args, name + '_sha256') is None):
            parser.error(f'--{name} and --{name}-sha256 must be provided together')
    lab = Lab(args.lab_path)
    build_path, output_path = args.build_path, args.output_path
    if lab.state.get('agent_only_compute'):
        raise RuntimeError('preserved existing Agent-only deployment; use a fresh lab')
    if args.agent_only_compute:
        lab.verify_owned()
        if 'prefix' in lab.state or 'credentials_file' in lab.state:
            raise RuntimeError('Agent-only compute requires a new disposable lab deployment')
    output_path.mkdir(mode=0o700, exist_ok=not args.agent_only_compute)
    inputs = output_path / 'inputs'
    inputs.mkdir(mode=0o700, exist_ok=True)
    bundle = json.loads((build_path / 'target/cube/artifacts/x86_64/bundle.json').read_text())
    paths = {
        'console': build_path / 'target/brocade-release/cvm/cargo/x86_64-unknown-linux-gnu/release/brocade-console',
        'agent': build_path / 'target/brocade-agent-build/cvm/x86_64-unknown-linux-musl/release/brocade-agent',
        'bundle.tar.gz': Path(bundle['bundle']),
        'cvm_stack.py': build_path / 'scripts/cvm_stack.py',
        'proxy.py': Path(__file__).with_name('release_proxy.py'),
    }
    hashes = {}
    for name, source in paths.items():
        target = inputs / name
        if not target.exists():
            shutil.copyfile(source, target)
            target.chmod(0o600)
        digest = hashlib.file_digest(target.open('rb'), 'sha256').hexdigest()
        assert digest == hashlib.file_digest(source.open('rb'), 'sha256').hexdigest()
        hashes[name] = digest
    for name in ('geoip.dat', 'geosite.dat'):
        target = inputs / name
        source = getattr(args, name.removesuffix('.dat'))
        if source is not None:
            copy_pinned_input(source, getattr(args, name.removesuffix('.dat') + '_sha256'), target)
        elif not target.exists():
            url = 'https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release/' + name
            with urllib.request.urlopen(url, timeout=120) as response, target.open('wb') as stream:
                shutil.copyfileobj(response, stream)
            target.chmod(0o600)
        assert target.stat().st_size > 0
        hashes[name] = hashlib.file_digest(target.open('rb'), 'sha256').hexdigest()
    if args.probe is not None:
        copy_pinned_input(args.probe, args.probe_sha256, inputs / 'probe')
        hashes['probe'] = args.probe_sha256
    assert hashes['bundle.tar.gz'] == bundle['sha256']
    (output_path / 'input-hashes.json').write_text(json.dumps(hashes, indent=2))
    prefix = '/var/lib/brocade/acceptance-' + lab.state['task']
    lab.state['prefix'] = prefix
    credentials = output_path / 'credentials.json'
    if credentials.exists():
        creds = json.loads(credentials.read_text())
    else:
        creds = {
            'db_password': secrets.token_hex(24),
            'bootstrap': secrets.token_hex(32),
            'secret': base64.b64encode(secrets.token_bytes(32)).decode(),
            'operator_id': 'acceptance-admin',
            'password': secrets.token_urlsafe(24),
        }
        credentials.write_text(json.dumps(creds))
        credentials.chmod(0o600)
    lab.state['credentials_file'] = str(credentials)
    lab.save()

    def install(role):
        agent_only = role == 'compute' and args.agent_only_compute
        if agent_only:
            lab.remote(role, shlex.join(['sudo', 'sh', '-s', '--', prefix]), data=AGENT_ONLY_LAYOUT.encode())
        else:
            lab.remote(role, f'sudo mkdir -p {prefix} /usr/local/lib/brocade /data/cubelet/storage')
        for name in ('geoip.dat', 'geosite.dat'):
            lab.upload(role, inputs / name, prefix + '/' + name, 0o600)
        lab.upload(role, inputs / 'agent', '/usr/local/bin/brocade-agent')
        script = 'set -eu\n'
        if agent_only:
            # Create this image exclusively; never reuse or reformat an existing volume.
            script += f"""! mountpoint -q /data/cubelet/storage || exit 1
(set -C; : > {prefix}/storage.img)
"""
        script += f"""\
if ! mountpoint -q /data/cubelet/storage; then
 truncate -s 20G {prefix}/storage.img
 mkfs.ext4 -F -E nodiscard {prefix}/storage.img
 mount -o loop {prefix}/storage.img /data/cubelet/storage
fi
"""
        if not agent_only:
            lab.upload(role, inputs / 'bundle.tar.gz', prefix + '/bundle.tar.gz', 0o600)
            lab.upload(role, inputs / 'cvm_stack.py', '/usr/local/lib/brocade/cvm-stack.py')
            script += f"""\
python3 /usr/local/lib/brocade/cvm-stack.py install --bundle {prefix}/bundle.tar.gz --sha256 {hashes['bundle.tar.gz']}
python3 /usr/local/lib/brocade/cvm-stack.py status
"""
        result = lab.remote(role, 'sudo sh -s', data=script.encode(), timeout=600, check=False)
        (output_path / (role + '-runtime.log')).write_bytes(result.stdout + result.stderr)
        if result.returncode:
            raise RuntimeError(role + ' runtime preparation failed; see private log')
        print(
            'PASS: ' + ('Agent and empty storage prepared on ' if agent_only else 'Cube installed on ') + role,
            flush=True,
        )

    with ThreadPoolExecutor(max_workers=2) as pool:
        list(pool.map(install, ('compute', 'provider')))
    lab.upload('compute', inputs / 'console', prefix + '/console')
    lab.upload('compute', inputs / 'proxy.py', prefix + '/proxy.py')
    # All credentials stay in stdin or mode-0600 files, outside command arguments.
    sql = (
        'CREATE USER brocade WITH PASSWORD '
        + "'"
        + creds['db_password']
        + "';\nCREATE DATABASE brocade OWNER brocade;\n"
    )
    lab.remote('compute', 'sudo -u postgres psql -v ON_ERROR_STOP=1', data=sql.encode())
    env = (
        'DATABASE_URL=postgres://brocade:' + creds['db_password'] + '@127.0.0.1/brocade\n'
        'BROCADE_BOOTSTRAP_TOKEN=' + creds['bootstrap'] + '\n'
        'BROCADE_SECRET_KEY=' + creds['secret'] + '\n'
        'BROCADE_ADMIN_BIND=0.0.0.0:8080\n'
        'BROCADE_AGENT_PUBLIC_URL=http://' + lab.state['roles']['compute']['ipv4'] + ':18081\n'
    )
    lab.python(
        'compute',
        'from pathlib import Path;import sys,os;p=Path('
        + repr(prefix + '/console.env')
        + ');p.write_bytes(sys.stdin.buffer.read());os.chmod(p,0o600)',
        env.encode(),
    )
    unit = (
        '[Unit]\nAfter=network-online.target postgresql.service\n[Service]\nEnvironmentFile='
        + prefix
        + '/console.env\nExecStart='
        + prefix
        + '/console\nRestart=on-failure\n[Install]\nWantedBy=multi-user.target\n'
    )
    lab.python(
        'compute',
        'from pathlib import Path;import sys;Path("/etc/systemd/system/brocade-lab-console.service").write_bytes(sys.stdin.buffer.read())',
        unit.encode(),
    )
    lab.remote('compute', 'sudo systemctl daemon-reload && sudo systemctl start brocade-lab-console')
    origin = 'http://127.0.0.1:' + str(lab.state['console_port'])
    for _ in range(120):
        try:
            urllib.request.urlopen(origin + '/healthz', timeout=2).close()
            break
        except OSError:
            time.sleep(1)
    else:
        raise RuntimeError('current Console did not become healthy')
    if args.agent_only_compute:
        lab.state['agent_only_compute'] = True
        lab.save()
    print('PASS: actual production Console and native PostgreSQL healthy inside Ubuntu 26.04 KVM', flush=True)


if __name__ == '__main__':
    main()
