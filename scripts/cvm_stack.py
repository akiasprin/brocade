#!/usr/bin/env python3
"""Package and install the optional, node-only CVM stack (Python stdlib only).

The archive contains immutable runtime bytes and public configuration templates.
Disks, Cubelet metadata, execution records and administrator network settings stay
outside releases. A failed or interrupted activation restores the previous release.
"""
import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import gzip
import hashlib
import io
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
import threading


CONTRACT = 'brocade-cvm-stack-v2'
STATE_ABI = 'protected-root-v1+cubevs-attachments-v2'
BASE = Path('/opt/brocade/cvm')
TOOLBOX = Path('/usr/local/services/cubetoolbox')
STATE = Path('/data/cubelet')
HOST_BOOT_ID = Path('/proc/sys/kernel/random/boot_id')
UNITS = ('brocade-cvm-cubelet.service',)
AGENT_BIN = Path(os.environ.get('BROCADE_CVM_AGENT_BIN', '/usr/local/bin/brocade-agent'))
MANAGER = Path('/usr/local/lib/brocade/cvm-stack.py')
REQUIRED = (
    'runtime/Cubelet/bin/cubelet',
    'runtime/cube-shim/bin/containerd-shim-cube-rs',
    'runtime/cube-shim/bin/cube-runtime',
    'runtime/cube-kernel-scf/vmlinux',
    'runtime/cube-image/cube-guest-image-cpu.img',
    'runtime/cube-agent/cube-agent.ext4',
    'runtime/cube-shim/version', 'runtime/cube-kernel-scf/version',
    'runtime/cube-image/version', 'runtime/cube-agent/version',
    'config/cubelet.toml', 'config/dynamic.yaml',
    'config/cube.json', 'config/network.json',
    'runtime/bin/cvm-host.py', 'runtime/bin/cvm-stack.py',
)
MAX_FILES = 4096
MAX_TOTAL = 16 * 1024**3
MAX_JSON = 2 * 1024**2
MAX_RELEASES = 16
REPORTER = None


class StackError(RuntimeError):
    pass


class ProgressReporter:
    def __init__(self, stage):
        self.stage = stage
        self.started = time.monotonic()
        self.stop = threading.Event()
        self.worker = threading.Thread(target=self.report_periodically, daemon=True)
        self.worker.start()
        self.report()

    def report(self):
        print(f'[{timestamp()}] CVM {self.stage}: {time.monotonic()-self.started:.1f}s', file=sys.stderr, flush=True)

    def report_periodically(self):
        while not self.stop.wait(30):
            self.report()

    def finish(self, status):
        self.stop.set()
        self.worker.join(timeout=1)
        print(f'[{timestamp()}] CVM {status}, elapsed {time.monotonic()-self.started:.3f}s', file=sys.stderr, flush=True)


def report_stage(label):
    if REPORTER is not None:
        REPORTER.stage = label
        REPORTER.report()


def progress_task(label):
    def decorate(function):
        def wrapped(*args, **kwargs):
            reporter = ProgressReporter(label)
            try:
                value = function(*args, **kwargs)
            except BaseException:
                reporter.finish('failed')
                raise
            reporter.finish('completed')
            return value
        return wrapped
    return decorate


def timestamp():
    return datetime.now(timezone.utc).isoformat(timespec='seconds')


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024**2), b''):
            h.update(block)
    return h.hexdigest()


def safe_name(name):
    p = PurePosixPath(name)
    return bool(name) and not p.is_absolute() and str(p) == name and all(
        part not in ('', '.', '..') and re.fullmatch(r'[A-Za-z0-9_.+-]+', part)
        for part in p.parts)


def read_json(path):
    if path.stat().st_size > MAX_JSON:
        raise StackError('configuration exceeds size limit')
    return json.loads(path.read_bytes())


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def durable_mkdir(path, mode=0o700):
    missing = []
    parent = path
    while not parent.exists():
        missing.append(parent)
        parent = parent.parent
    for directory in reversed(missing):
        directory.mkdir(mode=mode)
        sync_dir(directory.parent)


def atomic(path, data, mode=0o600):
    durable_mkdir(path.parent)
    fd, temporary = tempfile.mkstemp(prefix='.cvm-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            os.fchmod(stream.fileno(), mode)
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        sync_dir(path.parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def atomic_json(path, value):
    atomic(path, (json.dumps(value, sort_keys=True, indent=2) + '\n').encode())


def point(path, target):
    durable_mkdir(path.parent)
    temporary = path.with_name('.' + path.name + '-next')
    temporary.unlink(missing_ok=True)
    temporary.symlink_to(target)
    os.replace(temporary, path)
    sync_dir(path.parent)


def validate_manifest(value):
    if not isinstance(value, dict):
        raise StackError('CVM stack manifest must be a JSON object')
    if value.get('contract') != CONTRACT or value.get('state_abi') != STATE_ABI:
        raise StackError('unsupported CVM stack or persistent-state contract')
    if value.get('architecture') not in ('x86_64', 'aarch64'):
        raise StackError('unsupported CVM architecture')
    backends = value.get('storage_backends')
    if not isinstance(backends, list) or not backends or any(not isinstance(b, str) or b not in ('xfs', 'plain') for b in backends) or len(backends) != len(set(backends)):
        raise StackError('invalid CVM storage backend inventory')
    files = value.get('files', {})
    if not isinstance(files, dict) or not 1 <= len(files) <= MAX_FILES:
        raise StackError('invalid stack file inventory')
    total = 0
    for name, item in files.items():
        if not isinstance(item, dict):
            raise StackError('invalid stack file inventory entry')
        if not safe_name(name) or not name.startswith(('runtime/', 'config/')):
            raise StackError('invalid stack member path')
        if not re.fullmatch(r'[0-9a-f]{64}', item.get('sha256', '')):
            raise StackError('invalid stack file digest')
        size = item.get('bytes')
        if type(size) is not int or size < 0 or size > MAX_TOTAL:
            raise StackError('invalid stack file size')
        if item.get('mode') not in (0o600, 0o644, 0o755):
            raise StackError('invalid stack file permissions')
        total += size
    if total > MAX_TOTAL or any(name not in files for name in REQUIRED):
        raise StackError('incomplete or oversized node runtime')
    if not re.fullmatch(r'[0-9a-f]{64}', value.get('source_sha256', '')):
        raise StackError('stack source identity is missing')
    if value.get('min_glibc') is not None and not re.fullmatch(r'[0-9]+\.[0-9]+', value['min_glibc']):
        raise StackError('invalid glibc compatibility floor')
    return value


def inspect_archive(path, expected, destination=None):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
        raise StackError('CVM bundle must be an owned regular file without group or other write access')
    if not re.fullmatch(r'[0-9a-f]{64}', expected) or digest(path) != expected:
        raise StackError('CVM bundle SHA-256 mismatch')
    with tarfile.open(path, 'r:gz') as archive:
        members = archive.getmembers()
        if len(members) > MAX_FILES + 1:
            raise StackError('too many stack archive members')
        names = [member.name for member in members]
        if len(set(names)) != len(names) or 'manifest.json' not in names:
            raise StackError('duplicate archive member or missing manifest')
        for member in members:
            if not member.isfile() or not safe_name(member.name):
                raise StackError('archive contains a link, device or unsafe path')
        manifest_member = archive.getmember('manifest.json')
        if manifest_member.size > MAX_JSON:
            raise StackError('stack manifest exceeds size limit')
        manifest = validate_manifest(json.load(archive.extractfile(manifest_member)))
        if set(names) != {'manifest.json', *manifest['files']}:
            raise StackError('archive differs from its manifest inventory')
        for member in members:
            if member.name == 'manifest.json':
                continue
            item = manifest['files'][member.name]
            if member.size != item['bytes']:
                raise StackError('archive member size differs from manifest')
            stream = archive.extractfile(member)
            h = hashlib.sha256()
            output = None
            try:
                if destination:
                    target = destination / member.name
                    target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                    output = target.open('xb')
                    os.fchmod(output.fileno(), item['mode'])
                for block in iter(lambda: stream.read(1024**2), b''):
                    h.update(block)
                    if output:
                        output.write(block)
                if output:
                    output.flush()
                    os.fsync(output.fileno())
            finally:
                if output:
                    output.close()
            if h.hexdigest() != item['sha256']:
                raise StackError('stack member SHA-256 mismatch')
        if destination:
            atomic_json(destination / 'manifest.json', manifest)
            for directory, _, _ in os.walk(destination, topdown=False):
                sync_dir(Path(directory))
    return manifest


def run(args, timeout=45):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    if result.returncode:
        # Command output may contain administrator configuration; never echo it.
        raise StackError(f'{Path(args[0]).name} failed (exit {result.returncode})')
    return result.stdout.decode().strip()


def local_storage_backend():
    filesystem = run(['findmnt', '-n', '-o', 'FSTYPE', '-T', str(STATE / 'storage')])
    if filesystem == 'xfs':
        return 'xfs'
    if filesystem == 'ext4':
        return 'plain'
    raise StackError('CVM storage requires persistent XFS/reflink or ext4/plain volumes')


def local_storage_uuid():
    identity = run(['findmnt', '-n', '-o', 'UUID', '-T', str(STATE / 'storage')])
    if not re.fullmatch(r'[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}', identity):
        raise StackError('ext4 CVM storage requires a stable filesystem UUID')
    return identity.lower()


def verify_storage_binding(release, backend):
    path = release / 'generated/storage.json'
    if not path.exists() and backend != 'plain':
        return
    binding = read_json(path)
    if binding.get('backend') != backend or (backend == 'plain' and binding.get('filesystem_uuid') != local_storage_uuid()):
        raise StackError('CVM storage filesystem changed or is not mounted; refusing a replacement pool')


def require_storage_support(manifest, backend):
    backends = manifest.get('storage_backends')
    if not isinstance(backends, list) or backend not in backends:
        raise StackError('this CVM runtime does not support the existing storage backend; keep the current release')


def preflight(manifest, check_virtualization=True):
    if os.geteuid() != 0 or platform.system() != 'Linux':
        raise StackError('CVM installation requires Linux root')
    arch = {'amd64': 'x86_64', 'arm64': 'aarch64'}.get(platform.machine(), platform.machine())
    if manifest['architecture'] != arch:
        raise StackError('CVM bundle does not match the host architecture')
    if not Path('/run/systemd/system').is_dir():
        raise StackError('CVM currently requires a running systemd host')
    for command in ('ip', 'tc', 'ss', 'nft', 'conntrack', 'findmnt', 'nsenter', 'mount', 'systemctl'):
        if not shutil.which(command):
            raise StackError(f'CVM requires {command}')
    if check_virtualization:
        with open('/dev/kvm', 'rb', buffering=0) as device:
            if fcntl.ioctl(device.fileno(), 0xAE00, 0) != 12:
                raise StackError('unsupported KVM API')
    with open('/dev/net/tun', 'r+b', buffering=0):
        pass
    if tuple(map(int, platform.release().split('-')[0].split('.')[:2])) < (5, 15):
        raise StackError('CVM requires Linux 5.15 or later; BPF is checked again at readiness')
    required = manifest.get('min_glibc')
    if required:
        libc, version = platform.libc_ver()
        if libc != 'glibc' or tuple(map(int, version.split('.'))) < tuple(map(int, required.split('.'))):
            raise StackError('host glibc is older than this CVM runtime bundle requires')
    storage = STATE / 'storage'
    storage.mkdir(parents=True, exist_ok=True, mode=0o700)
    backend = local_storage_backend()
    require_storage_support(manifest, backend)
    if backend == 'plain':
        local_storage_uuid()
    for metadata in (STATE, STATE / 'root', STATE / 'state'):
        metadata.mkdir(parents=True, exist_ok=True, mode=0o700)
        if run(['findmnt', '-n', '-o', 'FSTYPE', '-T', str(metadata)]) in ('tmpfs', 'ramfs'):
            raise StackError('Cubelet metadata must survive host reboot')
    # Probe the selected allocation primitive on the actual storage filesystem.
    with tempfile.TemporaryDirectory(prefix='.cvm-storage-', dir=storage) as temporary:
        source, target = Path(temporary) / 'source', Path(temporary) / 'target'
        source.write_bytes(b'CVM reflink preflight\n')
        if backend == 'xfs':
            with source.open('rb') as src, target.open('wb') as dst:
                fcntl.ioctl(dst.fileno(), 0x40049409, src.fileno())
                dst.flush(); os.fsync(dst.fileno())
        else:
            with target.open('wb') as dst:
                os.posix_fallocate(dst.fileno(), 0, 1024**2)
                dst.write(b'CVM plain allocation preflight\n')
                dst.flush(); os.fsync(dst.fileno())
                if os.fstat(dst.fileno()).st_blocks * 512 < 1024**2:
                    raise StackError('ext4 storage did not retain the requested physical allocation')


def persist_boot_modules():
    path = Path('/etc/modules-load.d/brocade-cvm.conf')
    header = '# Managed by Brocade CVM\n'
    if path.is_symlink() or (path.exists() and (path.stat().st_size > 8192 or not path.read_text().startswith(header))):
        raise StackError('existing CVM boot-module configuration is not installer-owned')
    modules = ['tun']
    for module in ('kvm_intel', 'kvm_amd', 'kvm_pvm'):
        if (Path('/sys/module') / module).is_dir():
            modules.append(module)
    atomic(path, (header + '\n'.join(modules) + '\n').encode(), 0o644)


def prepare_runtime_directories():
    for name in ('/data/log/Cubelet', '/data/log/CubeShim', '/data/log/CubeVmm',
                 '/data/cube-shim/disks', '/data/snapshot_pack/disks', '/data/cube-shared/volume'):
        path = Path(name)
        durable_mkdir(path)
        info = path.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise StackError('runtime directory ownership or permissions are unsafe')


def ready(timeout=60, config=None):
    deadline = time.monotonic() + timeout
    config = config or BASE / 'current/generated/cube.json'
    while time.monotonic() < deadline:
        try:
            result = subprocess.run([str(AGENT_BIN), 'cvm-check', '--config', str(config)],
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    timeout=min(35, max(.1, deadline - time.monotonic())))
            if result.returncode == 0 and len(result.stdout) <= MAX_JSON:
                report = json.loads(result.stdout)
                caps = report.get('capabilities', {})
                if report.get('executor') == 'brocade-cube-rust-v1' and caps.get('create') and caps.get('stable_storage'):
                    return
        except (OSError, ValueError, subprocess.TimeoutExpired):
            pass
        time.sleep(min(1, max(0, deadline - time.monotonic())))
    raise StackError('CVM stack readiness was not confirmed by the Rust Agent')


def verify_release(release):
    # Releases are root-owned and private. Bind the verified SHA inventory to
    # inode/ctime metadata so a service restart does not reread multi-GiB Guest
    # images on a 1 GiB host. Any replacement, write or permission change forces
    # full verification; a receipt never substitutes for the initial SHA check.
    directory = release.lstat()
    if not stat.S_ISDIR(directory.st_mode) or directory.st_uid != os.geteuid() or directory.st_mode & 0o022:
        raise StackError('runtime release directory is not private and owned')
    def fingerprint(path, mode=None):
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise StackError('runtime inventory contains an unsafe file')
        if mode is not None and stat.S_IMODE(info.st_mode) != mode:
            raise StackError('runtime file permissions differ from its inventory')
        return [info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns,
                info.st_mode, info.st_uid, info.st_gid, info.st_nlink]
    manifest_path = release / 'manifest.json'
    before_manifest = fingerprint(manifest_path)
    manifest = validate_manifest(read_json(manifest_path))
    manifest_sha = digest(manifest_path)
    if fingerprint(manifest_path) != before_manifest:
        raise StackError('runtime manifest changed during verification')
    files = {name: fingerprint(release / name, item['mode']) for name, item in manifest['files'].items()}
    receipt = release / '.verified-files-v1.json'
    expected = {'contract': 'cvm-verified-files-v1', 'manifest_sha256': manifest_sha, 'files': files}
    if receipt.exists() or receipt.is_symlink():
        fingerprint(receipt, 0o600)
        if read_json(receipt) == expected:
            return manifest
    for name, item in manifest['files'].items():
        path = release / name
        if files[name][2] != item['bytes'] or digest(path) != item['sha256'] or fingerprint(path, item['mode']) != files[name]:
            raise StackError('retained CVM runtime does not match its immutable inventory')
    if fingerprint(manifest_path) != before_manifest:
        raise StackError('runtime manifest changed during verification')
    atomic_json(receipt, expected)
    return manifest


def host_manager(installation, release):
    manifest = verify_release(release)
    name = 'runtime/bin/cvm-host.py'
    spec = importlib.util.spec_from_file_location('brocade_cvm_host', release / name)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.validate(manifest.get('pvm'), manifest['files'])
    if manifest.get('pvm') and manifest['architecture'] != 'x86_64':
        raise StackError('PVM host kernels currently require x86_64')
    return module.Host(sys.modules[__name__], installation, release)


def copy_immutable(source, target):
    if target.exists():
        if target.is_symlink() or digest(source) != digest(target):
            raise StackError('retained system artifact has a conflicting identity')
        return
    durable_mkdir(target.parent)
    fd, temporary = tempfile.mkstemp(prefix='.cvm-', dir=target.parent)
    try:
        with source.open('rb') as src, os.fdopen(fd, 'wb') as dst:
            os.fchmod(dst.fileno(), source.stat().st_mode & 0o777)
            shutil.copyfileobj(src, dst, 1024**2)
            dst.flush()
            os.fsync(dst.fileno())
        os.replace(temporary, target)
        sync_dir(target.parent)
    finally:
        Path(temporary).unlink(missing_ok=True)


def set_toml_values(source, section, values):
    """Replace managed scalar settings without rewriting administrator sections."""
    header = f'[{section}]'
    matches = list(re.finditer(r'(?m)^[ \t]*' + re.escape(header) + r'[ \t]*$', source))
    if len(matches) > 1:
        raise StackError('duplicate managed Cubelet configuration section')
    if matches:
        start = matches[0].end()
        following = re.search(r'(?m)^[ \t]*\[', source[start:])
        end = start + following.start() if following else len(source)
        body = source[start:end]
    else:
        source += '\n' + header
        start = end = len(source)
        body = '\n'
    for key, value in values.items():
        assignment = f'    {key} = {json.dumps(value)}'
        body, count = re.subn(r'(?m)^[ \t]*' + re.escape(key) + r'[ \t]*=[^\n]*$', lambda _: assignment, body)
        if count > 1:
            raise StackError('duplicate managed Cubelet resource setting')
        if not count:
            body += assignment + '\n'
    return source[:start] + body + source[end:]


def apply_resource_defaults(source, reference):
    defaults = tomllib.loads(reference)
    budgets = {
        'cubelog': ('file_num', 'file_size'),
        'plugins."io.cubelet.controller.config.v1.cubelet"': ('node_status_update_frequency',),
        'plugins."io.cubelet.internal.v1.cgroup"': ('pool_size', 'pool_workers'),
        'plugins."io.cubelet.internal.v1.resource-metrics"': ('max_concurrent_requests',),
        'plugins."io.cubelet.internal.v1.network"': ('tap_init_num',),
        'plugins."io.cubelet.cubebox-service.v1.gc-service"': ('cleanup_interval',),
        'plugins."io.cubelet.images-service.v1.images-service".image_gc': ('max_deletion_per_cycle',),
        'plugins."io.cubelet.workflow.v1.workflow".flows.create': ('concurrent',),
        'plugins."io.cubelet.workflow.v1.workflow".flows.destroy': ('concurrent',),
        'plugins."io.containerd.cri.v1.images"': ('max_concurrent_downloads',),
    }
    for section, keys in budgets.items():
        path = re.findall(r'"([^"]+)"|([^.]+)', section)
        table = defaults
        for quoted, plain in path:
            table = table.get(quoted or plain, {})
        values = {key: table[key] for key in keys if key in table}
        if values:
            source = set_toml_values(source, section, values)
    return source


def host_memory_budget():
    mib = 1024**2
    physical = os.sysconf('SC_PHYS_PAGES') * os.sysconf('SC_PAGE_SIZE')
    # Guest memory excludes the Agent, Cubelet, kernel/BPF and per-VM overhead.
    reserve = max(384*mib, physical // 8)
    return max(0, physical - reserve) // (64*mib) * (64*mib)


def render_configuration(release, previous=None, mode='kvm'):
    """Keep immutable payloads separate from host-rendered public configuration."""
    generated = release / 'generated'
    if previous:
        old = read_json(previous / 'generated/cube.json')
        old_native = read_json(previous / 'generated/network.json')
        old_cubelet = (previous / 'generated/cubelet.toml').read_text()
        old_dynamic = (previous / 'generated/dynamic.yaml').read_bytes()
    generated.mkdir(exist_ok=True, mode=0o700)
    uplinks = json.loads(run(['ip', '-j', 'route', 'show', 'default']))
    uplink = next((route.get('dev') for route in uplinks if route.get('dev')), '')
    if not re.fullmatch(r'[A-Za-z0-9_.-]{1,15}', uplink):
        raise StackError('CVM requires an explicit usable IPv4 uplink')
    for source in sorted((release / 'config').rglob('*')):
        if source.is_file():
            data = source.read_bytes().replace(b'@CVM_RELEASE@', str(release).encode()).replace(b'@CVM_UPLINK@', uplink.encode())
            atomic(generated / source.relative_to(release / 'config'), data)
    if mode not in ('kvm', 'pvm'):
        raise StackError('invalid effective virtualization mode')
    if mode == 'pvm':
        atomic(generated / 'cube.json', (generated / 'cube-pvm.json').read_bytes())
    cfg = read_json(generated / 'cube.json')
    cfg['virtualization_mode'] = mode
    native = read_json(generated / 'network.json')
    caps = cfg['capabilities']
    backend = local_storage_backend()
    require_storage_support(read_json(release / 'manifest.json'), backend)
    if previous and old['storage_backend'] != backend:
        raise StackError('existing CVM volumes cannot switch storage backends')
    if previous:
        verify_storage_binding(previous, backend)
    if backend == 'plain':
        atomic_json(generated / 'storage.json', {'backend': backend, 'filesystem_uuid': local_storage_uuid()})
    cfg['storage_backend'] = backend
    if previous:
        # Administrator network sources and capacity are persistent host choices.
        for key, value in old_native.items():
            if key not in ('shim_version', 'shim_sha256'):
                native[key] = value
        for key in caps:
            if key != 'images':
                caps[key] = old['capabilities'][key]
        atomic(generated / 'cubelet.toml', old_cubelet.replace(str(previous), str(release)).encode())
        atomic(generated / 'dynamic.yaml', old_dynamic)
    else:
        mib, gib = 1024**2, 1024**3
        memory = host_memory_budget()
        disk = shutil.disk_usage(STATE / 'storage')
        disk_budget = disk.free // gib * gib
        cpu = os.cpu_count() or 1
        if memory < 128*mib or disk_budget < 3*gib:
            raise StackError('host capacity is below the minimum instance requirements')
        caps.update(allocatable_vcpus=cpu, allocatable_memory_bytes=memory, allocatable_disk_bytes=disk_budget)
        caps['resource_limits'] = dict(min_vcpus=1, max_vcpus=min(cpu, 8), min_memory_bytes=128*mib,
            max_memory_bytes=min(memory, 4*gib), memory_step_bytes=64*mib,
            min_root_disk_bytes=3*gib, max_root_disk_bytes=min(disk_budget, 64*gib), root_disk_step_bytes=gib)
    memory_budget = host_memory_budget()
    if memory_budget < 128*1024**2:
        raise StackError('host has insufficient memory after the runtime reserve')
    caps['allocatable_memory_bytes'] = min(caps['allocatable_memory_bytes'], memory_budget)
    if caps.get('resource_limits'):
        limits = caps['resource_limits']
        limits['max_memory_bytes'] = min(limits['max_memory_bytes'], caps['allocatable_memory_bytes'])
        if limits['min_memory_bytes'] > limits['max_memory_bytes']:
            raise StackError('retained instance limits exceed the host memory budget')
    config_path = generated / 'cubelet.toml'
    atomic(config_path, apply_resource_defaults(config_path.read_text(), (release / 'config/cubelet.toml').read_text()).encode())
    dynamic_path = generated / 'dynamic.yaml'
    dynamic = dynamic_path.read_text()
    for key, value in [('creation_concurrent_num', 1), ('node_status_max_images', 256)]:
        dynamic = re.sub(r'(?m)^(\s*' + key + r'\s*:)\s*[^\n]*$', lambda match: match[1] + ' ' + str(value), dynamic)
    atomic(dynamic_path, dynamic.encode())
    # Recompute manifests after rendering paths; a system image remains independent of capacity.
    for image in caps['images']:
        path = Path(cfg['image_files'][image['id']])
        if not path.is_relative_to(generated):
            raise StackError('packaged system manifest is outside the managed release configuration')
        image['sha256'] = digest(path)
    if previous:
        by_id = {image['id']: image for image in caps['images']}
        for image in old['capabilities']['images'] if old.get('virtualization_mode', 'kvm') == mode else []:
            if image['id'] in by_id and image != by_id[image['id']]:
                raise StackError('a published image identity was changed by the CVM bundle')
            by_id[image['id']] = image
            cfg['image_files'].setdefault(image['id'], old['image_files'][image['id']])
        caps['images'] = [by_id[key] for key in sorted(by_id)]
    # The allocated NIC contract replaces the local bridge catalogue. Its device
    # slots do not depend on how many administrator bridges happened to exist.
    native.pop('bridge_sources', None)
    native['state_dir'] = '/data/cubelet/network-attachments-v2'
    caps['hot_attach'] = caps['hot_detach'] = True
    caps['max_interfaces'] = native['max_interfaces'] = 3
    if backend == 'plain':
        config_path = generated / 'cubelet.toml'
        source = config_path.read_text()
        parsed_storage = tomllib.loads(source)['plugins']['io.cubelet.internal.v1.storage']
        section = '[plugins."io.cubelet.internal.v1.storage"]'
        if parsed_storage.get('local_backend', 'plain') != 'plain':
            raise StackError('retained Cubelet configuration selects a different storage backend')
        if 'local_backend' not in parsed_storage:
            if source.count(section) != 1:
                raise StackError('Cubelet storage configuration is ambiguous')
            source = source.replace(section, section + '\n    local_backend = "plain"')
        if 'plain_reserve_bytes' not in parsed_storage:
            source = source.replace(section, section + '\n    plain_reserve_bytes = 0')
        atomic(config_path, source.encode())
    # Cubelet copies the shared kernel into its image cache. Keep that cache
    # separate by virtualization mode so a previously booted native image can
    # never supply a kernel to a PVM guest (or the reverse).
    image_base = STATE / 'system-images'
    if mode == 'pvm':
        image_base /= 'pvm'
    config_path = generated / 'cubelet.toml'
    source = config_path.read_text()
    section = '[plugins."io.cubelet.internal.v1.images"]'
    if source.count(section) != 1:
        raise StackError('Cubelet image configuration is ambiguous')
    before, content = source.split(section)
    body, separator, after = content.partition('\n[')
    body = '\n'.join(line for line in body.splitlines()
                     if not re.match(r'^\s*(image_base_path|shared_kernel_path)\s*=', line))
    kernel = release / 'runtime' / ('cube-kernel-pvm' if mode == 'pvm' else 'cube-kernel-scf') / 'vmlinux'
    settings = f'\n    image_base_path = {json.dumps(str(image_base))}\n    shared_kernel_path = {json.dumps(str(kernel))}\n'
    atomic(config_path, (before + section + settings + body + separator + after).encode())
    configuration = tomllib.loads(config_path.read_text())
    if configuration.get('root') != str(STATE / 'root') or configuration.get('state') != str(STATE / 'state'):
        raise StackError('Cubelet metadata must use the persistent CVM data paths')
    if configuration['grpc']['address'] != cfg['cubelet_socket'] or configuration['grpc'].get('tcp_address'):
        raise StackError('Cubelet RPC must use the private configured Unix socket')
    if configuration['http']['address'] != '127.0.0.1:8080' or configuration['debug']['address'] != '127.0.0.1:9966':
        raise StackError('Cubelet inspection and debug listeners must stay on loopback')
    shim = release / 'runtime/cube-shim/bin/containerd-shim-cube-rs'
    if not re.fullmatch(r'[A-Za-z0-9_.-]{1,96}', native['shim_version']) or native['shim_sha256'] != digest(shim):
        raise StackError('native network configuration does not pin the packaged Shim')
    for component in ('cube-shim', 'cube-kernel-scf', 'cube-image', 'cube-agent'):
        source = release / 'runtime' / component
        version = (source / 'version').read_text().strip()
        if not re.fullmatch(r'[A-Za-z0-9_.-]{1,96}', version):
            raise StackError('invalid immutable runtime component version')
        for file in source.rglob('*'):
            if file.is_file():
                copy_immutable(file, STATE / 'root/component_versions' / component / version / file.relative_to(source))
    pvm_kernel = release / 'runtime/cube-kernel-pvm'
    if pvm_kernel.is_dir():
        version = (pvm_kernel / 'version').read_text().strip()
        if not re.fullmatch(r'[A-Za-z0-9_.-]{1,96}', version):
            raise StackError('invalid PVM Guest kernel component version')
        copy_immutable(pvm_kernel / 'vmlinux', STATE / 'root/component_versions/cube-kernel-scf' / version / 'vmlinux')
    for file in (release / 'runtime/cubebox_os_image').rglob('*.ext4'):
        copy_immutable(file, image_base / file.relative_to(release / 'runtime/cubebox_os_image'))
    atomic_json(generated / 'cube.json', cfg)
    atomic_json(generated / 'network.json', native)


def prepare_boot_runtime_state():
    """Retain Cube databases, but never recover dead shim transports after boot."""
    boot = HOST_BOOT_ID.read_text().strip()
    if not re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', boot):
        raise StackError('host boot identity is unavailable')
    metadata = STATE / 'state'
    receipt = metadata / '.brocade-runtime-boot.json'
    runtime = metadata / 'io.containerd.runtime.v2.task'
    previous = read_json(receipt).get('host_boot_id') if receipt.exists() else None
    if receipt.exists() and (receipt.is_symlink() or not isinstance(previous, str) or not re.fullmatch(
        r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', previous
    )):
        raise StackError('runtime boot receipt is invalid')
    if runtime.is_symlink() or (runtime.exists() and (
        not runtime.is_dir() or runtime.stat().st_uid != os.geteuid() or runtime.stat().st_mode & 0o022
    )):
        raise StackError('runtime transport directory is not owned and private')
    if previous == boot:
        return
    if previous is None and runtime.exists() and any(runtime.iterdir()):
        raise StackError('existing runtime transports have no managed boot receipt')
    if runtime.exists():
        # The exact containerd task directory contains kernel-lifetime sockets
        # and bundles. Cubelet's volume, network and sandbox databases are its
        # siblings. Never traverse a still-mounted live guest filesystem.
        mounts = run(['findmnt', '-rn', '-o', 'TARGET']).splitlines()
        if any(path == str(runtime) or path.startswith(str(runtime) + '/') for path in mounts):
            raise StackError('runtime transports still contain mounted filesystems')
        shutil.rmtree(runtime)
        sync_dir(metadata)
    atomic_json(receipt, {'host_boot_id': boot})


def start_cubelet(release):
    # Cubelet's existing startup contract leaves explicit non-tmpfs mounts
    # alone. A single persistent bind avoids its automatic private tmpfs and
    # keeps metadata visible across daemon namespaces and host restarts.
    metadata = str(STATE / 'state')
    if run(['findmnt', '-n', '-o', 'FSTYPE', '-T', metadata]) in ('tmpfs', 'ramfs'):
        raise StackError('refusing to replace volatile Cubelet metadata implicitly')
    host = host_manager(Installation(), release)
    config = read_json(release / 'generated/cube.json')
    mode = config['virtualization_mode']
    if host.state().get('pending') and host.mode() != mode:
        raise StackError('CVM virtualization transition has not rendered its matching Guest kernel yet')
    host.check_live(mode)
    verify_storage_binding(release, local_storage_backend())
    if run(['findmnt', '-n', '-o', 'TARGET', '-T', metadata]) != metadata:
        run(['mount', '--bind', metadata, metadata])
    prepare_boot_runtime_state()
    binary = release / 'runtime/Cubelet/bin/cubelet'
    process = subprocess.Popen([str(binary), '--config', str(release / 'generated/cubelet.toml'),
                                '--dynamic-conf-path', str(release / 'generated/dynamic.yaml')], start_new_session=True)
    deadline = time.monotonic() + 40
    while time.monotonic() < deadline:
        try:
            pid = int(Path('/run/cube-let.pid').read_text().strip())
            if pid > 1 and Path(f'/proc/{pid}/exe').resolve() == binary and Path('/run/cubelet/cubelet.sock').is_socket():
                return
        except (OSError, ValueError):
            pass
        # Cubelet intentionally kills its launcher parent after entering its mount namespace.
        if process.poll() not in (None, 0, -9):
            raise StackError('Cubelet launcher failed before daemon readiness')
        time.sleep(.2)
    raise StackError('Cubelet did not publish its actual daemon PID and Unix socket')


class Installation:
    def __init__(self, base=BASE, toolbox=TOOLBOX, units=Path('/etc/systemd/system')):
        self.base, self.toolbox, self.units = base, toolbox, units
        self.state_path = base / 'installation.json'

    @contextmanager
    def locked(self):
        durable_mkdir(self.base)
        if self.base.is_symlink() or self.base.stat().st_mode & 0o022 or self.base.stat().st_uid != os.geteuid():
            raise StackError('CVM installation directory is not private and owned')
        fd = os.open(self.base / 'install.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            yield

    def state(self):
        return read_json(self.state_path) if self.state_path.exists() else {}

    def release(self, identity):
        if not re.fullmatch(r'[0-9a-f]{64}', identity):
            raise StackError('invalid CVM release identity')
        return self.base / 'releases' / identity

    def check_ownership(self, state):
        if self.toolbox.exists() or self.toolbox.is_symlink():
            owned = {self.release(identity) / 'runtime' for identity in (state.get('current'), state.get('pending')) if identity}
            if not self.toolbox.is_symlink() or self.toolbox.readlink() not in owned:
                raise StackError('existing Cube installation is not managed by this CVM installer')
        for name in UNITS:
            path = self.units / name
            if path.exists() and not path.read_text().startswith('# Managed by Brocade CVM\n'):
                raise StackError('existing CVM service unit is not installer-owned')
        if not state.get('current') and not state.get('pending'):
            for directory in (STATE / 'root', STATE / 'state', STATE / 'storage'):
                if directory.exists() and any(item.name != 'lost+found' or not item.is_dir() or any(item.iterdir()) for item in directory.iterdir()):
                    raise StackError('existing Cubelet state has no managed CVM installation receipt')

    def activate(self, identity):
        if run([str(AGENT_BIN), 'cvm-check', '--version']).strip() != 'brocade-cube-rust-v1':
            raise StackError('upgrade Brocade Agent before installing the Rust CVM stack')
        if run([str(AGENT_BIN), 'cvm-check', '--network-contract']).strip() != 'cubevs-attachments-v2':
            raise StackError('upgrade Brocade Agent to the allocated NIC contract before changing services')
        dropin = self.units / 'brocade-agent.service.d/brocade-cvm.conf'
        if dropin.exists() and not dropin.read_text().startswith('# Managed by Brocade CVM\n'):
            raise StackError('existing Agent CVM drop-in is not installer-owned')
        report_stage('verify retained runtime')
        release = self.release(identity)
        verify_release(release)
        if run([str(AGENT_BIN), 'cvm-check', '--maintenance-contract']) != 'brocade-cvm-maintenance-v1':
            raise StackError('upgrade Agent to the CVM maintenance contract before activating this runtime')
        prepare_runtime_directories()
        state = self.state()
        previous = self.release(state['current']) if state.get('current') and state['current'] != identity else None
        if previous is None and (release / 'generated/cube.json').exists():
            previous = release
        host = host_manager(self, release)
        host.check_live(host.mode())
        render_configuration(release, previous, mode=host.mode())
        report_stage('activate services')
        point(self.toolbox, release / 'runtime')
        point(self.base / 'current', release)
        self.units.mkdir(parents=True, exist_ok=True)
        common = '# Managed by Brocade CVM\n'
        cubelet = common + f'''[Unit]
Description=Brocade CVM Cubelet
After=network-online.target
Wants=network-online.target
RequiresMountsFor=/data/cubelet /data/cubelet/storage
[Service]
Type=forking
PIDFile=/run/cube-let.pid
RuntimeDirectory=cubelet
RuntimeDirectoryMode=0700
Delegate=yes
KillMode=process
UMask=0077
Environment=CUBE_NETWORK_ATTACHMENTS_CONFIG={release}/generated/network.json
Environment=LD_LIBRARY_PATH={release}/runtime/lib
Environment=GOMEMLIMIT=128MiB
Environment=PATH={release}/runtime/cube-shim/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
ExecStart=/usr/bin/python3 {MANAGER} start-cubelet --release {identity}
Restart=on-failure
RestartSec=5
TimeoutStopSec=40
TimeoutStartSec=60
LimitMEMLOCK=infinity
LimitNOFILE=65536
[Install]
WantedBy=multi-user.target
'''
        dropin.parent.mkdir(parents=True, exist_ok=True)
        settings = common + f'''[Unit]
After=brocade-cvm-cubelet.service
Wants=brocade-cvm-cubelet.service
[Service]
Environment=BROCADE_CVM_CONFIG={self.base}/current/generated/cube.json
Environment=BROCADE_CVM_STATE_DIR=/var/lib/brocade/cvm-cube
'''
        atomic(dropin, settings.encode(), 0o644)
        atomic(self.units / UNITS[0], cubelet.encode(), 0o644)
        run(['systemctl', 'daemon-reload'])
        run(['systemctl', 'enable', *UNITS])
        run(['systemctl', 'restart', UNITS[0]])
        pid = run(['systemctl', 'show', '--value', '-p', 'MainPID', UNITS[0]])
        binary = release / 'runtime/Cubelet/bin/cubelet'
        if not pid.isdigit() or int(pid) < 2 or digest(Path('/proc') / pid / 'exe') != digest(binary):
            raise StackError('running Cubelet does not match the activated artifact')
        if run(['nsenter', '-t', pid, '-m', 'findmnt', '-n', '-o', 'FSTYPE', '-T', str(STATE / 'state')]) in ('tmpfs', 'ramfs'):
            raise StackError('running Cubelet metadata is not persistent')
        # Fresh enrollment writes and starts the Agent unit after this optional
        # stack step. Existing installations reload the new configuration now.
        if run(['systemctl', 'show', '--value', '-p', 'LoadState', 'brocade-agent.service']) == 'loaded':
            run(['systemctl', 'restart', 'brocade-agent.service'])
        report_stage('verify actual capabilities')
        ready(config=release / 'generated/cube.json')

    def change(self, identity, action, force=False):
        state = self.state()
        if state.get('pending') and not (state['pending'] == identity and (not state.get('current') or force)):
            raise StackError('an interrupted CVM activation requires cvm-stack recover first')
        self.check_ownership(state)
        previous = state.get('current')
        manifest = verify_release(self.release(identity))
        backend = local_storage_backend()
        require_storage_support(manifest, backend)
        if previous:
            old_backend = read_json(self.release(previous) / 'generated/cube.json')['storage_backend']
            if old_backend != backend:
                raise StackError('existing CVM volumes cannot switch storage backends')
            verify_storage_binding(self.release(previous), backend)
        if previous == identity and not force:
            ready()
            return
        if previous:
            old = read_json(self.release(previous) / 'manifest.json')
            if old['state_abi'] != manifest['state_abi']:
                raise StackError('CVM persistent-state ABI does not permit this upgrade or rollback')
        pending = {**state, 'pending': identity, 'action': action, 'started_at': timestamp()}
        atomic_json(self.state_path, pending)
        try:
            self.activate(identity)
        except BaseException:
            if previous and not force:
                self.activate(previous)
                atomic_json(self.state_path, {**state, 'last_failure_at': timestamp()})
            else:
                for unit in reversed(UNITS):
                    if (self.units / unit).exists():
                        run(['systemctl', 'stop', unit])
            # First installation has no previous runtime. Keep its pending receipt
            # and files for explicit recovery; it must not be reported as installed.
            raise
        retained_previous = state.get('previous') if previous == identity else previous
        atomic_json(self.state_path, {'current': identity, 'previous': retained_previous, 'action': action, 'completed_at': timestamp()})

    def install(self, archive, expected, mode=None):
        report_stage('verify bundle')
        manifest = inspect_archive(archive, expected)
        with self.locked():
            state = self.state()
            self.check_ownership(state)
            report_stage('host preflight')
            preflight(manifest, check_virtualization=False)
            target = self.release(expected)
            if not target.exists():
                report_stage('stage immutable release')
                durable_mkdir(target.parent)
                if len(list(target.parent.iterdir())) >= MAX_RELEASES:
                    raise StackError('CVM retained release limit reached; audit image and instance references before removing old releases')
                required = sum(item['bytes'] for item in manifest['files'].values())
                if shutil.disk_usage(target.parent).free < required:
                    raise StackError('insufficient space for a staged CVM release; the active release is unchanged')
                with tempfile.TemporaryDirectory(prefix='.incoming-', dir=target.parent) as temporary:
                    stage = Path(temporary) / 'release'
                    stage.mkdir(mode=0o700)
                    inspect_archive(archive, expected, stage)
                    os.rename(stage, target)
                    sync_dir(target.parent)
            host = host_manager(self, target)
            if host.prepare(mode):
                return True
            persist_boot_modules()
            self.change(expected, 'install', force=bool(host.state().get('pending')))
            host.commit()
            return False

    def set_mode(self, mode, identity=None):
        with self.locked():
            identity = identity or self.state().get('current')
            if not identity:
                raise StackError('install a CVM runtime before selecting its mode')
            release = self.release(identity)
            preflight(verify_release(release), check_virtualization=False)
            host = host_manager(self, release)
            pending = host.state().get('pending')
            if pending and HOST_BOOT_ID.read_text().strip() != pending['boot_id'] and platform.release() != pending['kernel']:
                host.cancel()
                state = self.state()
                if state.get('current'):
                    self.activate(state['current'])
                    state.pop('pending', None)
                    atomic_json(self.state_path, {**state, 'recovered_at': timestamp()})
                raise StackError('requested virtualization kernel did not boot; original mode restored')
            if host.prepare(mode):
                return True
            persist_boot_modules()
            self.change(identity, 'virtualization', force=bool(host.state().get('pending')))
            host.commit()
            return False

    def recover(self):
        with self.locked():
            if (self.base / 'host.json').exists() and read_json(self.base / 'host.json').get('pending'):
                raise StackError('a host mode transition is pending; use resume or cancel-mode before runtime recovery')
            state = self.state()
            if not state.get('pending'):
                raise StackError('there is no interrupted CVM activation')
            if not state.get('current'):
                raise StackError('initial installation failed; inspect its configuration, then retry the same bundle')
            self.activate(state['current'])
            state.pop('pending')
            state['recovered_at'] = timestamp()
            atomic_json(self.state_path, state)


def pack(tree, output, architecture, source_sha256, min_glibc, provenance=None, storage_backends=None, pvm=None):
    files = {}
    total = 0
    for path in sorted(tree.rglob('*')):
        if path.is_symlink() or not (path.is_dir() or path.is_file()):
            raise StackError('node runtime tree must contain only directories and regular files')
        if not path.is_file():
            continue
        name = path.relative_to(tree).as_posix()
        mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
        files[name] = {'sha256': digest(path), 'bytes': path.stat().st_size, 'mode': mode}
        total += path.stat().st_size
        if total > MAX_TOTAL or len(files) > MAX_FILES:
            raise StackError('node runtime tree exceeds package limits')
    if pvm is not None:
        import cvm_host
        cvm_host.validate(pvm, files)
        if architecture != 'x86_64':
            raise StackError('PVM host kernels currently require x86_64')
    manifest = validate_manifest({'contract': CONTRACT, 'state_abi': STATE_ABI, 'architecture': architecture,
                                  'source_sha256': source_sha256, 'min_glibc': min_glibc, 'files': files,
                                  'storage_backends': ['xfs'] if storage_backends is None else storage_backends,
                                  'provenance': provenance})
    if pvm is not None:
        manifest['pvm'] = pvm
    output.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as raw, gzip.GzipFile(fileobj=raw, mode='wb', mtime=0, filename='') as compressed, tarfile.open(fileobj=compressed, mode='w') as archive:
        data = (json.dumps(manifest, sort_keys=True) + '\n').encode()
        member = tarfile.TarInfo('manifest.json')
        member.size, member.mode = len(data), 0o644
        archive.addfile(member, io.BytesIO(data))
        for name, item in files.items():
            member = tarfile.TarInfo(name)
            member.size, member.mode = item['bytes'], item['mode']
            with (tree / name).open('rb') as stream:
                archive.addfile(member, stream)
    print(json.dumps({'bundle': str(output), 'sha256': digest(output), 'architecture': architecture}))


def main():
    global REPORTER
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    build = sub.add_parser('pack')
    build.add_argument('--node-tree', type=Path, required=True)
    build.add_argument('--output', type=Path, required=True)
    build.add_argument('--architecture', choices=('x86_64', 'aarch64'), required=True)
    build.add_argument('--source-sha256', required=True)
    build.add_argument('--min-glibc')
    for command in ('install', 'check'):
        p = sub.add_parser(command)
        p.add_argument('--bundle', type=Path, required=True)
        p.add_argument('--sha256', required=True)
        if command == 'install':
            p.add_argument('--mode', choices=('auto', 'kvm', 'pvm'))
    mode = sub.add_parser('mode')
    mode.add_argument('selection', choices=('auto', 'kvm', 'pvm'))
    resume = sub.add_parser('resume')
    resume.add_argument('--release', required=True)
    sub.add_parser('cancel-mode')
    sub.add_parser('status')
    sub.add_parser('rollback')
    sub.add_parser('recover')
    start = sub.add_parser('start-cubelet')
    start.add_argument('--release', required=True)
    args = parser.parse_args()
    if args.command in ('install', 'check', 'rollback', 'recover', 'pack', 'mode', 'resume'):
        REPORTER = ProgressReporter(args.command)
    install = Installation()
    if args.command == 'pack':
        pack(args.node_tree, args.output, args.architecture, args.source_sha256, args.min_glibc)
    elif args.command == 'check':
        preflight(inspect_archive(args.bundle, args.sha256))
        print('CVM bundle and host preflight passed')
    elif args.command == 'install':
        if install.install(args.bundle, args.sha256, args.mode):
            pending_reboot()
    elif args.command in ('mode', 'resume'):
        if install.set_mode(args.selection if args.command == 'mode' else None,
                            args.release if args.command == 'resume' else None):
            pending_reboot()
    elif args.command == 'cancel-mode':
        with install.locked():
            state = read_json(install.base / 'host.json')
            if not state.get('pending'):
                raise StackError('there is no pending virtualization transition')
            host = host_manager(install, install.release(state['pending']['release']))
            host.cancel()
    elif args.command == 'rollback':
        with install.locked():
            previous = install.state().get('previous')
            if not previous:
                raise StackError('no previous CVM release is retained')
            manifest = verify_release(install.release(previous))
            preflight(manifest)
            if (install.base / 'host.json').exists():
                state = read_json(install.base / 'host.json')
                if state.get('pending') or (state.get('effective') == 'pvm' and not manifest.get('pvm')):
                    raise StackError('finish the host mode transition before rolling back to this runtime')
            target_host = host_manager(install, install.release(previous))
            if target_host:
                target_host.check_live(target_host.mode())
            install.change(previous, 'rollback')
    elif args.command == 'start-cubelet':
        start_cubelet(install.release(args.release))
    elif args.command == 'recover':
        install.recover()
    else:
        state = install.state()
        if (install.base / 'host.json').exists():
            state['virtualization'] = read_json(install.base / 'host.json')
        if state.get('current') and not state.get('pending') and not state.get('virtualization', {}).get('pending'):
            ready(timeout=3)
        print(json.dumps(state, sort_keys=True))


def pending_reboot():
    print('CVM virtualization is prepared; reboot the host manually. Installation will resume automatically after boot.', file=sys.stderr)
    if REPORTER is not None:
        REPORTER.finish('awaiting_reboot')
    sys.exit(75)


if __name__ == '__main__':
    try:
        main()
    except (StackError, OSError, ValueError, subprocess.SubprocessError, tarfile.TarError) as error:
        if REPORTER is not None:
            REPORTER.finish('failed')
        print(f'CVM stack: {error}', file=sys.stderr)
        sys.exit(1)
    else:
        if REPORTER is not None:
            REPORTER.finish('completed')
