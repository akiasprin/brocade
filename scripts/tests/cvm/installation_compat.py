"""Installation-only probes, executed as root inside an owned disposable VM.

No inner Guest is created. The exact candidate installer and Agent are used;
OS requirements and failures are reported without replacing system libraries.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import time

ROOT = Path('/var/lib/cvm-install-test')
EVIDENCE = ROOT / 'compatibility.json'
MANAGER = '/usr/local/lib/brocade/cvm-stack.py'
AGENT = '/usr/local/bin/brocade-agent'
CANARY = Path('/var/lib/cvm-install-canary')


def command(args, timeout=60):
    result = subprocess.run(args, capture_output=True, timeout=timeout)
    return result.returncode, result.stdout.decode(errors='replace'), result.stderr.decode(errors='replace')


def checked(args, timeout=60):
    code, output, error = command(args, timeout)
    if code:
        raise RuntimeError(str(args[0]) + ' failed: ' + (output + error)[-2000:])
    return output.strip()


def runtime_cli_check():
    release = Path('/opt/brocade/cvm/current/runtime')
    env = {**os.environ, 'LD_LIBRARY_PATH': str(release / 'lib')}
    versions = {}
    for name in ('containerd-shim-cube-rs', 'cube-runtime'):
        result = subprocess.run([str(release / 'cube-shim/bin' / name), '--version'],
                                env=env, capture_output=True, text=True, timeout=15)
        if result.returncode or name not in result.stdout:
            raise RuntimeError(f'{name} loader/version check failed: '
                               + (result.stdout + result.stderr)[-2000:])
        versions[name] = result.stdout.strip()
    return versions


def write(path, value, mode=0o600):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value)
    path.chmod(mode)


def facts():
    release = {}
    for line in Path('/etc/os-release').read_text().splitlines():
        if '=' in line:
            key, value = line.split('=', 1)
            release[key] = value.strip('"')
    return {'os': release.get('PRETTY_NAME'), 'os_id': release.get('ID'), 'version_id': release.get('VERSION_ID'),
            'kernel': platform.release(), 'architecture': platform.machine(), 'python': platform.python_version(),
            'libc': platform.libc_ver(), 'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip()}


def save(result):
    write(EVIDENCE, json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    write('/lab/results/compatibility.json', json.dumps(result, ensure_ascii=False, indent=2) + '\n', 0o644)


def canary_setup():
    write(CANARY / 'sentinel', 'preserve existing service and application data\n')
    write('/etc/systemd/system/cvm-install-canary.service', '''[Unit]
Description=Owned CVM compatibility HTTP canary
After=network.target
[Service]
ExecStart=/usr/bin/python3 -m http.server 18080 --bind 127.0.0.1 --directory /var/lib/cvm-install-canary
Restart=on-failure
[Install]
WantedBy=multi-user.target
''', 0o644)
    checked(['systemctl', 'daemon-reload'])
    checked(['systemctl', 'enable', '--now', 'cvm-install-canary.service'])


def canary_check():
    import urllib.request
    checked(['systemctl', 'is-active', '--quiet', 'cvm-install-canary.service'])
    for _ in range(30):
        try:
            content = urllib.request.urlopen('http://127.0.0.1:18080/sentinel', timeout=2).read()
            assert content == (CANARY / 'sentinel').read_bytes()
            return hashlib.sha256(content).hexdigest()
        except OSError:
            time.sleep(.1)
    raise RuntimeError('pre-existing HTTP canary is unavailable')


def prepare(expected, filesystem):
    ROOT.mkdir(parents=True, exist_ok=True)
    result = {'facts': facts(), 'storage_filesystem': filesystem, 'bundle_sha256': expected, 'installation': 'not_attempted', 'checks': []}
    try:
        Path('/usr/local/lib/brocade').mkdir(parents=True, exist_ok=True)
        shutil.copy2('/lab/inputs/installation_compat.py', '/usr/local/lib/brocade/cvm-install-compat.py')
        shutil.copy2('/lab/inputs/cvm_stack.py', MANAGER)
        shutil.copy2('/lab/inputs/agent-binary', AGENT)
        Path(AGENT).chmod(0o755)
        shutil.copy2('/lab/inputs/bundle.tar.gz', '/root/cvm-install.tar.gz')
        Path('/root/cvm-install.tar.gz').chmod(0o600)
        canary_setup()
        result['canary_sha256'] = canary_check()
        assert checked([AGENT, 'cvm-check', '--version']) == 'brocade-cube-rust-v1'
        result['checks'].append('matching static Agent executes on this OS')
        if sys.version_info >= (3, 11):
            checked(['apt-get', 'update'], 300)
            checked(['apt-get', 'install', '-y', '--no-install-recommends', 'python3', 'iproute2', 'nftables', 'util-linux', 'xfsprogs', 'kmod', 'ca-certificates'], 300)
            if command(['modprobe', 'kvm_intel'])[0] != 0:
                checked(['modprobe', 'kvm_amd'])
            checked(['modprobe', 'tun'])
            Path('/data/cubelet/storage').mkdir(parents=True, exist_ok=True)
            checked(['truncate', '-s', '16G', str(ROOT / 'storage.img')])
            formatter = ['mkfs.ext4', '-F', '-E', 'nodiscard'] if filesystem == 'ext4' else ['mkfs.xfs', '-m', 'reflink=1', '-f']
            checked(formatter + [str(ROOT / 'storage.img')])
            with Path('/etc/fstab').open('a') as stream:
                stream.write(str(ROOT / 'storage.img') + ' /data/cubelet/storage ' + filesystem + ' loop 0 0\n')
            checked(['mount', '/data/cubelet/storage'])
            result['storage_uuid'] = checked(['findmnt', '-n', '-o', 'UUID', '-T', '/data/cubelet/storage'])
            # An existing unrelated interface, nftables table and HTTP service
            # must survive installation and reboot without being adopted.
            write(CANARY / 'network.py', '''import pathlib,subprocess
def run(*args):subprocess.run(args,check=True)
run('ip','link','add','cvm-canary0','type','dummy')
run('ip','link','set','dev','cvm-canary0','alias','owned-install-canary')
run('ip','address','add','198.18.250.1/32','dev','cvm-canary0')
run('ip','link','set','dev','cvm-canary0','up')
run('nft','-f','/var/lib/cvm-install-canary/rules.nft')
''')
            write(CANARY / 'rules.nft', '''table inet cvm_install_canary {
    chain input {
        type filter hook input priority 50;
        policy accept;
    }
}
''')
            write('/etc/systemd/system/cvm-install-canary-network.service', '''[Unit]
Description=Owned CVM compatibility network canary
After=nftables.service
Before=brocade-cvm-cubelet.service
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/bin/python3 /var/lib/cvm-install-canary/network.py
[Install]
WantedBy=multi-user.target
''', 0o644)
            checked(['systemctl', 'daemon-reload'])
            checked(['systemctl', 'start', 'nftables.service'])
            checked(['systemctl', 'enable', '--now', 'cvm-install-canary-network.service'])
            result['network_before'] = network_check()
            result['route_before'] = json.loads(checked(['ip', '-j', 'route', 'show', 'default']))
        install = ['python3', MANAGER, 'install', '--bundle', '/root/cvm-install.tar.gz', '--sha256', expected]
        code, output, error = command(install, 180)
        write(ROOT / 'installer.log', output + error)
        result['returncode'] = code
        result['reason'] = (output + error)[-4000:]
        if code:
            result['installation'] = 'rejected'
            result['existing_service_preserved'] = canary_check() == result['canary_sha256']
            result['installed_receipt_exists'] = Path('/opt/brocade/cvm/installation.json').exists()
            save(result)
            return
        result['installation'] = 'installed'
        result['checks'].append('exact candidate bundle installs without replacing OS libraries')
        result['runtime_versions'] = runtime_cli_check()
        result['checks'].append('GNU Shim and runtime load packaged libraries and execute successfully')
        result['capabilities'] = json.loads(checked([AGENT, 'cvm-check', '--config', '/opt/brocade/cvm/current/generated/cube.json'], 40))
        assert result['capabilities']['capabilities']['create'] and result['capabilities']['capabilities']['stable_storage']
        assert result['capabilities']['capabilities']['guest_terminal']
        result['current'] = json.loads(checked(['python3', MANAGER, 'status'], 15))['current']
        assert result['current'] == expected
        result['cubelet_pid'] = checked(['systemctl', 'show', '--value', '-p', 'MainPID', 'brocade-cvm-cubelet.service'])
        config = Path('/opt/brocade/cvm/current/generated/cube.json')
        result['config_sha256'] = hashlib.sha256(config.read_bytes()).hexdigest()
        checked(install, 180)
        assert checked(['systemctl', 'show', '--value', '-p', 'MainPID', 'brocade-cvm-cubelet.service']) == result['cubelet_pid']
        assert hashlib.sha256(config.read_bytes()).hexdigest() == result['config_sha256']
        result['checks'].append('same-bundle reinstall preserves running Cubelet PID and configuration')
        result['existing_service_preserved'] = canary_check() == result['canary_sha256']
        assert network_check() == result['network_before']
        assert json.loads(checked(['ip', '-j', 'route', 'show', 'default'])) == result['route_before']
        result['checks'].append('existing HTTP service, nft table, dummy interface and default route preserved')
    except Exception as error:
        result['installation'] = 'setup_failed' if result['installation'] == 'not_attempted' else 'failed'
        result['error'] = str(error)[-4000:]
        result['setup_journal'] = command(['journalctl', '--no-pager', '-n', '35', '-u', 'cvm-install-canary-network.service'])[1][-6000:]
    save(result)


def network_check():
    assert Path('/sys/class/net/cvm-canary0/ifalias').read_text().strip() == 'owned-install-canary'
    state = json.loads(checked(['nft', '-j', 'list', 'table', 'inet', 'cvm_install_canary']))
    return [item for item in state['nftables'] if 'metainfo' not in item]


def verify():
    before = json.loads(EVIDENCE.read_text())
    assert facts()['boot_id'] != before['facts']['boot_id']
    config = Path('/opt/brocade/cvm/current/generated/cube.json')
    deadline = time.monotonic() + 60
    while True:
        # systemctl is-active with several units succeeds when any is active.
        services = ('brocade-cvm-cubelet.service', 'cvm-install-canary.service', 'cvm-install-canary-network.service')
        if all(command(['systemctl', 'is-active', '--quiet', unit], 10)[0] == 0 for unit in services):
            code, output, error = command([AGENT, 'cvm-check', '--config', str(config)], 10)
            if code == 0:
                caps = json.loads(output)
                break
        if time.monotonic() > deadline:
            journal = command(['journalctl', '--no-pager', '-n', '50', '-u', 'brocade-cvm-cubelet.service',
                               '-u', 'cvm-install-canary.service', '-u', 'cvm-install-canary-network.service'])[1]
            raise RuntimeError('CVM/canary services did not become ready after OS reboot: ' + journal[-4000:])
        time.sleep(1)
    assert json.loads(checked(['python3', MANAGER, 'status']))['current'] == before['bundle_sha256']
    assert checked(['findmnt', '-n', '-o', 'UUID', '-T', '/data/cubelet/storage']) == before['storage_uuid']
    assert canary_check() == before['canary_sha256']
    # nft handles are allocated by each kernel boot. Within one boot installation
    # must preserve them; after reboot compare the complete rule content instead.
    def content(value):
        if isinstance(value, dict):
            return {key: content(item) for key, item in value.items() if key != 'handle'}
        if isinstance(value, list):
            return [content(item) for item in value]
        return value
    assert content(network_check()) == content(before['network_before'])
    assert json.loads(checked(['ip', '-j', 'route', 'show', 'default'])) == before['route_before']
    assert hashlib.sha256(config.read_bytes()).hexdigest() == before['config_sha256']
    assert caps == before['capabilities']
    assert runtime_cli_check() == before['runtime_versions']
    print(json.dumps({'facts': facts(), 'reboot_recovery': True, 'storage_uuid_preserved': True,
                      'configuration_preserved': True, 'existing_service_and_network_preserved': True,
                      'capabilities_preserved': True, 'runtime_loader_verified': True}))


if __name__ == '__main__':
    if sys.argv[1] == 'prepare': prepare(sys.argv[2], sys.argv[3])
    elif sys.argv[1] == 'verify': verify()
    else: raise SystemExit('unknown installation compatibility action')
